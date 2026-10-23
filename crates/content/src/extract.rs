//! Plain text from the files content search reads: text and code, Markdown, Jupyter notebooks,
//! RTF, Word, PowerPoint and Excel files, and PDFs (through pdfium.dll).

use std::{
    fs,
    io::{self, BufRead, BufReader, Read},
    path::Path,
    sync::OnceLock,
};

use pdfium_render::prelude::Pdfium;
use quick_xml::{Reader, events::Event as Xml};

/// Larger text files are logs or data dumps, not documents anyone searches by content.
pub const MAX_TEXT_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_OFFICE_BYTES: u64 = 64 * 1024 * 1024;
/// Scanned PDFs run to hundreds of megabytes of images with no text to find.
pub const MAX_PDF_BYTES: u64 = 64 * 1024 * 1024;
/// Uncompressed bytes read from one part of an Office file, against zip bombs.
const OFFICE_PART_BYTES: u64 = 32 * 1024 * 1024;
/// All the XML a document may inflate to: a deck of hundreds of zip-bomb slides would otherwise
/// keep a core busy for seconds at every preview.
const OFFICE_TOTAL_BYTES: u64 = 96 * 1024 * 1024;

const TEXT: &[&str] = &[
    "txt",
    "text",
    "log",
    "csv",
    "tsv",
    "json",
    "jsonl",
    "yaml",
    "yml",
    "toml",
    "ini",
    "cfg",
    "conf",
    "xml",
    "html",
    "htm",
    "css",
    "scss",
    "less",
    "js",
    "mjs",
    "cjs",
    "ts",
    "tsx",
    "jsx",
    "rs",
    "py",
    "pyi",
    "rb",
    "go",
    "java",
    "kt",
    "kts",
    "scala",
    "c",
    "h",
    "cpp",
    "hpp",
    "cc",
    "cxx",
    "cs",
    "fs",
    "swift",
    "php",
    "lua",
    "pl",
    "sh",
    "bash",
    "zsh",
    "ps1",
    "psm1",
    "bat",
    "cmd",
    "sql",
    "r",
    "m",
    "jl",
    "dart",
    "vue",
    "svelte",
    "tex",
    "bib",
    "rst",
    "adoc",
    "org",
    "gradle",
    "properties",
    "srt",
    "vtt",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Markdown,
    Notebook,
    Rtf,
    Word,
    PowerPoint,
    Excel,
    Pdf,
}

impl Kind {
    pub fn of(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_string_lossy().to_lowercase();
        Some(match ext.as_str() {
            "md" | "markdown" | "mdx" => Kind::Markdown,
            "ipynb" => Kind::Notebook,
            "rtf" => Kind::Rtf,
            "docx" => Kind::Word,
            "pptx" => Kind::PowerPoint,
            "xlsx" => Kind::Excel,
            "pdf" => Kind::Pdf,
            e if TEXT.contains(&e) => Kind::Text,
            _ => return None,
        })
    }

    pub fn max_bytes(self) -> u64 {
        match self {
            Kind::Word | Kind::PowerPoint | Kind::Excel => MAX_OFFICE_BYTES,
            Kind::Pdf => MAX_PDF_BYTES,
            _ => MAX_TEXT_BYTES,
        }
    }
}

/// The text of `path`, or None when it turns out to be binary.
pub fn extract(path: &Path, kind: Kind) -> io::Result<Option<String>> {
    Ok(match kind {
        Kind::Text | Kind::Markdown => text(path, MAX_TEXT_BYTES)?,
        Kind::Notebook => text(path, MAX_TEXT_BYTES)?.map(|t| notebook(&t)),
        Kind::Rtf => text(path, MAX_TEXT_BYTES)?.map(|t| rtf(&t)),
        Kind::Word | Kind::PowerPoint | Kind::Excel => Some(office(path, kind, usize::MAX)?),
        Kind::Pdf => Some(pdf(path, MAX_TEXT_BYTES as usize)?),
    })
}

