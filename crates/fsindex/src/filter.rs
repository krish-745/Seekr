//! The file query language: `ext:pdf,docx` `in:downloads` `in:C:\Work` `kind:image`
//! `folder:` `dm:today` `size:>10mb` `"exact words"` `-exclude`. Filters narrow by metadata; the
//! rest becomes a nucleo pattern for the name, where quotes turn into a substring atom and a
//! leading minus into a negation.

use windows::Win32::{Foundation::FILETIME, Storage::FileSystem::FileTimeToLocalFileTime};

const TICKS_PER_SECOND: i64 = 10_000_000;
const TICKS_PER_DAY: i64 = 86_400 * TICKS_PER_SECOND;
/// FILETIME of 1970-01-01 UTC.
const FILETIME_UNIX_EPOCH: i64 = 116_444_736_000_000_000;

const IMAGE: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "heic", "heif", "bmp", "tif", "tiff", "svg", "ico", "raw",
    "cr2", "nef", "arw", "dng", "avif",
];
const VIDEO: &[&str] = &[
    "mp4", "mkv", "mov", "avi", "wmv", "webm", "m4v", "flv", "mpg", "mpeg", "3gp",
];
const AUDIO: &[&str] = &[
    "mp3", "wav", "flac", "aac", "m4a", "ogg", "opus", "wma", "aiff", "mid",
];
const DOCUMENT: &[&str] = &[
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "rtf", "txt", "md",
    "csv", "epub", "pages", "key", "numbers",
];
const CODE: &[&str] = &[
    "rs", "py", "js", "ts", "tsx", "jsx", "c", "h", "cpp", "hpp", "cc", "cs", "java", "kt", "go",
    "rb", "php", "swift", "lua", "sh", "ps1", "bat", "sql", "html", "css", "scss", "json", "toml",
    "yaml", "yml", "xml", "ipynb", "r", "m", "dart", "vue", "svelte",
];
const ARCHIVE: &[&str] = &[
    "zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "xz", "zst", "iso", "cab",
];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    /// Lower-case extensions without the dot; a file matches any of them.
    pub exts: Vec<String>,
    /// Some(true) for folders only, Some(false) for files only.
    pub folders: Option<bool>,
    /// Lower-case `in:` terms, all required: a folder-name fragment, or a path from the drive
    /// root such as `c:\work` (`\work` once `on_drive` has checked the letter).
    pub within: Vec<String>,
    /// Last write as FILETIMEs, from inclusive to exclusive.
    pub modified: Option<(i64, i64)>,
    /// Bytes, both ends inclusive. Folders never match.
    pub size: Option<(u64, u64)>,
}

impl Filter {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// This filter for the index of `drive`, or None when an `in:` path names another drive.
    pub fn on_drive(&self, drive: char) -> Option<Self> {
        let mut filter = self.clone();
        for term in &mut filter.within {
            let mut chars = term.chars();
            if let (Some(letter), Some(':')) = (chars.next(), chars.next()) {
                if !letter.eq_ignore_ascii_case(&drive) {
                    return None;
                }
                *term = term[2..].to_string();
                if !term.starts_with('\\') {
                    term.insert(0, '\\');
                }
            }
        }
        Some(filter)
    }
}

/// One recognised filter as the panel shows it; `valid` is false when the value didn't parse
/// and the filter is ignored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chip {
    pub label: String,
    pub valid: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    /// The nucleo pattern for names; empty when only filters were typed.
    pub text: String,
    pub filter: Filter,
    pub chips: Vec<Chip>,
}

pub fn parse(query: &str) -> Parsed {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as i64 / 100)
        + FILETIME_UNIX_EPOCH;
    parse_at(query, now, local_offset(now))
}

