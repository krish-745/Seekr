//! Clipboard history: text (with its HTML and RTF forms), images and files, kept in
//! `clipboard.db` for 30 days or 1,000 entries, pinned ones for good. Type `clip ` to list them.
//! Copies a password manager marks as private, the way Windows' own history skips them, are never
//! read.

use std::{
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, params};
use tauri::{AppHandle, Manager};
use windows::{
    Win32::{
        Foundation::{HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, WPARAM},
        System::{
            DataExchange::{
                AddClipboardFormatListener, CloseClipboard, GetClipboardData, GetClipboardOwner,
                IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW,
            },
            LibraryLoader::GetModuleHandleW,
            Memory::{GlobalLock, GlobalSize, GlobalUnlock},
            Ole::{CF_DIB, CF_HDROP, CF_UNICODETEXT},
            Threading::GetCurrentProcessId,
        },
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW,
            GetWindowThreadProcessId, HWND_MESSAGE, MSG, RegisterClassW, WINDOW_EX_STYLE,
            WINDOW_STYLE, WM_CLIPBOARDUPDATE, WNDCLASSW,
        },
    },
    core::{HSTRING, PCWSTR, w},
};

use crate::{
    clipboard, paste,
    query::{Action, Item},
};

const KEEP_DAYS: i64 = 30;
const KEEP_ENTRIES: i64 = 1_000;
const MAX_TEXT_BYTES: usize = 4 << 20;
const MAX_PIXELS: u64 = 40_000_000;
const SHOWN: usize = 50;
const PREVIEW_CHARS: usize = 20_000;

const PASTE: Action = Action {
    id: "paste",
    label: "Paste",
};
const COPY: Action = Action {
    id: "copy",
    label: "Copy",
};
const PLAIN: Action = Action {
    id: "plain",
    label: "Paste as plain text",
};
const PIN: Action = Action {
    id: "clippin",
    label: "Pin",
};
const UNPIN: Action = Action {
    id: "clipunpin",
    label: "Unpin",
};
const DELETE: Action = Action {
    id: "clipdelete",
    label: "Delete from history",
};

pub struct Clips {
    db: Mutex<Connection>,
}

/// What a copy left on the clipboard.
#[derive(Debug, PartialEq)]
enum Captured {
    Text {
        text: String,
        html: Option<Vec<u8>>,
        rtf: Option<Vec<u8>>,
    },
    Files(Vec<String>),
    Image {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
}

fn images_dir() -> PathBuf {
    common::data_dir().join("clips")
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

impl Clips {
    fn open(path: &std::path::Path) -> rusqlite::Result<Self> {
        Self::with(Connection::open(path)?)
    }

    fn with(db: Connection) -> rusqlite::Result<Self> {
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS clips (
                id INTEGER PRIMARY KEY,
                hash INTEGER NOT NULL UNIQUE,
                kind TEXT NOT NULL,
                text TEXT NOT NULL,
                html BLOB,
                rtf BLOB,
                image TEXT,
                bytes INTEGER NOT NULL,
                used INTEGER NOT NULL,
                pinned INTEGER NOT NULL DEFAULT 0
            );",
        )?;
        Ok(Self { db: Mutex::new(db) })
    }

    /// Stores a copy (or moves an identical one to the top) and drops what is past keeping.
    fn add(&self, captured: Captured, at: i64) -> rusqlite::Result<()> {
        let hash = fingerprint(&captured);
        let (kind, text, html, rtf, image, bytes) = match captured {
            Captured::Text { text, html, rtf } => {
                let bytes = text.len();
                ("text", text, html, rtf, None, bytes)
            }
            Captured::Files(files) => {
                let bytes = files.len();
                ("files", files.join("\n"), None, None, None, bytes)
            }
            Captured::Image {
                width,
                height,
                rgba,
            } => {
                let path = images_dir().join(format!("{hash:016x}.png"));
                if !path.exists() {
                    std::fs::create_dir_all(images_dir())
                        .and_then(|()| std::fs::write(&path, encode_png(width, height, &rgba)?))
                        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
                }
                let text = format!("Image {width} × {height}");
                let path = path.display().to_string();
                ("image", text, None, None, Some(path), rgba.len())
            }
        };
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT INTO clips (hash, kind, text, html, rtf, image, bytes, used)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(hash) DO UPDATE SET used = excluded.used,
                html = excluded.html, rtf = excluded.rtf",
            params![hash as i64, kind, text, html, rtf, image, bytes as i64, at],
        )?;
        prune(&db, at)
    }