/// pdfium.dll from beside the running exe, bound once; None when it is missing.
fn pdfium() -> Option<&'static Pdfium> {
    static PDFIUM: OnceLock<Option<Pdfium>> = OnceLock::new();
    PDFIUM
        .get_or_init(|| {
            let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
            let library = Pdfium::pdfium_platform_library_name_at_path(&dir);
            Pdfium::bind_to_library(library).ok().map(Pdfium::new)
        })
        .as_ref()
}

/// The text of a PDF, page by page with a blank line between pages, stopping after `limit`
/// bytes. Scanned pages have no text layer, so they add nothing.
pub fn pdf(path: &Path, limit: usize) -> io::Result<String> {
    let pdfium = pdfium().ok_or_else(|| io::Error::other("pdfium.dll is missing"))?;
    let document = pdfium
        .load_pdf_from_file(path, None)
        .map_err(|e| io::Error::other(format!("{e:?}")))?;
    let mut text = String::new();
    for page in document.pages().iter() {
        if let Ok(page_text) = page.text() {
            text.push_str(page_text.all().trim_end());
        }
        text.push_str("\n\n");
        if text.len() > limit {
            break;
        }
    }
    Ok(text)
}

/// Up to `limit` bytes of a text file, or None for binary content. UTF-16 files with a byte
/// order mark (as Windows writes them) are decoded; anything else is read as UTF-8.
pub fn text(path: &Path, limit: u64) -> io::Result<Option<String>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(limit).read_to_end(&mut bytes)?;
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_le_bytes(c))
            .collect();
        return Ok(Some(String::from_utf16_lossy(&units)));
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(bytes).into_owned()))
}