/// Local time minus UTC, in FILETIME ticks.
fn local_offset(now: i64) -> i64 {
    let utc = FILETIME {
        dwLowDateTime: now as u32,
        dwHighDateTime: (now >> 32) as u32,
    };
    let mut local = FILETIME::default();
    if unsafe { FileTimeToLocalFileTime(&utc, &mut local) }.is_err() {
        return 0;
    }
    ((i64::from(local.dwHighDateTime) << 32) | i64::from(local.dwLowDateTime)) - now
}

fn parse_at(query: &str, now: i64, offset: i64) -> Parsed {
    let mut parsed = Parsed::default();
    let mut words = Vec::new();
    for token in tokens(query) {
        if let Some(phrase) = token.strip_prefix('"') {
            let phrase = phrase.strip_suffix('"').unwrap_or(phrase);
            if !phrase.is_empty() {
                words.push(format!("'{}", phrase.replace(' ', "\\ ")));
                parsed.chips.push(chip(format!("exact: {phrase}"), true));
            }
            continue;
        }
        if let Some(word) = token.strip_prefix('-').filter(|w| !w.is_empty()) {
            words.push(format!("!{word}"));
            parsed.chips.push(chip(format!("not: {word}"), true));
            continue;
        }
        let Some((key, value)) = token.split_once(':') else {
            words.push(token);
            continue;
        };
        let f = &mut parsed.filter;
        let lower = value.to_lowercase();
        let (label, valid) = match key.to_lowercase().as_str() {
            "ext" => {
                let exts: Vec<String> = lower
                    .split(',')
                    .map(|e| e.trim_start_matches('.').to_string())
                    .filter(|e| !e.is_empty())
                    .collect();
                let label = format!("ext: {}", exts.join(", "));
                let valid = !exts.is_empty();
                f.exts.extend(exts);
                (label, valid)
            }
            "in" => {
                let term = lower.trim_end_matches('\\').to_string();
                let valid = !term.is_empty();
                if valid {
                    f.within.push(term);
                }
                (format!("in: {value}"), valid)
            }
            "folder" | "folders" if lower.is_empty() => {
                f.folders = Some(true);
                ("folders only".into(), true)
            }
            "folder" => {
                f.within.push(lower.trim_end_matches('\\').to_string());
                (format!("in: {value}"), true)
            }
            "kind" | "type" => {
                let valid = apply_kind(f, &lower);
                (format!("kind: {lower}"), valid)
            }
            "dm" | "modified" => {
                let range = date_range(&lower, now, offset);
                f.modified = range.or(f.modified);
                (format!("modified: {lower}"), range.is_some())
            }
            "size" => {
                let range = size_range(&lower);
                f.size = range.or(f.size);
                (format!("size: {lower}"), range.is_some())
            }
            // Not a filter, such as part of a URL or a drive letter.
            _ => {
                words.push(token);
                continue;
            }
        };
        parsed.chips.push(chip(label, valid));
    }
    parsed.text = words.join(" ");
    parsed
}

fn chip(label: String, valid: bool) -> Chip {
    Chip { label, valid }
}

/// Splits on whitespace, keeping a double-quoted phrase (quotes included) as one token.
fn tokens(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in query.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                current.push(c);
            }
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn apply_kind(f: &mut Filter, kind: &str) -> bool {
    let exts = match kind {
        "folder" | "folders" | "dir" => {
            f.folders = Some(true);
            return true;
        }
        "file" | "files" => {
            f.folders = Some(false);
            return true;
        }
        "image" | "images" | "picture" | "pictures" | "photo" | "photos" => IMAGE,
        "video" | "videos" | "movie" | "movies" => VIDEO,
        "audio" | "music" | "sound" => AUDIO,
        "doc" | "docs" | "document" | "documents" => DOCUMENT,
        "code" | "source" => CODE,
        "archive" | "archives" | "zip" => ARCHIVE,
        _ => return false,
    };
    f.folders = Some(false);
    f.exts.extend(exts.iter().map(|e| e.to_string()));
    true
}