    /// Newest first, pinned ones on top, keeping those whose text has every word of `filter`.
    fn items(&self, filter: &str, at: i64) -> rusqlite::Result<Vec<Item>> {
        let words: Vec<String> = filter.split_whitespace().map(str::to_lowercase).collect();
        let db = self.db.lock().unwrap();
        let mut statement = db.prepare(
            "SELECT id, kind, text, html IS NOT NULL OR rtf IS NOT NULL, image, bytes, used, pinned
             FROM clips ORDER BY pinned DESC, used DESC",
        )?;
        let rows = statement.query_map([], |r| {
            Ok(Row {
                id: r.get(0)?,
                kind: r.get(1)?,
                text: r.get(2)?,
                rich: r.get(3)?,
                image: r.get(4)?,
                bytes: r.get(5)?,
                used: r.get(6)?,
                pinned: r.get(7)?,
            })
        })?;
        let mut items = Vec::new();
        for row in rows {
            let row = row?;
            let lower = row.text.to_lowercase();
            if words.iter().all(|w| lower.contains(w.as_str())) {
                items.push(row.item(at));
                if items.len() == SHOWN {
                    break;
                }
            }
        }
        Ok(items)
    }

    fn run(&self, app: &AppHandle, id: i64, action: &str) -> Result<(), String> {
        let db = self.db.lock().unwrap();
        let sql = |e: rusqlite::Error| e.to_string();
        match action {
            "clippin" | "clipunpin" => {
                let pinned = action == "clippin";
                db.execute(
                    "UPDATE clips SET pinned = ?1 WHERE id = ?2",
                    params![pinned, id],
                )
                .map_err(sql)?;
                return Ok(());
            }
            "clipdelete" => {
                let image: Option<Option<String>> = db
                    .query_row("SELECT image FROM clips WHERE id = ?1", [id], |r| r.get(0))
                    .optional()
                    .map_err(sql)?;
                db.execute("DELETE FROM clips WHERE id = ?1", [id])
                    .map_err(sql)?;
                if let Some(Some(path)) = image {
                    let _ = std::fs::remove_file(path);
                }
                return Ok(());
            }
            _ => {}
        }
        let (kind, text, html, rtf, image): Stored = db
            .query_row(
                "SELECT kind, text, html, rtf, image FROM clips WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .map_err(sql)?;
        db.execute(
            "UPDATE clips SET used = ?1 WHERE id = ?2",
            params![now(), id],
        )
        .map_err(sql)?;
        drop(db);
        let plain = action == "plain";
        let mut formats = Vec::new();
        match kind.as_str() {
            "files" => {
                let paths: Vec<&str> = text.lines().collect();
                formats.push((u32::from(CF_HDROP.0), clipboard::files_bytes(&paths)));
            }
            "image" => {
                let png =
                    std::fs::read(image.ok_or("image missing")?).map_err(|e| e.to_string())?;
                let (width, height, rgba) = decode_png(&png).ok_or("image unreadable")?;
                formats.push((u32::from(CF_DIB.0), rgba_to_dib(width, height, &rgba)));
                formats.push((format("PNG"), png));
            }
            _ => {
                formats.push((u32::from(CF_UNICODETEXT.0), clipboard::text_bytes(&text)));
                if !plain {
                    formats.extend(html.map(|h| (format("HTML Format"), h)));
                    formats.extend(rtf.map(|r| (format("Rich Text Format"), r)));
                }
            }
        }
        let owner = crate::panel_hwnd(app).ok_or("panel window is gone")?;
        clipboard::set(owner, &formats)?;
        if action == "copy" {
            crate::hide_panel(app, true);
        } else {
            paste::into_previous(app);
        }
        Ok(())
    }
}

/// kind, text, HTML, RTF and image path of one entry.
type Stored = (
    String,
    String,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<String>,
);

struct Row {
    id: i64,
    kind: String,
    text: String,
    rich: bool,
    image: Option<String>,
    bytes: i64,
    used: i64,
    pinned: bool,
}

impl Row {
    fn item(self, at: i64) -> Item {
        let ago = ago(at - self.used);
        let (title, what) = match self.kind.as_str() {
            "image" => (self.text.clone(), "Image".to_string()),
            "files" => {
                let names: Vec<&str> = self
                    .text
                    .lines()
                    .map(|p| p.rsplit('\\').next().unwrap_or(p))
                    .collect();
                let title = match names.len() {
                    1 => names[0].to_string(),
                    n => format!("{n} files: {}", names.join(", ")),
                };
                (title, "Files".to_string())
            }
            _ => {
                let line = self
                    .text
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .unwrap_or("");
                let title: String = line.chars().take(120).collect();
                (title, format!("Text · {}", size(self.bytes)))
            }
        };
        let mut subtitle = format!("{what} · {ago}");
        if self.pinned {
            subtitle = format!("Pinned · {subtitle}");
        }
        let mut actions = vec![PASTE, COPY];
        if self.rich {
            actions.push(PLAIN);
        }
        actions.push(if self.pinned { UNPIN } else { PIN });
        actions.push(DELETE);
        Item {
            id: format!("clip:{}", self.id),
            title,
            subtitle,
            preview: if self.kind == "image" {
                String::new()
            } else {
                self.text.chars().take(PREVIEW_CHARS).collect()
            },
            // An image shows itself as the row's icon and, larger, in the preview pane.
            icon: self.image.as_deref().map(crate::preview::thumb_url),
            line: None,
            actions,
        }
    }
}

fn prune(db: &Connection, at: i64) -> rusqlite::Result<()> {
    let cutoff = at - KEEP_DAYS * 24 * 60 * 60;
    let mut statement = db.prepare(
        "SELECT id, image FROM clips WHERE pinned = 0 AND (used < ?1 OR id NOT IN
            (SELECT id FROM clips WHERE pinned = 0 ORDER BY used DESC LIMIT ?2))",
    )?;
    let old: Vec<(i64, Option<String>)> = statement
        .query_map(params![cutoff, KEEP_ENTRIES], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    for (id, image) in old {
        db.execute("DELETE FROM clips WHERE id = ?1", [id])?;
        if let Some(path) = image {
            let _ = std::fs::remove_file(path);
        }
    }
    Ok(())
}

fn ago(seconds: i64) -> String {
    match seconds {
        ..60 => "just now".into(),
        60..3600 => format!("{} min ago", seconds / 60),
        3600..86_400 => format!("{} h ago", seconds / 3600),
        86_400..172_800 => "yesterday".into(),
        _ => format!("{} days ago", seconds / 86_400),
    }
}

fn size(bytes: i64) -> String {
    match bytes {
        ..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{:.1} KB", bytes as f64 / 1024.0),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
    }
}

/// FNV-1a over the kind and content: stable across builds, so a copy of the same thing is found
/// again.
fn fingerprint(captured: &Captured) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
    };
    match captured {
        Captured::Text { text, .. } => {
            feed(b"t");
            feed(text.as_bytes());
        }
        Captured::Files(files) => {
            feed(b"f");
            files.iter().for_each(|f| feed(f.as_bytes()));
        }
        Captured::Image {
            width,
            height,
            rgba,
        } => {
            feed(b"i");
            feed(&width.to_le_bytes());
            feed(&height.to_le_bytes());
            feed(rgba);
        }
    }
    hash >> 1 // fits SQLite's signed integer
}

