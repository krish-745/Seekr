//! The preview pane for files and folders: highlighted text and code, Markdown, Office text,
//! folder listings, and shell thumbnails for images, video and anything else the shell can draw.
//! Cloud-only files never have their content read, so previewing doesn't download them.

use std::{
    fs,
    io::{self, BufRead, BufReader, Read},
    os::windows::fs::MetadataExt,
    path::Path,
    sync::OnceLock,
};

use pulldown_cmark::{Event, Options, Parser};
use quick_xml::{Reader, events::Event as Xml};
use serde::Serialize;
use syntect::{
    easy::HighlightLines, highlighting::ThemeSet, parsing::SyntaxSet, util::LinesWithEndings,
};

/// Read at most this much of a text file; the pane shows only the top anyway.
const TEXT_BYTES: u64 = 64 * 1024;
const CODE_LINES: usize = 400;
/// Minified files have megabyte lines that highlight slowly and show nothing useful.
const LINE_CHARS: usize = 1_000;
const FOLDER_ENTRIES: usize = 200;
const DOCUMENT_CHARS: usize = 20_000;
/// Uncompressed bytes read from one part of an Office file, against zip bombs.
const OFFICE_PART_BYTES: u64 = 16 * 1024 * 1024;
/// Thumbnail size in physical pixels; the pane is about 500 px wide.
const THUMB_PX: i32 = 512;
const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
const OFFLINE: u32 = 0x1000;
/// Extensions previewed with a shell thumbnail rather than their bytes.
const THUMBNAILS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "heic", "heif", "bmp", "tif", "tiff", "ico", "avif",
    "svg", "mp4", "mkv", "mov", "avi", "wmv", "webm", "m4v", "pdf",
];

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Preview {
    /// A `thumb.localhost` URL; the pane shows nothing if the shell has no thumbnail.
    Image {
        url: String,
    },
    /// Lines of (`#rrggbb`, text) runs.
    Code {
        lines: Vec<Vec<(String, String)>>,
        truncated: bool,
    },
    /// HTML from Markdown with any raw HTML dropped.
    Markdown {
        html: String,
    },
    Folder {
        entries: Vec<Entry>,
        total: usize,
    },
    Document {
        text: String,
        truncated: bool,
    },
    Nothing,
}

#[derive(Serialize)]
pub struct Entry {
    name: String,
    dir: bool,
}

/// The PNG for a `thumb.localhost/?path=...` request, decoded from the query `image` built.
pub fn thumbnail(query: &str) -> Option<Vec<u8>> {
    let encoded = query.strip_prefix("path=")?;
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    let path = String::from_utf8(decoded).ok()?;
    let _com = crate::fileops::Com::init();
    crate::apps::thumbnail_png(&path, THUMB_PX)
}

#[tauri::command]
pub async fn preview(path: String, dark: bool) -> Preview {
    tauri::async_runtime::spawn_blocking(move || build(&path, dark))
        .await
        .unwrap_or(Preview::Nothing)
}

fn build(path: &str, dark: bool) -> Preview {
    let p = Path::new(path);
    let Ok(meta) = fs::metadata(p) else {
        return Preview::Nothing;
    };
    if meta.is_dir() {
        return folder(p).unwrap_or(Preview::Nothing);
    }
    let ext = p
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let cloud_only = meta.file_attributes() & (RECALL_ON_DATA_ACCESS | OFFLINE) != 0;
    if cloud_only || THUMBNAILS.contains(&ext.as_str()) {
        return image(path);
    }
    let truncated = meta.len() > TEXT_BYTES;
    let built = match ext.as_str() {
        "docx" | "pptx" | "xlsx" => office(p, &ext).ok(),
        "md" | "markdown" => text(p).map(|t| markdown(&t)),
        _ => text(p).map(|t| code(&t, &ext, dark, truncated)),
    };
    // Binary files without a text form may still have a thumbnail.
    built.unwrap_or_else(|| image(path))
}

fn image(path: &str) -> Preview {
    let query: String = path
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect();
    Preview::Image {
        url: format!("http://thumb.localhost/?path={query}"),
    }
}

