//! App index: everything in shell:AppsFolder (Start menu shortcuts and Store apps), with icons
//! rendered once into a PNG cache and served to the panel through the `icon:` URI scheme.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, RwLock},
    time::{Duration, Instant},
};

use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;
use windows::{
    Win32::{
        Foundation::{HWND, SIZE},
        Graphics::Gdi::{
            BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS,
            DeleteDC, DeleteObject, GetDIBits, GetObjectW, HBITMAP, HGDIOBJ,
        },
        Storage::EnhancedStorage::PKEY_Link_TargetParsingPath,
        System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize},
        UI::{
            Shell::{
                BHID_EnumItems, FOLDERID_AppsFolder, IEnumShellItems, IShellItem, IShellItem2,
                IShellItemImageFactory, KF_FLAG_DEFAULT, SHCreateItemFromParsingName,
                SHGetKnownFolderItem, SIGDN, SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING,
                SIIGBF, SIIGBF_ICONONLY, SIIGBF_THUMBNAILONLY, ShellExecuteW,
            },
            WindowsAndMessaging::SW_SHOWNORMAL,
        },
    },
    core::{HSTRING, Interface, PCWSTR, w},
};

use crate::{
    clipboard,
    query::{Action, Item},
};

const OPEN: Action = Action {
    id: "open",
    label: "Open",
};
const LOCATION: Action = Action {
    id: "location",
    label: "Open file location",
};
const ADMIN: Action = Action {
    id: "admin",
    label: "Run as administrator",
};
const COPY_PATH: Action = Action {
    id: "copy",
    label: "Copy path",
};
/// Icon edge in physical pixels: sharp at the 24 px row size up to 200% scaling.
const ICON_PX: i32 = 48;
/// New installs show up on the first panel open after this much time.
const RESCAN_AFTER: Duration = Duration::from_secs(60);

struct App {
    name: String,
    /// The AppsFolder parsing name: an AppUserModelID or a known-folder path.
    id: String,
    /// The program a desktop app's shortcut points at; Store apps have none.
    target: Option<String>,
    icon: Option<String>,
}

#[derive(Default)]
pub struct AppIndex {
    apps: RwLock<Vec<App>>,
    scanned_at: Mutex<Option<Instant>>,
}

pub fn icon_dir() -> PathBuf {
    common::data_dir().join("icons")
}

pub fn all(index: &AppIndex) -> Vec<Item> {
    let apps = index.apps.read().unwrap();
    apps.iter()
        .map(|app| Item {
            id: format!("app:{}", app.id),
            title: app.name.clone(),
            subtitle: "Application".into(),
            preview: app.target.clone().unwrap_or_else(|| app.id.clone()),
            icon: app.icon.clone(),
            actions: match app.target {
                Some(_) => vec![OPEN, LOCATION, ADMIN, COPY_PATH],
                None => vec![OPEN],
            },
        })
        .collect()
}

/// Rescans on a background thread unless a scan ran or started within RESCAN_AFTER.
pub fn refresh(app: &AppHandle) {
    {
        let index = app.state::<AppIndex>();
        let mut at = index.scanned_at.lock().unwrap();
        if at.is_some_and(|t| t.elapsed() < RESCAN_AFTER) {
            return;
        }
        *at = Some(Instant::now());
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let started = Instant::now();
        match scan(&icon_dir()) {
            Ok(apps) => {
                tracing::info!("indexed {} apps in {:?}", apps.len(), started.elapsed());
                *app.state::<AppIndex>().apps.write().unwrap() = apps;
            }
            Err(e) => tracing::error!("app scan failed: {e}"),
        }
    });
}

pub fn run(app: &AppHandle, id: &str, action: &str, owner: HWND) -> Result<(), String> {
    let target = || {
        let index = app.state::<AppIndex>();
        let apps = index.apps.read().unwrap();
        let app = apps.iter().find(|a| a.id == id);
        app.and_then(|a| a.target.clone())
            .ok_or_else(|| format!("{id} has no program path"))
    };
    match action {
        "open" => launch(id, w!("open")),
        // The shell shows the UAC prompt; declining it is an error we just log.
        "admin" => launch(id, w!("runas")),
        "location" => app
            .opener()
            .reveal_item_in_dir(target()?)
            .map_err(|e| e.to_string()),
        "copy" => clipboard::set_text(owner, &target()?),
        _ => Err(format!("unknown action {action} on app {id}")),
    }
}