/// `clip` or `clip <words>`: the clipboard history, filtered by the words.
pub fn mode(text: &str) -> Option<&str> {
    let rest = text.trim_start().strip_prefix("clip")?;
    match rest.chars().next() {
        Some(' ') => Some(rest.trim()),
        _ => None,
    }
}

pub fn items(app: &AppHandle, filter: &str) -> Vec<Item> {
    let Some(clips) = app.try_state::<Clips>() else {
        return Vec::new();
    };
    clips.items(filter, now()).unwrap_or_else(|e| {
        tracing::error!("clipboard history unreadable: {e}");
        Vec::new()
    })
}

pub fn run(app: &AppHandle, id: &str, action: &str) -> Result<(), String> {
    let id: i64 = id.parse().map_err(|_| "bad clipboard entry")?;
    let clips = app.try_state::<Clips>().ok_or("clipboard history is off")?;
    clips.run(app, id, action)
}

// ---- watching the clipboard ----

static APP: OnceLock<AppHandle> = OnceLock::new();

/// Opens the history and starts listening for copies on a thread of its own.
pub fn start(app: &AppHandle) {
    let path = common::data_dir().join("clipboard.db");
    match Clips::open(&path) {
        Ok(clips) => {
            app.manage(clips);
        }
        Err(e) => {
            tracing::error!("{} unusable ({e}); no clipboard history", path.display());
            return;
        }
    }
    let _ = APP.set(app.clone());
    std::thread::spawn(|| unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(on_message),
            hInstance: instance.into(),
            lpszClassName: w!("SeekrClipboard"),
            ..Default::default()
        });
        let window = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("SeekrClipboard"),
            PCWSTR::null(),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance.into()),
            None,
        );
        let Ok(window) = window else {
            tracing::error!("no clipboard listener window");
            return;
        };
        if let Err(e) = AddClipboardFormatListener(window) {
            tracing::error!("can't listen to the clipboard: {e}");
            return;
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            DispatchMessageW(&msg);
        }
    });
}