/// The source of every cell, markdown and code alike, separated by blank lines.
fn notebook(json: &str) -> String {
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(json) else {
        return String::new();
    };
    let cells = doc["cells"].as_array().cloned().unwrap_or_default();
    cells
        .iter()
        .map(|cell| match &cell["source"] {
            serde_json::Value::Array(lines) => lines.iter().filter_map(|l| l.as_str()).collect(),
            serde_json::Value::String(s) => s.clone(),
            _ => String::new(),
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Plain text from RTF: groups that hold no text (fonts, colours, pictures, `{\*...}`) are
/// skipped, control words dropped, `\par` and `\line` become line breaks, `\'hh` and `\uN`
/// become characters.
fn rtf(src: &str) -> String {
    let mut out = String::new();
    // For each open group, whether its text is skipped.
    let mut skip = vec![false];
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        let skipping = *skip.last().unwrap_or(&false);
        match c {
            '{' => skip.push(skipping),
            '}' => {
                skip.pop();
            }
            '\\' => {
                let mut word = String::new();
                while let Some(&n) = chars.peek().filter(|n| n.is_ascii_alphabetic()) {
                    word.push(n);
                    chars.next();
                }
                if word.is_empty() {
                    match chars.next() {
                        Some('\'') => {
                            let hex: String = chars.by_ref().take(2).collect();
                            if let (false, Ok(b)) = (skipping, u8::from_str_radix(&hex, 16)) {
                                // Windows-1252 matches Latin-1 for letters people type.
                                out.push(b as char);
                            }
                        }
                        Some('*') => {
                            if let Some(top) = skip.last_mut() {
                                *top = true;
                            }
                        }
                        Some(other @ ('\\' | '{' | '}')) if !skipping => out.push(other),
                        _ => {}
                    }
                    continue;
                }
                let mut number = String::new();
                while let Some(&n) = chars.peek().filter(|n| n.is_ascii_digit() || **n == '-') {
                    number.push(n);
                    chars.next();
                }
                if chars.peek() == Some(&' ') {
                    chars.next();
                }
                match word.as_str() {
                    "fonttbl" | "colortbl" | "stylesheet" | "info" | "pict" | "header"
                    | "footer" => {
                        if let Some(top) = skip.last_mut() {
                            *top = true;
                        }
                    }
                    "par" | "line" if !skipping => out.push('\n'),
                    "tab" if !skipping => out.push('\t'),
                    "u" if !skipping => {
                        if let Some(ch) = number
                            .parse::<i32>()
                            .ok()
                            .and_then(|n| char::from_u32(n.rem_euclid(65536) as u32))
                        {
                            out.push(ch);
                        }
                        // The ANSI stand-in that follows \uN.
                        if chars.peek() == Some(&'?') {
                            chars.next();
                        }
                    }
                    _ => {}
                }
            }
            '\r' | '\n' => {}
            c if !skipping => out.push(c),
            _ => {}
        }
    }
    out
}

/// The text of a Word document, a PowerPoint deck (slide by slide) or an Excel workbook's
/// strings, from the XML parts inside the zip, stopping after `limit` bytes.
pub fn office(path: &Path, kind: Kind, limit: usize) -> io::Result<String> {
    let mut archive =
        zip::ZipArchive::new(BufReader::new(fs::File::open(path)?)).map_err(io::Error::other)?;
    let mut parts: Vec<String> = match kind {
        Kind::Word => vec!["word/document.xml".into()],
        Kind::Excel => vec!["xl/sharedStrings.xml".into()],
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
    let mut left = OFFICE_TOTAL_BYTES;
    for name in parts {
        let Ok(part) = archive.by_name(&name) else {
            continue;
        };
        let cap = left.min(OFFICE_PART_BYTES);
        let mut part = BufReader::new(part.take(cap));
        xml_text(&mut part, &mut text, limit);
        left -= cap - part.get_ref().limit();
        text.push('\n');
        if text.len() > limit || left == 0 {
            break;
        }
    }
    Ok(text)
}

/// Appends the content of every `<t>` element (`w:t`, `a:t`, plain `t`), with a line break
/// after each paragraph or shared string.
fn xml_text(input: &mut impl BufRead, out: &mut String, limit: usize) {
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
        if out.len() > limit {
            break;
        }
        buf.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deck_of_bomb_slides_stops_at_the_total_budget() {
        use std::io::Write as _;
        let path = std::env::temp_dir().join(format!("seekr-bomb-{}.pptx", std::process::id()));
        let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .large_file(true);
        let spaces = vec![b' '; 1 << 20];
        for n in 1..=5 {
            zip.start_file(format!("ppt/slides/slide{n}.xml"), options)
                .unwrap();
            zip.write_all(format!("<a:t>slide {n}</a:t>").as_bytes())
                .unwrap();
            for _ in 0..30 {
                zip.write_all(&spaces).unwrap();
            }
        }
        zip.start_file("ppt/slides/slide6.xml", options).unwrap();
        zip.write_all(b"<a:t>past the budget</a:t>").unwrap();
        zip.finish().unwrap();
        let text = office(&path, Kind::PowerPoint, usize::MAX).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(
            text.contains("slide 1") && text.contains("slide 4"),
            "{text}"
        );
        assert!(
            !text.contains("slide 5") && !text.contains("past the budget"),
            "{text}"
        );
    }

    /// Times extraction of every file in SEEKR_HOSTILE (made by a stress script):
    /// `cargo test -p content hostile_files -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn hostile_files() {
        let dir = std::env::var("SEEKR_HOSTILE").unwrap();
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            let Some(kind) = Kind::of(&path) else {
                continue;
            };
            for (what, limit) in [("preview", 20_000), ("index", usize::MAX)] {
                let start = std::time::Instant::now();
                let result = match kind {
                    Kind::Pdf => pdf(&path, limit),
                    _ => office(&path, kind, limit),
                };
                println!(
                    "{:>15} {what:7} {:>8.2?} {}",
                    path.file_name().unwrap().to_string_lossy(),
                    start.elapsed(),
                    match result {
                        Ok(t) => format!("{} chars", t.len()),
                        Err(e) => format!("error: {e}"),
                    }
                );
            }
        }
    }

    /// A PDF with one page per string, each drawing it in Helvetica, its byte offsets right.
    fn write_pdf(path: &Path, pages: &[&str]) {
        let n = pages.len();
        let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];
        for (i, words) in pages.iter().enumerate() {
            let stream = format!("BT /F1 12 Tf 72 720 Td ({words}) Tj ET");
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
                5 + 2 * i
            ));
            objects.push(format!(
                "<< /Length {} >>\nstream\n{stream}\nendstream",
                stream.len()
            ));
        }
        let mut pdf = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (i, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.push_str(&format!("{} 0 obj\n{object}\nendobj\n", i + 1));
        }
        let xref = pdf.len();
        pdf.push_str(&format!(
            "xref\n0 {}\n0000000000 65535 f \n",
            objects.len() + 1
        ));
        for offset in offsets {
            pdf.push_str(&format!("{offset:010} 00000 n \n"));
        }
        pdf.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        ));
        fs::write(path, pdf).unwrap();
    }

    #[test]
    fn pdf_text_page_by_page() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("seekr-pages-{}.pdf", std::process::id()));
        write_pdf(&path, &["Quarterly invoice total", "Second page words"]);
        let text = extract(&path, Kind::Pdf).unwrap().unwrap();
        let first = pdf(&path, 1).unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(text, "Quarterly invoice total\n\nSecond page words\n\n");
        assert_eq!(
            first, "Quarterly invoice total\n\n",
            "stops after the limit"
        );
    }

    #[test]
    fn a_broken_pdf_is_an_error_not_a_crash() {
        let path = std::env::temp_dir().join(format!("seekr-broken-{}.pdf", std::process::id()));
        fs::write(&path, b"%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 9 0 R").unwrap();
        let result = pdf(&path, usize::MAX);
        fs::remove_file(&path).unwrap();
        assert!(result.is_err(), "{result:?}");
    }

    #[test]
    fn kinds_by_extension() {
        assert_eq!(Kind::of(Path::new("a.RS")), Some(Kind::Text));
        assert_eq!(Kind::of(Path::new("notes.md")), Some(Kind::Markdown));
        assert_eq!(Kind::of(Path::new("deck.pptx")), Some(Kind::PowerPoint));
        assert_eq!(Kind::of(Path::new("Report.PDF")), Some(Kind::Pdf));
        assert_eq!(Kind::of(Path::new("photo.jpg")), None);
        assert_eq!(
            Kind::of(Path::new(".env")),
            None,
            "secrets stay out of the index"
        );
    }

    #[test]
    fn office_xml_text_keeps_paragraphs_and_entities() {
        let xml = r#"<w:document><w:body><w:p><w:r><w:t>Fish &amp; chips</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">A&#33;</w:t></w:r></w:p></w:body></w:document>"#;
        let mut out = String::new();
        xml_text(&mut xml.as_bytes(), &mut out, usize::MAX);
        assert_eq!(out, "Fish & chips\nA!\n");
    }

    #[test]
    fn notebook_cells_in_order() {
        let nb = r##"{"cells":[{"cell_type":"markdown","source":["# Title\n","Intro"]},{"cell_type":"code","source":"print(1)"}]}"##;
        assert_eq!(notebook(nb), "# Title\nIntro\n\nprint(1)");
        assert_eq!(notebook("not json"), "");
    }

    #[test]
    fn rtf_keeps_text_only() {
        // `^` stands for a backslash, so the source holds no escape sequences of its own.
        let src = r"{^rtf1^ansi{^fonttbl{^f0 Arial;}}{^*^generator Word;}^f0 Hello ^b world^b0^par Caf^'e9 ^u8364? done}"
            .replace('^', &char::from(92).to_string());
        assert_eq!(
            rtf(&src),
            format!("Hello world\nCaf{} {} done", char::from(0xE9), '\u{20ac}')
        );
    }
}