fn launch(id: &str, verb: PCWSTR) -> Result<(), String> {
    let target = HSTRING::from(format!(r"shell:AppsFolder\{id}"));
    let result = unsafe { ShellExecuteW(None, verb, &target, None, None, SW_SHOWNORMAL) };
    // ShellExecute reports success as any value above 32.
    if result.0 as usize > 32 {
        Ok(())
    } else {
        Err(format!(
            "could not launch {id} (code {})",
            result.0 as usize
        ))
    }
}

fn scan(icon_dir: &Path) -> windows::core::Result<Vec<App>> {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
    let result = scan_apps_folder(icon_dir);
    unsafe { CoUninitialize() };
    let mut apps = result?;
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps.dedup_by(|a, b| a.id == b.id);
    Ok(apps)
}

fn scan_apps_folder(icon_dir: &Path) -> windows::core::Result<Vec<App>> {
    if let Err(e) = fs::create_dir_all(icon_dir) {
        tracing::warn!("no icon cache: {e}");
    }
    let folder: IShellItem =
        unsafe { SHGetKnownFolderItem(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None)? };
    let items: IEnumShellItems = unsafe { folder.BindToHandler(None, &BHID_EnumItems)? };
    let mut apps = Vec::new();
    loop {
        let mut slot = [None];
        unsafe { items.Next(&mut slot, None)? };
        let Some(item) = slot[0].take() else {
            break;
        };
        let (Ok(name), Ok(id)) = (
            display_name(&item, SIGDN_NORMALDISPLAY),
            display_name(&item, SIGDN_PARENTRELATIVEPARSING),
        ) else {
            continue;
        };
        let icon = cached_icon(&item, &id, icon_dir);
        let target = target_path(&item);
        apps.push(App {
            name,
            id,
            target,
            icon,
        });
    }
    Ok(apps)
}

fn display_name(item: &IShellItem, sigdn: SIGDN) -> windows::core::Result<String> {
    let raw = unsafe { item.GetDisplayName(sigdn)? };
    let name = unsafe { raw.to_string() };
    unsafe { CoTaskMemFree(Some(raw.0 as _)) };
    name.map_err(|_| windows::core::Error::empty())
}

fn target_path(item: &IShellItem) -> Option<String> {
    let item: IShellItem2 = item.cast().ok()?;
    let raw = unsafe { item.GetString(&PKEY_Link_TargetParsingPath).ok()? };
    let path = unsafe { raw.to_string() };
    unsafe { CoTaskMemFree(Some(raw.0 as _)) };
    path.ok().filter(|p| !p.is_empty())
}

/// shortcut: an icon is rendered once per app id and never refreshed; delete the icons folder
/// to pick up changed icons until the indexer tracks app updates.
fn cached_icon(item: &IShellItem, id: &str, icon_dir: &Path) -> Option<String> {
    let file = format!("{:016x}.png", fnv1a(id));
    let path = icon_dir.join(&file);
    if !path.exists() {
        let png = render(item, ICON_PX, SIIGBF_ICONONLY)?;
        fs::write(&path, png).ok()?;
    }
    Some(format!("http://icon.localhost/{file}"))
}

/// The shell's thumbnail of the file at `path` as a PNG at most `px` square, or None when it has
/// no thumbnail handler. Call with COM initialised on this thread.
pub fn thumbnail_png(path: &str, px: i32) -> Option<Vec<u8>> {
    let item: IShellItem = unsafe { SHCreateItemFromParsingName(&HSTRING::from(path), None).ok()? };
    render(&item, px, SIIGBF_THUMBNAILONLY)
}

