//! Ranking: fuzzy match quality (word starts and acronyms score highest), plus how often and how
//! recently each result was chosen for the typed prefix, with pinned results on top. An empty
//! query lists pins, then recently used results.

use std::{
    collections::HashMap,
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};
use rusqlite::{Connection, params};

use crate::query::{Action, Item};

/// A use counts half as much after this long.
const HALF_LIFE_SECS: f64 = 14.0 * 24.0 * 3600.0;
/// Fuzzy score points per decayed use; nucleo scores a good 4-letter match around 100-150.
const POINTS_PER_USE: f64 = 40.0;
const MAX_USES: f64 = 10.0;
const PIN_BONUS: f64 = 100_000.0;
const MAX_RESULTS: usize = 50;
/// Usage is recorded under each prefix of the query up to this many characters.
const MAX_PREFIX_CHARS: usize = 16;
const HOME_LIMIT: usize = 9;

const PIN: Action = Action {
    id: "pin",
    label: "Pin to top",
};
const UNPIN: Action = Action {
    id: "unpin",
    label: "Unpin",
};

pub struct Ranker {
    matcher: Mutex<Matcher>,
    db: Mutex<Connection>,
}

impl Ranker {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        Self::with(Connection::open(path)?)
    }

    pub fn in_memory() -> rusqlite::Result<Self> {
        Self::with(Connection::open_in_memory()?)
    }

    fn with(db: Connection) -> rusqlite::Result<Self> {
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS usage (
                 prefix TEXT NOT NULL, item TEXT NOT NULL,
                 score REAL NOT NULL, last_used INTEGER NOT NULL,
                 PRIMARY KEY (prefix, item));
             CREATE TABLE IF NOT EXISTS pins (item TEXT PRIMARY KEY, pinned_at INTEGER NOT NULL);",
        )?;
        let mut config = Config::DEFAULT;
        config.prefer_prefix = true;
        Ok(Self {
            matcher: Mutex::new(Matcher::new(config)),
            db: Mutex::new(db),
        })
    }

    pub fn rank(&self, query: &str, items: Vec<Item>) -> Vec<Item> {
        let query = normalize(query);
        let pins = self.pins().unwrap_or_else(log_db_error);
        if query.is_empty() {
            return self.home(items, &pins);
        }
        let uses = self.uses(&query).unwrap_or_else(log_db_error);
        let pattern = Pattern::parse(&query, CaseMatching::Ignore, Normalization::Smart);
        let mut matcher = self.matcher.lock().unwrap();
        let mut buf = Vec::new();
        let mut scored: Vec<(f64, Item)> = items
            .into_iter()
            .filter_map(|item| {
                let fuzzy = pattern.score(Utf32Str::new(&item.title, &mut buf), &mut matcher)?;
                let used = uses.get(&item.id).copied().unwrap_or(0.0).min(MAX_USES);
                let pinned = if pins.contains_key(&item.id) {
                    PIN_BONUS
                } else {
                    0.0
                };
                Some((f64::from(fuzzy) + POINTS_PER_USE * used + pinned, item))
            })
            .collect();
        scored.sort_by(|(a, x), (b, y)| {
            b.total_cmp(a)
                .then(x.title.len().cmp(&y.title.len()))
                .then_with(|| x.title.cmp(&y.title))
        });
        scored
            .into_iter()
            .take(MAX_RESULTS)
            .map(|(_, item)| with_pin_action(item, &pins))
            .collect()
    }

    /// Pins in the order they were pinned, then the most recently used results.
    fn home(&self, items: Vec<Item>, pins: &HashMap<String, i64>) -> Vec<Item> {
        let recent = self.recent().unwrap_or_else(log_db_error);
        let mut by_id: HashMap<String, Item> =
            items.into_iter().map(|i| (i.id.clone(), i)).collect();
        let mut pinned: Vec<(&String, &i64)> = pins.iter().collect();
        pinned.sort_by_key(|(_, at)| **at);
        pinned
            .into_iter()
            .map(|(id, _)| id.clone())
            .chain(recent)
            .filter_map(|id| by_id.remove(&id))
            .take(HOME_LIMIT)
            .map(|item| with_pin_action(item, pins))
            .collect()
    }

    /// Credits a chosen result to every prefix of the query, and to "" for the recents list.
    pub fn record(&self, query: &str, item: &str) -> rusqlite::Result<()> {
        let now = unix_now();
        let query = normalize(query);
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        for prefix in prefixes(&query) {
            let previous: Option<(f64, i64)> = tx
                .query_row(
                    "SELECT score, last_used FROM usage WHERE prefix = ?1 AND item = ?2",
                    params![prefix, item],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .ok();
            let score = previous.map_or(0.0, |(s, at)| decayed(s, at, now)) + 1.0;
            tx.execute(
                "INSERT OR REPLACE INTO usage (prefix, item, score, last_used) VALUES (?1, ?2, ?3, ?4)",
                params![prefix, item, score, now],
            )?;
        }
        tx.commit()
    }

    /// Returns whether the item is pinned afterwards.
    pub fn toggle_pin(&self, item: &str) -> rusqlite::Result<bool> {
        let db = self.db.lock().unwrap();
        if db.execute("DELETE FROM pins WHERE item = ?1", [item])? > 0 {
            return Ok(false);
        }
        db.execute(
            "INSERT INTO pins (item, pinned_at) VALUES (?1, ?2)",
            params![item, unix_now()],
        )?;
        Ok(true)
    }

    fn pins(&self) -> rusqlite::Result<HashMap<String, i64>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare_cached("SELECT item, pinned_at FROM pins")?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect()
    }

    fn uses(&self, prefix: &str) -> rusqlite::Result<HashMap<String, f64>> {
        let now = unix_now();
        let db = self.db.lock().unwrap();
        let mut stmt =
            db.prepare_cached("SELECT item, score, last_used FROM usage WHERE prefix = ?1")?;
        stmt.query_map([truncate(prefix)], |r| {
            Ok((r.get::<_, String>(0)?, decayed(r.get(1)?, r.get(2)?, now)))
        })?
        .collect()
    }

    fn recent(&self) -> rusqlite::Result<Vec<String>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare_cached(
            "SELECT item FROM usage WHERE prefix = '' ORDER BY last_used DESC, rowid DESC LIMIT 50",
        )?;
        stmt.query_map([], |r| r.get(0))?.collect()
    }
}