/// `today` `yesterday` `week` `month` `year`, `3d` `12h` `2w`, a period `2024` `2024-05`
/// `2024-05-17` (local time), `>2024-05` (since) `<2024-05` (before), or `2024-01..2024-03`.
fn date_range(value: &str, now: i64, offset: i64) -> Option<(i64, i64)> {
    const OPEN: i64 = i64::MAX;
    let local_midnight = (now + offset).div_euclid(TICKS_PER_DAY) * TICKS_PER_DAY - offset;
    let ago = |ticks: i64| Some((now - ticks, OPEN));
    match value {
        "today" => return Some((local_midnight, OPEN)),
        "yesterday" => return Some((local_midnight - TICKS_PER_DAY, local_midnight)),
        "week" => return ago(7 * TICKS_PER_DAY),
        "month" => return ago(30 * TICKS_PER_DAY),
        "year" => return ago(365 * TICKS_PER_DAY),
        _ => {}
    }
    if let Some(n) = value.strip_suffix('h').and_then(|n| n.parse::<i64>().ok()) {
        return ago(n * 3600 * TICKS_PER_SECOND);
    }
    if let Some(n) = value.strip_suffix('d').and_then(|n| n.parse::<i64>().ok()) {
        return ago(n * TICKS_PER_DAY);
    }
    if let Some(n) = value.strip_suffix('w').and_then(|n| n.parse::<i64>().ok()) {
        return ago(n * 7 * TICKS_PER_DAY);
    }
    if let Some(since) = value.strip_prefix('>') {
        return Some((period(since, offset)?.0, OPEN));
    }
    if let Some(before) = value.strip_prefix('<') {
        return Some((i64::MIN, period(before, offset)?.0));
    }
    if let Some((from, to)) = value.split_once("..") {
        return Some((period(from, offset)?.0, period(to, offset)?.1));
    }
    period(value, offset)
}

/// `YYYY`, `YYYY-MM` or `YYYY-MM-DD` as a local-time FILETIME range.
fn period(value: &str, offset: i64) -> Option<(i64, i64)> {
    let parts: Vec<i64> = value
        .split('-')
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    let (start, end) = match parts[..] {
        [y] if (1601..=9999).contains(&y) => (days(y, 1, 1), days(y + 1, 1, 1)),
        [y, m] if (1..=12).contains(&m) => {
            let next = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
            (days(y, m, 1), days(next.0, next.1, 1))
        }
        [y, m, d] if (1..=12).contains(&m) && (1..=31).contains(&d) => {
            (days(y, m, d), days(y, m, d) + 1)
        }
        _ => return None,
    };
    let at = |day: i64| FILETIME_UNIX_EPOCH + day * TICKS_PER_DAY - offset;
    Some((at(start), at(end)))
}