/// Renders the shell image of `item` to a straight-alpha RGBA PNG.
fn render(item: &IShellItem, px: i32, flags: SIIGBF) -> Option<Vec<u8>> {
    let (w, h, rgba) = render_rgba(item, px, flags)?;
    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(&mut png, w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header().ok()?.write_image_data(&rgba).ok()?;
    Some(png)
}

fn render_rgba(item: &IShellItem, px: i32, flags: SIIGBF) -> Option<(u32, u32, Vec<u8>)> {
    let factory: IShellItemImageFactory = item.cast().ok()?;
    let size = SIZE { cx: px, cy: px };
    let bitmap = unsafe { factory.GetImage(size, flags).ok()? };
    let pixels = bitmap_bgra(bitmap);
    unsafe {
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
    }
    let (w, h, mut bgra) = pixels?;
    bgra_premultiplied_to_rgba(&mut bgra);
    Some((w, h, bgra))
}

fn bitmap_bgra(bitmap: HBITMAP) -> Option<(u32, u32, Vec<u8>)> {
    let mut bm = BITMAP::default();
    let got = unsafe {
        GetObjectW(
            HGDIOBJ(bitmap.0),
            size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut _ as _),
        )
    };
    if got == 0 || bm.bmWidth <= 0 || bm.bmHeight <= 0 {
        return None;
    }
    let (w, h) = (bm.bmWidth, bm.bmHeight);
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            // Negative height asks for top-down rows.
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bgra = vec![0u8; (w * h * 4) as usize];
    let dc = unsafe { CreateCompatibleDC(None) };
    let lines = unsafe {
        GetDIBits(
            dc,
            bitmap,
            0,
            h as u32,
            Some(bgra.as_mut_ptr() as _),
            &mut info,
            DIB_RGB_COLORS,
        )
    };
    unsafe {
        let _ = DeleteDC(dc);
    }
    (lines == h).then_some((w as u32, h as u32, bgra))
}

/// The shell hands back premultiplied BGRA; PNG wants straight RGBA. Bitmaps with no alpha
/// at all (every byte zero) are opaque.
fn bgra_premultiplied_to_rgba(px: &mut [u8]) {
    let opaque = px.as_chunks::<4>().0.iter().all(|p| p[3] == 0);
    for p in px.as_chunks_mut::<4>().0 {
        let a = if opaque { 255 } else { p[3] };
        let unmul = |c: u8| {
            if a == 0 || a == 255 {
                c
            } else {
                (u16::from(c) * 255 / u16::from(a)).min(255) as u8
            }
        };
        let (b, g, r) = (p[0], p[1], p[2]);
        p[0] = unmul(r);
        p[1] = unmul(g);
        p[2] = unmul(b);
        p[3] = a;
    }
}

/// Stable across Rust versions, unlike `DefaultHasher`, so cache names survive upgrades.
fn fnv1a(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Only names this module writes, so a request can never reach outside the icon cache.
pub fn is_icon_file(name: &str) -> bool {
    name.len() == 20 && name.ends_with(".png") && name[..16].bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpremultiplies_and_swaps_channels() {
        // Half-transparent pure red, premultiplied BGRA.
        let mut px = [0, 0, 128, 128];
        bgra_premultiplied_to_rgba(&mut px);
        assert_eq!(px, [255, 0, 0, 128]);
    }

    #[test]
    fn alpha_free_bitmaps_become_opaque() {
        let mut px = [10, 20, 30, 0];
        bgra_premultiplied_to_rgba(&mut px);
        assert_eq!(px, [30, 20, 10, 255]);
    }

    #[test]
    fn icon_names_are_validated() {
        let name = format!("{:016x}.png", fnv1a("Microsoft.WindowsNotepad!App"));
        assert!(is_icon_file(&name));
        assert!(!is_icon_file("..\\..\\secret.png"));
        assert!(!is_icon_file("0123456789abcdef.exe"));
    }

    #[test]
    fn fnv1a_is_stable() {
        assert_eq!(fnv1a(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a("a"), 0xaf63_dc4c_8601_ec8c);
    }
}