fn with_pin_action(mut item: Item, pins: &HashMap<String, i64>) -> Item {
    item.actions.push(if pins.contains_key(&item.id) {
        UNPIN
    } else {
        PIN
    });
    item
}

fn log_db_error<T: Default>(e: rusqlite::Error) -> T {
    tracing::error!("ranking database: {e}");
    T::default()
}

fn normalize(query: &str) -> String {
    query.trim().to_lowercase()
}

fn truncate(query: &str) -> &str {
    query
        .char_indices()
        .nth(MAX_PREFIX_CHARS)
        .map_or(query, |(i, _)| &query[..i])
}

/// "", then every prefix of `query` on a char boundary, up to MAX_PREFIX_CHARS characters.
fn prefixes(query: &str) -> impl Iterator<Item = &str> {
    let query = truncate(query);
    std::iter::once("").chain(
        query
            .char_indices()
            .skip(1)
            .map(move |(i, _)| &query[..i])
            .chain((!query.is_empty()).then_some(query)),
    )
}

fn decayed(score: f64, last_used: i64, now: i64) -> f64 {
    let age = (now - last_used).max(0) as f64;
    score * 0.5f64.powf(age / HALF_LIFE_SECS)
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(title: &str) -> Item {
        Item {
            id: title.to_lowercase(),
            title: title.into(),
            subtitle: String::new(),
            preview: String::new(),
            icon: None,
            actions: Vec::new(),
        }
    }

    fn titles(ranker: &Ranker, query: &str, names: &[&str]) -> Vec<String> {
        let items = names.iter().map(|n| item(n)).collect();
        ranker
            .rank(query, items)
            .into_iter()
            .map(|i| i.title)
            .collect()
    }

    #[test]
    fn acronyms_and_word_starts_win() {
        let r = Ranker::in_memory().unwrap();
        let ranked = titles(&r, "vsc", &["Avast Secure Cleanup", "Visual Studio Code"]);
        assert_eq!(ranked[0], "Visual Studio Code");
        let ranked = titles(&r, "term", &["Microsoft Intermediate", "Terminal"]);
        assert_eq!(ranked[0], "Terminal");
    }

    #[test]
    fn non_matches_are_dropped() {
        let r = Ranker::in_memory().unwrap();
        assert!(titles(&r, "zzz", &["Notepad", "Paint"]).is_empty());
    }

    #[test]
    fn chosen_results_rise_for_that_prefix() {
        let r = Ranker::in_memory().unwrap();
        let names = ["Notepad", "Notepad++"];
        assert_eq!(titles(&r, "no", &names)[0], "Notepad");
        for _ in 0..3 {
            r.record("note", "notepad++").unwrap();
        }
        assert_eq!(titles(&r, "no", &names)[0], "Notepad++");
    }

    #[test]
    fn pins_lead_and_offer_unpin() {
        let r = Ranker::in_memory().unwrap();
        assert!(r.toggle_pin("paint").unwrap());
        let ranked = r.rank("p", vec![item("Photos"), item("Paint")]);
        assert_eq!(ranked[0].title, "Paint");
        assert_eq!(ranked[0].actions.last().unwrap().id, "unpin");
        assert_eq!(ranked[1].actions.last().unwrap().id, "pin");
        assert!(!r.toggle_pin("paint").unwrap());
    }

    #[test]
    fn empty_query_lists_pins_then_recents() {
        let r = Ranker::in_memory().unwrap();
        r.record("cal", "calculator").unwrap();
        r.record("pho", "photos").unwrap();
        r.toggle_pin("paint").unwrap();
        let all = ["Calculator", "Paint", "Photos", "Terminal"];
        assert_eq!(titles(&r, "  ", &all), ["Paint", "Photos", "Calculator"]);
    }

    #[test]
    fn prefixes_respect_char_boundaries_and_length() {
        assert_eq!(prefixes("née").collect::<Vec<_>>(), ["", "n", "né", "née"]);
        assert_eq!(prefixes("").collect::<Vec<_>>(), [""]);
        assert_eq!(prefixes(&"a".repeat(40)).count(), 1 + MAX_PREFIX_CHARS);
    }

    #[test]
    fn uses_decay_by_half_life() {
        let half = HALF_LIFE_SECS as i64;
        assert!((decayed(4.0, 0, half) - 2.0).abs() < 1e-9);
        assert_eq!(decayed(4.0, 10, 5), 4.0);
    }
}