/// The start of a text file, or None for binary content. UTF-16 files (with a byte order mark,
/// as Windows writes them) are decoded; anything else is read as UTF-8.
fn text(path: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(TEXT_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_le_bytes(c))
            .collect();
        return Some(String::from_utf16_lossy(&units));
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return None;
    }
    Some(String::from_utf8_lossy(bytes).into_owned())
}

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn themes() -> &'static ThemeSet {
    static SET: OnceLock<ThemeSet> = OnceLock::new();
    SET.get_or_init(ThemeSet::load_defaults)
}

fn code(text: &str, ext: &str, dark: bool, truncated: bool) -> Preview {
    let ss = syntaxes();
    let first = text.lines().next().unwrap_or("");
    let syntax = ss
        .find_syntax_by_extension(ext)
        .or_else(|| ss.find_syntax_by_first_line(first))
        .unwrap_or_else(|| ss.find_syntax_plain_text());
    let theme = &themes().themes[if dark {
        "base16-ocean.dark"
    } else {
        "InspiredGitHub"
    }];
    let mut highlighter = HighlightLines::new(syntax, theme);
    let mut lines = Vec::new();
    for line in LinesWithEndings::from(text).take(CODE_LINES) {
        let line: String = line.chars().take(LINE_CHARS).collect();
        let runs = highlighter.highlight_line(&line, ss).unwrap_or_default();
        lines.push(
            runs.into_iter()
                .map(|(style, run)| {
                    let c = style.foreground;
                    let run = run.trim_end_matches(['\r', '\n']).to_string();
                    (format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b), run)
                })
                .collect(),
        );
    }
    let truncated = truncated || text.lines().nth(CODE_LINES).is_some();
    Preview::Code { lines, truncated }
}

/// Raw HTML is dropped, so the pane can't be made to run script or load remote content.
fn markdown(text: &str) -> Preview {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let events = Parser::new_ext(text, options)
        .filter(|e| !matches!(e, Event::Html(_) | Event::InlineHtml(_)));
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, events);
    Preview::Markdown { html }
}

fn folder(path: &Path) -> io::Result<Preview> {
    let mut entries: Vec<Entry> = fs::read_dir(path)?
        .flatten()
        .map(|e| Entry {
            name: e.file_name().to_string_lossy().into_owned(),
            dir: e.file_type().is_ok_and(|t| t.is_dir()),
        })
        .collect();
    entries.sort_by_cached_key(|e| (!e.dir, e.name.to_lowercase()));
    let total = entries.len();
    entries.truncate(FOLDER_ENTRIES);
    Ok(Preview::Folder { entries, total })
}

/// The text of a Word document, a PowerPoint deck (slide by slide) or an Excel workbook's
/// strings, from the XML parts inside the zip.
fn office(path: &Path, ext: &str) -> zip::result::ZipResult<Preview> {
    let mut archive = zip::ZipArchive::new(BufReader::new(fs::File::open(path)?))?;
    let mut parts: Vec<String> = match ext {
        "docx" => vec!["word/document.xml".into()],
        "xlsx" => vec!["xl/sharedStrings.xml".into()],
        _ => archive
            .file_names()
            .flatten()
            .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
            .map(|n| n.into_owned())
            .collect(),
    };
    // slide10 after slide9.
    parts.sort_by_key(|n| {
        let digits: String = n.chars().filter(char::is_ascii_digit).collect();
        digits.parse::<u32>().unwrap_or(0)
    });
    let mut text = String::new();
    for name in parts {
        let Ok(part) = archive.by_name(&name) else {
            continue;
        };
        xml_text(BufReader::new(part.take(OFFICE_PART_BYTES)), &mut text);
        text.push('\n');
        if text.len() > DOCUMENT_CHARS {
            break;
        }
    }
    let truncated = text.len() > DOCUMENT_CHARS;
    if truncated {
        let end = (0..=DOCUMENT_CHARS)
            .rev()
            .find(|&i| text.is_char_boundary(i))
            .unwrap_or(0);
        text.truncate(end);
    }
    Ok(Preview::Document {
        text: text.trim().to_string(),
        truncated,
    })
}