unsafe extern "system" fn on_message(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_CLIPBOARDUPDATE {
        if let Some(captured) = unsafe { read(window) }
            && let Some(clips) = APP.get().and_then(|a| a.try_state::<Clips>())
            && let Err(e) = clips.add(captured, now())
        {
            tracing::error!("clipboard entry not saved: {e}");
        }
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

fn format(name: &str) -> u32 {
    unsafe { RegisterClipboardFormatW(&HSTRING::from(name)) }
}

/// Reads what was just copied, unless Seekr wrote it (putting an entry back) or the copying app
/// asked for it to stay out of clipboard history.
unsafe fn read(window: HWND) -> Option<Captured> {
    unsafe {
        let owner = GetClipboardOwner().ok()?;
        let mut pid = 0;
        GetWindowThreadProcessId(owner, Some(&mut pid));
        if pid == GetCurrentProcessId() {
            return None;
        }
        let mut opened = OpenClipboard(Some(window));
        for _ in 0..10 {
            if opened.is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
            opened = OpenClipboard(Some(window));
        }
        opened.ok()?;
        let captured = if private() {
            None
        } else if IsClipboardFormatAvailable(u32::from(CF_HDROP.0)).is_ok() {
            data(u32::from(CF_HDROP.0))
                .map(|b| files_from(&b))
                .filter(|f| !f.is_empty())
                .map(Captured::Files)
        } else if IsClipboardFormatAvailable(u32::from(CF_UNICODETEXT.0)).is_ok() {
            data(u32::from(CF_UNICODETEXT.0))
                .filter(|b| b.len() <= MAX_TEXT_BYTES)
                .map(|b| text_from(&b))
                .filter(|t| !t.trim().is_empty())
                .map(|text| Captured::Text {
                    text,
                    html: data(format("HTML Format")),
                    rtf: data(format("Rich Text Format")),
                })
        } else if IsClipboardFormatAvailable(u32::from(CF_DIB.0)).is_ok() {
            data(u32::from(CF_DIB.0))
                .and_then(|b| dib_to_rgba(&b))
                .map(|(width, height, rgba)| Captured::Image {
                    width,
                    height,
                    rgba,
                })
        } else {
            None
        };
        let _ = CloseClipboard();
        captured
    }
}

/// The markers password managers and other apps put on a copy that history should skip.
unsafe fn private() -> bool {
    unsafe {
        let present = |name: &str| IsClipboardFormatAvailable(format(name)).is_ok();
        if present("ExcludeClipboardContentFromMonitorProcessing")
            || present("Clipboard Viewer Ignore")
        {
            return true;
        }
        // A DWORD 0 here means "don't keep this in clipboard history".
        data(format("CanIncludeInClipboardHistory"))
            .is_some_and(|b| b.get(..4) == Some(&[0, 0, 0, 0]))
    }
}

/// The bytes of a clipboard format, while the clipboard is open.
unsafe fn data(format: u32) -> Option<Vec<u8>> {
    unsafe {
        let handle: HANDLE = GetClipboardData(format).ok()?;
        let global = HGLOBAL(handle.0);
        let size = GlobalSize(global);
        let ptr = GlobalLock(global).cast::<u8>();
        if ptr.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(ptr, size).to_vec();
        let _ = GlobalUnlock(global);
        Some(bytes)
    }
}

/// UTF-16 up to its terminating null.
fn text_from(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&c| u16::from_le_bytes(c))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// A DROPFILES block: the paths start at its first field, null-separated, wide when its fifth
/// field says so.
fn files_from(bytes: &[u8]) -> Vec<String> {
    let field = |i: usize| {
        bytes
            .get(i..i + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let (Some(start), Some(wide)) = (field(0), field(16)) else {
        return Vec::new();
    };
    let Some(rest) = bytes.get(start as usize..) else {
        return Vec::new();
    };
    let joined = if wide != 0 {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_le_bytes(c))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(rest).into_owned()
    };
    joined
        .split('\0')
        .take_while(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

/// A 24- or 32-bit device-independent bitmap as RGBA rows from the top.
fn dib_to_rgba(dib: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let u32_at = |i: usize| {
        dib.get(i..i + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let header = u32_at(0)? as usize;
    let width = u32_at(4)? as i32;
    let height = u32_at(8)? as i32;
    let bits = u16::from_le_bytes([*dib.get(14)?, *dib.get(15)?]);
    let compression = u32_at(16)?;
    let colors = u32_at(32)? as usize;
    // BI_RGB, or BI_BITFIELDS with the usual masks (three of them after a plain 40-byte header).
    let offset = match compression {
        0 => header + colors * 4,
        3 if header == 40 => header + 12,
        3 => header,
        _ => return None,
    };
    if compression == 3 {
        let masks = (u32_at(40)?, u32_at(44)?, u32_at(48)?);
        if masks != (0x00FF_0000, 0x0000_FF00, 0x0000_00FF) {
            return None;
        }
    }
    if width <= 0 || height == 0 || !(bits == 24 || bits == 32) {
        return None;
    }
    let (w, h) = (width as usize, height.unsigned_abs() as usize);
    if (w as u64) * (h as u64) > MAX_PIXELS {
        return None;
    }
    let step = usize::from(bits / 8);
    let stride = (w * usize::from(bits)).div_ceil(32) * 4;
    let pixels = dib.get(offset..offset + stride * h)?;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        // Rows are stored from the bottom unless the height is negative.
        let row = if height > 0 { h - 1 - y } else { y };
        let line = &pixels[row * stride..row * stride + w * step];
        for p in line.chunks_exact(step) {
            let alpha = if step == 4 { p[3] } else { 255 };
            rgba.extend([p[2], p[1], p[0], alpha]);
        }
    }
    // Most 32-bit copies (screenshots among them) leave alpha at zero, meaning opaque.
    if step == 4 && rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
        rgba.as_chunks_mut::<4>()
            .0
            .iter_mut()
            .for_each(|p| p[3] = 255);
    }
    Some((w as u32, h as u32, rgba))
}

/// A 32-bit bottom-up DIB, which every app that pastes images reads.
fn rgba_to_dib(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut dib = Vec::with_capacity(40 + w * h * 4);
    for v in [40u32, width, height] {
        dib.extend(v.to_le_bytes());
    }
    dib.extend(1u16.to_le_bytes()); // planes
    dib.extend(32u16.to_le_bytes()); // bits per pixel
    dib.extend(0u32.to_le_bytes()); // BI_RGB
    dib.extend(((w * h * 4) as u32).to_le_bytes());
    dib.extend([0u8; 16]); // resolution and palette sizes
    for y in (0..h).rev() {
        for p in rgba[y * w * 4..(y + 1) * w * 4].as_chunks::<4>().0 {
            dib.extend([p[2], p[1], p[0], p[3]]);
        }
    }
    dib
}

fn encode_png(width: u32, height: u32, rgba: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(std::io::Error::other)?;
    writer
        .write_image_data(rgba)
        .map_err(std::io::Error::other)?;
    writer.finish().map_err(std::io::Error::other)?;
    Ok(out)
}

/// Only the RGBA PNGs this module writes.
fn decode_png(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .ok()?;
    let mut rgba = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut rgba).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    rgba.truncate(info.buffer_size());
    Some((info.width, info.height, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(t: &str) -> Captured {
        Captured::Text {
            text: t.into(),
            html: None,
            rtf: None,
        }
    }

    #[test]
    fn history_dedupes_filters_pins_and_prunes() {
        let clips = Clips::with(Connection::open_in_memory().unwrap()).unwrap();
        let day = 24 * 60 * 60;
        clips.add(text("old note"), 0).unwrap();
        clips.add(text("invoice 2041"), 40 * day).unwrap();
        clips
            .add(
                Captured::Files(vec![r"C:\a\report.pdf".into()]),
                40 * day + 5,
            )
            .unwrap();
        clips.add(text("invoice 2041"), 40 * day + 10).unwrap();
        let all = clips.items("", 40 * day + 70).unwrap();
        let titles: Vec<&str> = all.iter().map(|i| i.title.as_str()).collect();
        // The 40-day-old note is gone, and the repeat copy moved to the top instead of doubling.
        assert_eq!(titles, ["invoice 2041", "report.pdf"]);
        assert_eq!(all[0].subtitle, "Text · 12 B · 1 min ago");
        assert_eq!(clips.items("INVOICE", 40 * day).unwrap().len(), 1);

        let id: i64 = all[1].id.strip_prefix("clip:").unwrap().parse().unwrap();
        clips
            .db
            .lock()
            .unwrap()
            .execute("UPDATE clips SET pinned = 1 WHERE id = ?1", [id])
            .unwrap();
        let pinned = clips.items("", 40 * day + 70).unwrap();
        assert_eq!(pinned[0].title, "report.pdf");
        assert!(pinned[0].subtitle.starts_with("Pinned · Files"));
        assert!(pinned[0].actions.iter().any(|a| a.id == "clipunpin"));
        // Pinned entries outlive the 30 days.
        clips.add(text("later"), 100 * day).unwrap();
        let later = clips.items("", 100 * day).unwrap();
        assert_eq!(later.len(), 2);
    }

    #[test]
    fn modes_and_clipboard_formats() {
        assert_eq!(mode("clip "), Some(""));
        assert_eq!(mode("clip invoice "), Some("invoice"));
        assert_eq!(mode("clipchamp"), None);
        assert_eq!(mode("clip"), None);

        let mut drop = vec![20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0];
        for path in [r"C:\a.txt", r"D:\b c.png"] {
            drop.extend(path.encode_utf16().chain([0]).flat_map(u16::to_le_bytes));
        }
        drop.extend([0, 0]);
        assert_eq!(files_from(&drop), [r"C:\a.txt", r"D:\b c.png"]);
        assert_eq!(text_from(&clipboard::text_bytes("héllo")), "héllo");
    }

    #[test]
    fn images_round_trip_through_dib_and_png() {
        // 2 × 2: red, green / blue, white with half alpha.
        let rgba = vec![
            255, 0, 0, 255, 0, 255, 0, 255, //
            0, 0, 255, 255, 255, 255, 255, 128,
        ];
        let dib = rgba_to_dib(2, 2, &rgba);
        assert_eq!(dib_to_rgba(&dib), Some((2, 2, rgba.clone())));
        let png = encode_png(2, 2, &rgba).unwrap();
        assert_eq!(decode_png(&png), Some((2, 2, rgba)));

        // 24-bit, bottom-up, rows padded to four bytes: one row of 1 × 2.
        let mut dib24 = rgba_to_dib(1, 2, &[0; 8])[..40].to_vec();
        dib24[14] = 24;
        dib24.extend([1, 2, 3, 0, 4, 5, 6, 0]);
        let (_, _, pixels) = dib_to_rgba(&dib24).unwrap();
        assert_eq!(
            pixels,
            [6, 5, 4, 255, 3, 2, 1, 255],
            "bottom row comes last"
        );
    }
}