/// Days from 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's days_from_civil).
fn days(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `>10mb` `>=1gb` `<100kb` `<=5mb` `1mb..10mb` `empty`, or a bare size meaning at least that.
fn size_range(value: &str) -> Option<(u64, u64)> {
    if value == "empty" {
        return Some((0, 0));
    }
    if let Some((from, to)) = value.split_once("..") {
        return Some((bytes(from)?, bytes(to)?));
    }
    if let Some(n) = value.strip_prefix(">=") {
        return Some((bytes(n)?, u64::MAX));
    }
    if let Some(n) = value.strip_prefix("<=") {
        return Some((0, bytes(n)?));
    }
    if let Some(n) = value.strip_prefix('>') {
        return Some((bytes(n)?.saturating_add(1), u64::MAX));
    }
    if let Some(n) = value.strip_prefix('<') {
        return Some((0, bytes(n)?.checked_sub(1)?));
    }
    Some((bytes(value)?, u64::MAX))
}

/// `512`, `10kb`, `1.5mb`, `2g`: binary units.
fn bytes(value: &str) -> Option<u64> {
    let split = value
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(value.len());
    let (number, unit) = value.split_at(split);
    let number: f64 = number.parse().ok()?;
    let scale: f64 = match unit {
        "" | "b" => 1.0,
        "k" | "kb" => 1024.0,
        "m" | "mb" => 1024.0 * 1024.0,
        "g" | "gb" => 1024.0 * 1024.0 * 1024.0,
        "t" | "tb" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((number * scale) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-21 18:00 UTC, an hour east of UTC.
    const NOW: i64 = FILETIME_UNIX_EPOCH + 20_747 * TICKS_PER_DAY + 18 * 3600 * TICKS_PER_SECOND;
    const OFFSET: i64 = 3600 * TICKS_PER_SECOND;

    fn at(query: &str) -> Parsed {
        parse_at(query, NOW, OFFSET)
    }

    #[test]
    fn splits_filters_from_name_words() {
        let p = at(r#"report ext:PDF,.docx in:C:\Work\ -draft "q3 final" https://x.com"#);
        assert_eq!(p.text, r"report !draft 'q3\ final https://x.com");
        assert_eq!(p.filter.exts, ["pdf", "docx"]);
        assert_eq!(p.filter.within, [r"c:\work"]);
        let labels: Vec<&str> = p.chips.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "ext: pdf, docx",
                r"in: C:\Work\",
                "not: draft",
                "exact: q3 final"
            ]
        );
        assert!(at("plain words").filter.is_empty());
    }

    #[test]
    fn kinds_and_folders() {
        assert_eq!(at("folder:").filter.folders, Some(true));
        assert_eq!(at("kind:file").filter.folders, Some(false));
        let image = at("kind:image").filter;
        assert!(image.exts.contains(&"png".to_string()) && image.folders == Some(false));
        assert_eq!(at("folder:music").filter.within, ["music"]);
        assert!(!at("kind:banana").chips[0].valid);
    }

    #[test]
    fn dates_are_local_days_and_periods() {
        let local_midnight = FILETIME_UNIX_EPOCH + 20_747 * TICKS_PER_DAY - OFFSET;
        assert_eq!(
            at("dm:today").filter.modified,
            Some((local_midnight, i64::MAX))
        );
        assert_eq!(
            at("modified:yesterday").filter.modified,
            Some((local_midnight - TICKS_PER_DAY, local_midnight))
        );
        assert_eq!(
            at("dm:3d").filter.modified,
            Some((NOW - 3 * TICKS_PER_DAY, i64::MAX))
        );
        // 2026-10-21 is day 20,747.
        assert_eq!(
            at("dm:2026-10-21").filter.modified,
            Some((local_midnight, local_midnight + TICKS_PER_DAY))
        );
        let (from, to) = at("dm:2024").filter.modified.unwrap();
        assert_eq!((to - from) / TICKS_PER_DAY, 366, "a leap year");
        let (from, to) = at("dm:2026-01..2026-02").filter.modified.unwrap();
        assert_eq!((to - from) / TICKS_PER_DAY, 59);
        assert_eq!(at("dm:<2026").filter.modified.unwrap().0, i64::MIN);
        assert!(!at("dm:soon").chips[0].valid && at("dm:soon").filter.modified.is_none());
    }

    #[test]
    fn sizes() {
        let size = |q: &str| at(q).filter.size;
        assert_eq!(size("size:>10mb"), Some((10 * 1024 * 1024 + 1, u64::MAX)));
        assert_eq!(size("size:<1kb"), Some((0, 1023)));
        assert_eq!(size("size:1.5k"), Some((1536, u64::MAX)));
        assert_eq!(size("size:1mb..2mb"), Some((1 << 20, 2 << 20)));
        assert_eq!(size("size:empty"), Some((0, 0)));
        assert_eq!(size("size:lots"), None);
    }

    #[test]
    fn drive_paths_select_one_index() {
        let f = at(r"in:c:\work in:music").filter;
        assert_eq!(f.on_drive('C').unwrap().within, [r"\work", "music"]);
        assert!(f.on_drive('D').is_none());
        assert_eq!(at(r"in:c:").filter.on_drive('C').unwrap().within, [r"\"]);
    }
}
