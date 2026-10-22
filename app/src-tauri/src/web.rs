//! The web: shortcuts ("g tauri", "yt lofi", and any added under `[shortcuts]` in config.toml), a
//! "Search the web" row after every other result, and the bookmarks and history of Brave, Chrome
//! and Edge across their profiles. Browser data is read when the panel opens, at most every five
//! minutes, from copies (the browsers keep their History databases locked), and kept in memory.

use std::{
    collections::{BTreeMap, HashMap},
    fs, io,
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::query::{Action, Item};

const OPEN: Action = Action {
    id: "open",
    label: "Open",
};
const COPY: Action = Action {
    id: "copy",
    label: "Copy URL",
};
const REFRESH_EVERY: Duration = Duration::from_secs(5 * 60);
const HISTORY_ROWS: usize = 5_000;
const HISTORY_SHOWN: usize = 5;
/// Chrome counts microseconds from 1601; Unix seconds start 11,644,473,600 s later.
const CHROME_EPOCH_OFFSET: i64 = 11_644_473_600;
/// (key, name, URL with %s for the words)
const DEFAULT_SHORTCUTS: &[(&str, &str, &str)] = &[
    ("g", "Google", "https://www.google.com/search?q=%s"),
    ("ddg", "DuckDuckGo", "https://duckduckgo.com/?q=%s"),
    ("b", "Bing", "https://www.bing.com/search?q=%s"),
    (
        "yt",
        "YouTube",
        "https://www.youtube.com/results?search_query=%s",
    ),
    ("gh", "GitHub", "https://github.com/search?q=%s"),
    (
        "wiki",
        "Wikipedia",
        "https://en.wikipedia.org/w/index.php?search=%s",
    ),
    (
        "maps",
        "Google Maps",
        "https://www.google.com/maps/search/%s",
    ),
    ("amz", "Amazon", "https://www.amazon.in/s?k=%s"),
    (
        "so",
        "Stack Overflow",
        "https://stackoverflow.com/search?q=%s",
    ),
    ("r", "Reddit", "https://www.reddit.com/search/?q=%s"),
    ("npm", "npm", "https://www.npmjs.com/search?q=%s"),
    ("crates", "crates.io", "https://crates.io/search?q=%s"),
    (
        "mdn",
        "MDN",
        "https://developer.mozilla.org/en-US/search?q=%s",
    ),
    ("imdb", "IMDb", "https://www.imdb.com/find/?q=%s"),
    (
        "tr",
        "Google Translate",
        "https://translate.google.com/?text=%s",
    ),
];
/// (name, folder under %LOCALAPPDATA% holding its profiles)
const BROWSERS: &[(&str, &str)] = &[
    ("Brave", r"BraveSoftware\Brave-Browser\User Data"),
    ("Chrome", r"Google\Chrome\User Data"),
    ("Edge", r"Microsoft\Edge\User Data"),
];

struct Shortcut {
    key: String,
    name: String,
    url: String,
}

#[derive(Clone, Debug, PartialEq)]
struct Page {
    title: String,
    url: String,
    browser: &'static str,
    visits: i64,
    /// Unix seconds of the last visit; 0 for a bookmark.
    last: i64,
}

#[derive(Default)]
struct Pages {
    bookmarks: Vec<Page>,
    history: Vec<Page>,
    read: Option<Instant>,
}

pub struct Web {
    shortcuts: Vec<Shortcut>,
    /// The shortcut the "Search the web" row uses.
    search: String,
    pages: Mutex<Pages>,
    reading: AtomicBool,
}

impl Web {
    /// The built-in shortcuts with the user's added or replaced ones.
    pub fn new(extra: &BTreeMap<String, String>, search: &str) -> Self {
        let mut shortcuts: Vec<Shortcut> = DEFAULT_SHORTCUTS
            .iter()
            .filter(|(key, ..)| !extra.contains_key(*key))
            .map(|&(key, name, url)| Shortcut {
                key: key.into(),
                name: name.into(),
                url: url.into(),
            })
            .collect();
        for (key, url) in extra {
            let host = url
                .split('/')
                .nth(2)
                .unwrap_or(url)
                .trim_start_matches("www.");
            shortcuts.push(Shortcut {
                key: key.to_lowercase(),
                name: host.to_string(),
                url: url.clone(),
            });
        }
        let search = if shortcuts.iter().any(|s| s.key == search) {
            search.to_string()
        } else {
            "g".to_string()
        };
        Self {
            shortcuts,
            search,
            pages: Mutex::default(),
            reading: AtomicBool::new(false),
        }
    }

    fn shortcut(&self, key: &str) -> Option<&Shortcut> {
        self.shortcuts.iter().find(|s| s.key == key)
    }

    /// "g rust tauri": the search it names, as the first result.
    pub fn shortcut_items(&self, text: &str) -> Vec<Item> {
        let Some((key, words)) = text.trim_start().split_once(' ') else {
            return Vec::new();
        };
        let words = words.trim();
        match self.shortcut(&key.to_lowercase()) {
            Some(s) if !words.is_empty() => vec![search_item(s, words)],
            _ => Vec::new(),
        }
    }

    /// The last row of every query: the words on the default search engine.
    pub fn fallback(&self, text: &str) -> Option<Item> {
        let words = text.trim();
        if words.is_empty() {
            return None;
        }
        let s = self.shortcut(&self.search)?;
        let mut item = search_item(s, words);
        item.title = format!("Search the web for “{words}”");
        Some(item)
    }

    /// Bookmarks, for ranking with apps by name.
    pub fn bookmarks(&self) -> Vec<Item> {
        let pages = self.pages.lock().unwrap();
        pages
            .bookmarks
            .iter()
            .map(|p| page_item(p, "Bookmark"))
            .collect()
    }

    /// The pages visited most and most recently whose title or address has every word,
    /// leaving out bookmarks, which rank with the apps.
    pub fn history(&self, text: &str) -> Vec<Item> {
        let words: Vec<String> = text.split_whitespace().map(str::to_lowercase).collect();
        if text.trim().chars().count() < 2 {
            return Vec::new();
        }
        let pages = self.pages.lock().unwrap();
        let now = now();
        let mut found: Vec<(f64, &Page)> = pages
            .history
            .iter()
            .filter(|p| {
                let title = p.title.to_lowercase();
                let url = p.url.to_lowercase();
                words
                    .iter()
                    .all(|w| title.contains(w.as_str()) || url.contains(w.as_str()))
            })
            .map(|p| (frecency(p, now), p))
            .collect();
        found.sort_by(|a, b| b.0.total_cmp(&a.0));
        found
            .into_iter()
            .take(HISTORY_SHOWN)
            .map(|(_, p)| page_item(p, "History"))
            .collect()
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Visits count, but fade with a two-week half-life since the last one.
fn frecency(page: &Page, now: i64) -> f64 {
    let days = (now - page.last).max(0) as f64 / 86_400.0;
    (page.visits as f64 + 1.0).ln() * 0.5f64.powf(days / 14.0)
}

fn search_item(s: &Shortcut, words: &str) -> Item {
    let url = s.url.replace("%s", &encode(words));
    Item {
        id: format!("url:{url}"),
        title: format!("Search {} for “{words}”", s.name),
        subtitle: format!("{} · {}", s.key, s.name),
        preview: url,
        icon: None,
        line: None,
        actions: vec![OPEN, COPY],
    }
}

fn page_item(page: &Page, kind: &str) -> Item {
    let host = page.url.split('/').nth(2).unwrap_or("");
    Item {
        id: format!("url:{}", page.url),
        title: page.title.clone(),
        subtitle: format!("{kind} · {} · {host}", page.browser),
        preview: page.url.clone(),
        icon: None,
        line: None,
        actions: vec![OPEN, COPY],
    }
}

/// Percent-encoding for a query string value, with spaces as "+".
fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Reads browser bookmarks and history in the background, unless they were read recently.
pub fn refresh(app: &AppHandle) {
    let Some(web) = app.try_state::<Web>() else {
        return;
    };
    let fresh = web
        .pages
        .lock()
        .unwrap()
        .read
        .is_some_and(|t| t.elapsed() < REFRESH_EVERY);
    if fresh || web.reading.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let web = app.state::<Web>();
        let start = Instant::now();
        let (bookmarks, history) = read_browsers();
        tracing::info!(
            "read {} bookmarks and {} history entries in {:?}",
            bookmarks.len(),
            history.len(),
            start.elapsed()
        );
        *web.pages.lock().unwrap() = Pages {
            bookmarks,
            history,
            read: Some(Instant::now()),
        };
        web.reading.store(false, Ordering::SeqCst);
    });
}

fn read_browsers() -> (Vec<Page>, Vec<Page>) {
    let local = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_default();
    let mut bookmarks = Vec::new();
    let mut history: HashMap<String, Page> = HashMap::new();
    for &(browser, folder) in BROWSERS {
        for profile in profiles(&local.join(folder)) {
            if let Ok(text) = fs::read_to_string(profile.join("Bookmarks")) {
                bookmarks.extend(parse_bookmarks(&text, browser));
            }
            match read_history(&profile.join("History"), browser) {
                Ok(pages) => {
                    for page in pages {
                        // The same page in several browsers or profiles counts once, at its best.
                        match history.get_mut(&page.url) {
                            Some(seen) if seen.visits >= page.visits => {}
                            _ => {
                                history.insert(page.url.clone(), page);
                            }
                        }
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!("{browser} history unreadable: {e}"),
            }
        }
    }
    bookmarks.sort_by(|a, b| a.url.cmp(&b.url));
    bookmarks.dedup_by(|a, b| a.url == b.url);
    for b in &bookmarks {
        history.remove(&b.url);
    }
    (bookmarks, history.into_values().collect())
}

/// "Default", "Profile 1" and the like: folders with a Bookmarks or History file.
fn profiles(user_data: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(user_data) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("Bookmarks").exists() || p.join("History").exists())
        .collect()
}

/// Every URL bookmark in a Chromium `Bookmarks` file, at any depth.
fn parse_bookmarks(text: &str, browser: &'static str) -> Vec<Page> {
    fn walk(node: &Value, browser: &'static str, out: &mut Vec<Page>) {
        if node["type"] == "url"
            && let (Some(title), Some(url)) = (node["name"].as_str(), node["url"].as_str())
            && url.starts_with("http")
        {
            out.push(Page {
                title: if title.is_empty() { url } else { title }.to_string(),
                url: url.to_string(),
                browser,
                visits: 0,
                last: 0,
            });
        }
        if let Some(children) = node["children"].as_array() {
            children.iter().for_each(|c| walk(c, browser, out));
        }
    }
    let Ok(root) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(roots) = root["roots"].as_object() {
        roots.values().for_each(|r| walk(r, browser, &mut out));
    }
    out
}

/// Opens a file the browser holds open, letting it keep writing and even deleting.
fn copy_shared(from: &Path, to: &Path) -> io::Result<()> {
    const SHARE_ALL: u32 = 0x1 | 0x2 | 0x4; // read, write, delete
    let mut source = fs::OpenOptions::new()
        .read(true)
        .share_mode(SHARE_ALL)
        .open(from)?;
    let mut target = fs::File::create(to)?;
    io::copy(&mut source, &mut target)?;
    Ok(())
}

/// The recent pages of one History database, read from a copy (with its write-ahead log, which
/// holds the latest visits) that is deleted afterwards.
fn read_history(path: &Path, browser: &'static str) -> io::Result<Vec<Page>> {
    if !path.exists() {
        return Err(io::ErrorKind::NotFound.into());
    }
    let dir = std::env::temp_dir().join(format!("seekr-history-{}", std::process::id()));
    fs::create_dir_all(&dir)?;
    let copy = dir.join("History");
    let result = (|| {
        copy_shared(path, &copy)?;
        for suffix in ["-wal", "-journal"] {
            let side = path.with_file_name(format!("History{suffix}"));
            if side.exists() {
                copy_shared(&side, &dir.join(format!("History{suffix}")))?;
            }
        }
        let db = rusqlite::Connection::open(&copy).map_err(io::Error::other)?;
        let mut statement = db
            .prepare(
                "SELECT url, title, visit_count, last_visit_time FROM urls
                 WHERE hidden = 0 AND title != '' AND url LIKE 'http%'
                 ORDER BY last_visit_time DESC LIMIT ?1",
            )
            .map_err(io::Error::other)?;
        statement
            .query_map([HISTORY_ROWS as i64], |r| {
                Ok(Page {
                    url: r.get(0)?,
                    title: r.get(1)?,
                    visits: r.get(2)?,
                    last: r.get::<_, i64>(3)? / 1_000_000 - CHROME_EPOCH_OFFSET,
                    browser,
                })
            })
            .map_err(io::Error::other)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(io::Error::other)
    })();
    let _ = fs::remove_dir_all(&dir);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_and_fallback() {
        let mut extra = BTreeMap::new();
        extra.insert(
            "jira".to_string(),
            "https://acme.atlassian.net/browse/%s".to_string(),
        );
        extra.insert(
            "g".to_string(),
            "https://www.google.co.in/search?q=%s".to_string(),
        );
        let web = Web::new(&extra, "ddg");
        let g = &web.shortcut_items("g rust & tauri")[0];
        assert_eq!(g.id, "url:https://www.google.co.in/search?q=rust+%26+tauri");
        assert_eq!(
            web.shortcut_items("jira SEEK-12")[0].title,
            "Search acme.atlassian.net for “SEEK-12”"
        );
        assert!(web.shortcut_items("g").is_empty());
        assert!(web.shortcut_items("notepad").is_empty());
        let fallback = web.fallback("best biryani").unwrap();
        assert_eq!(fallback.id, "url:https://duckduckgo.com/?q=best+biryani");
        assert_eq!(fallback.title, "Search the web for “best biryani”");
        assert!(web.fallback("  ").is_none());
        assert_eq!(Web::new(&BTreeMap::new(), "nope").search, "g");
    }

    #[test]
    fn bookmarks_history_and_ranking() {
        let json = r#"{"roots": {"bookmark_bar": {"children": [
            {"type": "url", "name": "Tauri docs", "url": "https://tauri.app/"},
            {"type": "folder", "children": [
                {"type": "url", "name": "", "url": "https://docs.rs/"},
                {"type": "url", "name": "Settings", "url": "chrome://settings"}]}]},
            "other": {"children": []}}}"#;
        let pages = parse_bookmarks(json, "Brave");
        let titles: Vec<&str> = pages.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, ["Tauri docs", "https://docs.rs/"]);

        let web = Web::new(&BTreeMap::new(), "g");
        let now = now();
        let page = |title: &str, url: &str, visits, days: i64| Page {
            title: title.into(),
            url: url.into(),
            browser: "Chrome",
            visits,
            last: now - days * 86_400,
        };
        web.pages.lock().unwrap().history = vec![
            page("Rust book", "https://doc.rust-lang.org/book/", 3, 0),
            page("Rust playground", "https://play.rust-lang.org/", 50, 60),
            page("Cooking", "https://example.com/rust-free", 1, 0),
        ];
        let found = web.history("rust");
        // Visited often but two months ago ranks below visited today.
        assert_eq!(found[0].title, "Rust book");
        assert_eq!(found.len(), 3, "the address counts too");
        assert_eq!(web.history("rust book").len(), 1);
        assert!(web.history("r").is_empty());
    }

    #[test]
    fn copies_a_file_another_handle_holds_open() {
        let dir = std::env::temp_dir().join(format!("seekr-copy-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let from = dir.join("locked");
        let mut held = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .share_mode(0x1 | 0x2 | 0x4)
            .open(&from)
            .unwrap();
        io::Write::write_all(&mut held, b"history").unwrap();
        copy_shared(&from, &dir.join("copy")).unwrap();
        assert_eq!(fs::read(dir.join("copy")).unwrap(), b"history");
        drop(held);
        let _ = fs::remove_dir_all(&dir);
    }
}