/// Appends the content of every `<t>` element (`w:t`, `a:t`, plain `t`), with a line break
/// after each paragraph or shared string.
fn xml_text(input: impl BufRead, out: &mut String) {
    let mut reader = Reader::from_reader(input);
    let mut buf = Vec::new();
    let mut in_text = false;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Xml::Start(e)) if e.local_name().as_ref() == "t" => in_text = true,
            Ok(Xml::End(e)) => match e.local_name().as_ref() {
                "t" => in_text = false,
                "p" | "si" => out.push('\n'),
                _ => {}
            },
            Ok(Xml::Text(t)) if in_text => out.push_str(&t),
            Ok(Xml::GeneralRef(r)) if in_text => match r.resolve_char_ref() {
                Ok(Some(c)) => out.push(c),
                _ => out.push_str(match &*r {
                    "lt" => "<",
                    "gt" => ">",
                    "amp" => "&",
                    "quot" => "\"",
                    "apos" => "'",
                    _ => "",
                }),
            },
            Ok(Xml::Eof) | Err(_) => break,
            _ => {}
        }
        if out.len() > DOCUMENT_CHARS {
            break;
        }
        buf.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnail_urls_round_trip_paths() {
        let Preview::Image { url } = image(r"C:\Users\me\My Photos\ünï 100%.png") else {
            panic!("not an image");
        };
        let query = url.split_once('?').unwrap().1;
        assert!(!query.contains(' ') && !query.contains('\\'));
        let encoded = query.strip_prefix("path=").unwrap();
        let mut decoded = Vec::new();
        let mut i = 0;
        while i < encoded.len() {
            if &encoded[i..i + 1] == "%" {
                decoded.push(u8::from_str_radix(&encoded[i + 1..i + 3], 16).unwrap());
                i += 3;
            } else {
                decoded.push(encoded.as_bytes()[i]);
                i += 1;
            }
        }
        assert_eq!(
            String::from_utf8(decoded).unwrap(),
            r"C:\Users\me\My Photos\ünï 100%.png"
        );
    }

    #[test]
    fn markdown_drops_raw_html() {
        let Preview::Markdown { html } =
            markdown("# Title\n\n<script>alert(1)</script>\n\nok <img src=x onerror=y>")
        else {
            panic!("not markdown");
        };
        assert!(html.contains("<h1>Title</h1>"));
        assert!(!html.contains("<script") && !html.contains("<img"));
    }

    #[test]
    fn code_is_highlighted_in_runs() {
        let Preview::Code { lines, truncated } = code("fn main() {}\n", "rs", true, false) else {
            panic!("not code");
        };
        assert!(!truncated);
        assert_eq!(lines.len(), 1);
        let joined: String = lines[0].iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(joined, "fn main() {}");
        assert!(lines[0].len() > 1, "keywords get their own colour");
    }

    #[test]
    fn office_xml_text_keeps_paragraphs_and_entities() {
        let xml = r#"<w:document><w:body><w:p><w:r><w:t>Fish &amp; chips</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">A&#33;</w:t></w:r></w:p></w:body></w:document>"#;
        let mut out = String::new();
        xml_text(xml.as_bytes(), &mut out);
        assert_eq!(out, "Fish & chips\nA!\n");
    }

    #[test]
    fn text_reads_utf16_and_refuses_binary() {
        let dir = std::env::temp_dir().join(format!("seekr-preview-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("héllo".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        fs::write(dir.join("a.txt"), utf16).unwrap();
        fs::write(dir.join("b.bin"), [1, 0, 2, 0]).unwrap();
        assert_eq!(text(&dir.join("a.txt")).as_deref(), Some("héllo"));
        assert_eq!(text(&dir.join("b.bin")), None);
        let Ok(Preview::Folder { entries, total }) = folder(&dir) else {
            panic!("not a folder");
        };
        assert_eq!(total, 2);
        assert_eq!(entries[0].name, "a.txt");
        fs::remove_dir_all(&dir).unwrap();
    }
}
