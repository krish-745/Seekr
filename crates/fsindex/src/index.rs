//! The app's in-memory file-name index: flat arrays and one name arena, no allocation per entry,
//! a parallel cancellable fuzzy matcher, and a compact on-disk dump for fast restarts.

use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashMap},
    io::{self, Read, Write},
};

use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{AtomKind, CaseMatching, Normalization, Pattern},
};

use crate::usn::Record;

const NONE: u32 = u32::MAX;
const DIR: u8 = 1;
const DELETED: u8 = 2;
const DIRECTORY_ATTRIBUTE: u32 = 0x10;
const USN_REASON_FILE_DELETE: u32 = 0x200;
const USN_REASON_RENAME_OLD_NAME: u32 = 0x1000;
const MAGIC: &[u8; 8] = b"SEEKRIX1";
/// Queries this short match as a substring. Fuzzy "in" matches a third of all names on a drive,
/// costs the most to score, and nobody typing two letters wants them scattered through a name.
const SUBSTRING_UP_TO: usize = 2;
/// Above this many candidates a one-word fuzzy query is ranked with nucleo's greedy matcher. It
/// accepts exactly the same names as the optimal one in a fraction of the time; only the final
/// `limit` are then re-scored optimally for their order.
const GREEDY_ABOVE: usize = 100_000;
/// How many entries a worker scores between checks for a newer keystroke.
const CANCEL_EVERY: usize = 8192;

#[derive(Default)]
pub struct Index {
    pub journal_id: u64,
    /// Every journal record before this USN has been applied.
    pub usn: i64,
    frns: Vec<u64>,
    parents: Vec<u32>,
    flags: Vec<u8>,
    name_starts: Vec<u32>,
    name_lens: Vec<u16>,
    /// UTF-8 names back to back; renames append, and `save` drops the old bytes.
    names: Vec<u8>,
    by_frn: HashMap<u64, u32>,
}

/// A min-heap of (score, shorter name first, entry) keeping each worker's best `limit` matches.
type TopK = BinaryHeap<Reverse<(u32, Reverse<u16>, u32)>>;

pub struct Matches {
    /// The best matches, best first.
    pub top: Vec<Hit>,
    /// Every entry that matched, in index order; feed back as `within` for a longer query.
    pub all: Vec<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub entry: u32,
    pub score: u32,
}

impl Index {
    /// Builds from service records (a snapshot), whose parents may come after their children.
    pub fn from_records(
        journal_id: u64,
        usn: i64,
        records: impl IntoIterator<Item = Record>,
    ) -> Self {
        let mut index = Self {
            journal_id,
            usn,
            ..Self::default()
        };
        let mut parent_frns = Vec::new();
        for r in records {
            parent_frns.push(r.parent);
            index.push(&r, NONE);
        }
        for (i, parent) in parent_frns.into_iter().enumerate() {
            index.parents[i] = index.by_frn.get(&parent).copied().unwrap_or(NONE);
        }
        index
    }

    /// Applies one journal record from the service, in order.
    pub fn apply(&mut self, r: &Record) {
        if r.reason & USN_REASON_RENAME_OLD_NAME != 0 {
            return; // The matching RENAME_NEW_NAME record carries the new name and parent.
        }
        let existing = self.by_frn.get(&r.frn).copied();
        if r.reason & USN_REASON_FILE_DELETE != 0 {
            if let Some(i) = existing {
                self.flags[i as usize] |= DELETED;
                self.by_frn.remove(&r.frn);
            }
            return;
        }
        let parent = self.by_frn.get(&r.parent).copied().unwrap_or(NONE);
        match existing {
            Some(i) if self.name(i) == r.name && self.parents[i as usize] == parent => {}
            Some(i) => {
                let i = i as usize;
                self.parents[i] = parent;
                self.name_starts[i] = self.names.len() as u32;
                self.name_lens[i] = r.name.len() as u16;
                self.names.extend_from_slice(r.name.as_bytes());
            }
            None => self.push(r, parent),
        }
    }

    /// Applies a batch from the no-admin watcher. Unlike the journal, a watcher reports only the
    /// top of a folder that was deleted or moved away, so everything under it goes too.
    pub fn apply_watched(&mut self, records: &[Record]) {
        let mut folder_removed = false;
        for r in records {
            folder_removed |= r.reason & USN_REASON_FILE_DELETE != 0
                && self.by_frn.get(&r.frn).is_some_and(|&e| self.is_dir(e));
            self.apply(r);
        }
        if folder_removed {
            self.remove_orphans();
        }
    }

    /// Deletes every entry with a deleted ancestor, in one pass over the index.
    fn remove_orphans(&mut self) {
        const UNKNOWN: u8 = 0;
        const LIVE: u8 = 1;
        const DEAD: u8 = 2;
        let mut state = vec![UNKNOWN; self.frns.len()];
        let mut chain = Vec::new();
        for start in 0..self.frns.len() as u32 {
            let mut at = start;
            let verdict = loop {
                if at == NONE {
                    break LIVE;
                }
                let i = at as usize;
                if state[i] != UNKNOWN {
                    break state[i];
                }
                if self.flags[i] & DELETED != 0 {
                    break DEAD;
                }
                chain.push(at);
                at = self.parents[i];
                if chain.len() > 512 {
                    break LIVE; // A parent loop; leave it alone.
                }
            };
            for i in chain.drain(..) {
                let i = i as usize;
                state[i] = verdict;
                if verdict == DEAD && self.flags[i] & DELETED == 0 {
                    self.flags[i] |= DELETED;
                    if self.by_frn.get(&self.frns[i]) == Some(&(i as u32)) {
                        self.by_frn.remove(&self.frns[i]);
                    }
                }
            }
        }
    }

    fn push(&mut self, r: &Record, parent: u32) {
        let i = self.frns.len() as u32;
        self.frns.push(r.frn);
        self.parents.push(parent);
        self.flags.push(if r.attributes & DIRECTORY_ATTRIBUTE != 0 {
            DIR
        } else {
            0
        });
        self.name_starts.push(self.names.len() as u32);
        self.name_lens
            .push(r.name.len().min(u16::MAX as usize) as u16);
        self.names.extend_from_slice(r.name.as_bytes());
        self.by_frn.insert(r.frn, i);
    }

    pub fn len(&self) -> usize {
        self.by_frn.len()
    }

    pub fn contains(&self, frn: u64) -> bool {
        self.by_frn.contains_key(&frn)
    }

    pub fn is_empty(&self) -> bool {
        self.by_frn.is_empty()
    }

    pub fn name(&self, entry: u32) -> &str {
        let i = entry as usize;
        let start = self.name_starts[i] as usize;
        let bytes = &self.names[start..start + self.name_lens[i] as usize];
        // Only whole `&str`s are ever appended to the arena.
        std::str::from_utf8(bytes).unwrap_or("")
    }

    pub fn is_dir(&self, entry: u32) -> bool {
        self.flags[entry as usize] & DIR != 0
    }

    /// The full path on `drive`, built by walking parents.
    pub fn path(&self, drive: char, entry: u32) -> String {
        let mut parts = Vec::new();
        let mut at = entry;
        while at != NONE && parts.len() < 512 {
            parts.push(self.name(at));
            at = self.parents[at as usize];
        }
        parts.reverse();
        format!("{drive}:\\{}", parts.join("\\"))
    }

    /// The `limit` best matches of `query` by name, scored on every core, plus every entry that
    /// matched at all. Pass a previous query's `all` as `within` when `can_narrow(previous, query)`:
    /// anything matching the longer query matched the shorter one, so only those are scored.
    /// Returns None as soon as `cancelled` says a newer query has started.
    pub fn search(
        &self,
        query: &str,
        within: Option<&[u32]>,
        limit: usize,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Option<Matches> {
        let pattern = if uses_substring(query) {
            Pattern::new(
                query,
                CaseMatching::Ignore,
                Normalization::Smart,
                AtomKind::Substring,
            )
        } else {
            Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart)
        };
        let everything: Vec<u32>;
        let candidates = match within {
            Some(c) => c,
            None => {
                everything = (0..self.frns.len() as u32).collect();
                &everything
            }
        };
        let one_word = !query
            .trim()
            .contains(|c: char| c.is_whitespace() || "^'!$\\".contains(c));
        let greedy = (!uses_substring(query) && one_word && candidates.len() > GREEDY_ABOVE)
            .then(|| query.trim().to_lowercase());
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
        let chunk = candidates.len().div_ceil(threads).max(1);
        let parts: Vec<Option<(TopK, Vec<u32>)>> = std::thread::scope(|s| {
            let workers: Vec<_> = candidates
                .chunks(chunk)
                .map(|part| {
                    let (pattern, greedy) = (&pattern, greedy.as_deref());
                    s.spawn(move || self.search_part(pattern, greedy, part, limit, cancelled))
                })
                .collect();
            workers
                .into_iter()
                .map(|w| w.join().unwrap_or(None))
                .collect()
        });
        let mut best = Vec::new();
        let mut all = Vec::new();
        for part in parts {
            let (top, matched) = part?;
            best.extend(top.into_iter().map(|Reverse(t)| t));
            all.extend(matched);
        }
        // Best score first, then shorter names, then index order for stable results.
        let order = |a: &(u32, Reverse<u16>, u32), b: &(u32, Reverse<u16>, u32)| {
            b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2))
        };
        best.sort_unstable_by(order);
        best.truncate(limit);
        if greedy.is_some() {
            let mut matcher = matcher();
            let mut buf = Vec::new();
            for hit in &mut best {
                let name = Utf32Str::new(self.name(hit.2), &mut buf);
                hit.0 = pattern.score(name, &mut matcher).unwrap_or(hit.0);
            }
            best.sort_unstable_by(order);
        }
        let top = best
            .into_iter()
            .map(|(score, _, entry)| Hit { entry, score })
            .collect();
        Some(Matches { top, all })
    }

    fn search_part(
        &self,
        pattern: &Pattern,
        greedy: Option<&str>,
        part: &[u32],
        limit: usize,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Option<(TopK, Vec<u32>)> {
        use windows::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
        };
        // Below normal, so the panel painting the first results wins the CPU while every core
        // searches. These threads end with the query, so nothing needs restoring.
        let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL) };
        let mut matcher = matcher();
        let mut buf = Vec::new();
        let mut top = BinaryHeap::with_capacity(limit + 1);
        let mut matched = Vec::new();
        // Utf32Str::new picks the ASCII form when it can; nucleo never matches an ASCII name
        // against a needle in the Unicode form.
        let mut needle_buf = Vec::new();
        let needle = greedy.map(|q| Utf32Str::new(q, &mut needle_buf));
        for (n, &entry) in part.iter().enumerate() {
            if n % CANCEL_EVERY == 0 && cancelled() {
                return None;
            }
            let i = entry as usize;
            if self.flags[i] & DELETED != 0 {
                continue;
            }
            let name = Utf32Str::new(self.name(entry), &mut buf);
            let score = match needle {
                Some(needle) => matcher.fuzzy_match_greedy(name, needle).map(u32::from),
                None => pattern.score(name, &mut matcher),
            };
            let Some(score) = score else {
                continue;
            };
            matched.push(entry);
            top.push(Reverse((score, Reverse(self.name_lens[i]), entry)));
            if top.len() > limit {
                top.pop();
            }
        }
        Some((top, matched))
    }

    /// Writes the live entries (renamed and deleted names are dropped) for a later `load`.
    pub fn save(&self, out: impl Write) -> io::Result<()> {
        // New positions for live entries, so parents can be rewritten after compaction.
        let mut remap = vec![NONE; self.frns.len()];
        let live: Vec<usize> = (0..self.frns.len())
            .filter(|&i| self.flags[i] & DELETED == 0)
            .collect();
        for (new, &old) in live.iter().enumerate() {
            remap[old] = new as u32;
        }
        let mut z = zstd::Encoder::new(out, 1)?;
        z.write_all(MAGIC)?;
        z.write_all(&self.journal_id.to_le_bytes())?;
        z.write_all(&self.usn.to_le_bytes())?;
        z.write_all(&(live.len() as u64).to_le_bytes())?;
        let mut buf = Vec::with_capacity(live.len() * 8);
        live.iter()
            .for_each(|&i| buf.extend(self.frns[i].to_le_bytes()));
        z.write_all(&buf)?;
        buf.clear();
        live.iter().for_each(|&i| {
            let p = self.parents[i];
            buf.extend((if p == NONE { NONE } else { remap[p as usize] }).to_le_bytes());
        });
        z.write_all(&buf)?;
        buf.clear();
        live.iter().for_each(|&i| buf.push(self.flags[i]));
        z.write_all(&buf)?;
        buf.clear();
        live.iter()
            .for_each(|&i| buf.extend(self.name_lens[i].to_le_bytes()));
        z.write_all(&buf)?;
        for &i in &live {
            z.write_all(self.name(i as u32).as_bytes())?;
        }
        z.finish()?.flush()
    }

    pub fn load(input: impl Read) -> io::Result<Self> {
        let mut z = zstd::Decoder::new(input)?;
        let mut magic = [0u8; 8];
        z.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a Seekr index",
            ));
        }
        let journal_id = read_u64(&mut z)?;
        let usn = read_u64(&mut z)? as i64;
        let n = usize::try_from(read_u64(&mut z)?).map_err(|_| invalid())?;
        if n > 100_000_000 {
            return Err(invalid());
        }
        let frns: Vec<u64> = read_array(&mut z, n, u64::from_le_bytes)?;
        let parents: Vec<u32> = read_array(&mut z, n, u32::from_le_bytes)?;
        let flags: Vec<u8> = read_array(&mut z, n, |b: [u8; 1]| b[0])?;
        let name_lens: Vec<u16> = read_array(&mut z, n, u16::from_le_bytes)?;
        let total: usize = name_lens.iter().map(|&l| l as usize).sum();
        let mut names = vec![0u8; total];
        z.read_exact(&mut names)?;
        if std::str::from_utf8(&names).is_err()
            || parents.iter().any(|&p| p != NONE && p as usize >= n)
        {
            return Err(invalid());
        }
        let mut name_starts = Vec::with_capacity(n);
        let mut at = 0u32;
        for &l in &name_lens {
            name_starts.push(at);
            at += u32::from(l);
        }
        let by_frn = frns
            .iter()
            .enumerate()
            .map(|(i, &f)| (f, i as u32))
            .collect();
        Ok(Self {
            journal_id,
            usn,
            frns,
            parents,
            flags,
            name_starts,
            name_lens,
            names,
            by_frn,
        })
    }
}

fn matcher() -> Matcher {
    let mut config = Config::DEFAULT;
    config.prefer_prefix = true;
    // Pattern atoms set this themselves; the greedy path calls the matcher directly.
    config.ignore_case = true;
    Matcher::new(config)
}

/// One or two letters without spaces match as a substring; anything longer is fuzzy.
pub fn uses_substring(query: &str) -> bool {
    let q = query.trim();
    q.chars().count() <= SUBSTRING_UP_TO && !q.contains(char::is_whitespace)
}

/// Whether `previous`'s matches contain every match of `query`, so `search` may look only there.
/// One-letter substring and fuzzy matches are the same set, so they narrow either kind. `!`
/// negates in nucleo's syntax, where a longer query can match more names, not fewer.
pub fn can_narrow(previous: &str, query: &str) -> bool {
    query.starts_with(previous)
        && !query.contains('!')
        && (uses_substring(previous) == uses_substring(query)
            || previous.trim().chars().count() == 1)
}

fn read_u64(r: &mut impl Read) -> io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

fn read_array<const N: usize, T>(
    r: &mut impl Read,
    n: usize,
    parse: fn([u8; N]) -> T,
) -> io::Result<Vec<T>> {
    let mut bytes = vec![0u8; n * N];
    r.read_exact(&mut bytes)?;
    Ok(bytes.as_chunks::<N>().0.iter().map(|c| parse(*c)).collect())
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "corrupt Seekr index")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: u64 = 5;

    fn rec(frn: u64, parent: u64, name: &str, dir: bool) -> Record {
        Record {
            frn,
            parent,
            usn: 0,
            reason: 0,
            attributes: if dir { DIRECTORY_ATTRIBUTE } else { 0x20 },
            name: name.into(),
        }
    }

    /// C:\Users\me\Documents\report.pdf, C:\Users\me\notes.txt, children listed before parents.
    fn sample() -> Index {
        Index::from_records(
            1,
            100,
            [
                rec(13, 12, "report.pdf", false),
                rec(14, 11, "notes.txt", false),
                rec(10, ROOT, "Users", true),
                rec(11, 10, "me", true),
                rec(12, 11, "Documents", true),
            ],
        )
    }

    fn names(index: &Index, query: &str) -> Vec<String> {
        index
            .search(query, None, 10, &|| false)
            .unwrap()
            .top
            .iter()
            .map(|h| index.name(h.entry).to_string())
            .collect()
    }

    fn entry(index: &Index, frn: u64) -> u32 {
        index.by_frn[&frn]
    }

    #[test]
    fn builds_paths_from_out_of_order_records() {
        let index = sample();
        assert_eq!(index.len(), 5);
        assert_eq!(
            index.path('C', entry(&index, 13)),
            r"C:\Users\me\Documents\report.pdf"
        );
        assert!(index.is_dir(entry(&index, 12)));
    }

    #[test]
    fn finds_by_fuzzy_name() {
        let index = sample();
        assert_eq!(names(&index, "rep"), ["report.pdf"]);
        assert_eq!(names(&index, "docs")[0], "Documents");
        assert!(names(&index, "zzz").is_empty());
    }

    #[test]
    fn applies_creates_renames_moves_and_deletes() {
        let mut index = sample();
        let mut created = rec(15, 12, "draft.docx", false);
        created.reason = 0x100;
        index.apply(&created);
        assert_eq!(
            index.path('C', entry(&index, 15)),
            r"C:\Users\me\Documents\draft.docx"
        );

        let mut old = rec(15, 12, "draft.docx", false);
        old.reason = USN_REASON_RENAME_OLD_NAME;
        index.apply(&old);
        let mut renamed = rec(15, 11, "final.docx", false);
        renamed.reason = 0x2000;
        index.apply(&renamed);
        assert_eq!(
            index.path('C', entry(&index, 15)),
            r"C:\Users\me\final.docx"
        );
        assert!(names(&index, "draft").is_empty());

        let mut gone = rec(14, 11, "notes.txt", false);
        gone.reason = USN_REASON_FILE_DELETE;
        index.apply(&gone);
        assert!(names(&index, "notes").is_empty());
        assert_eq!(index.len(), 5);
    }

    #[test]
    fn watched_folder_removal_takes_its_contents() {
        let mut index = sample();
        let mut gone = rec(11, 10, "me", true);
        gone.reason = USN_REASON_FILE_DELETE;
        // The folder comes back under the same id with one file, as after a rename round trip.
        let back = rec(11, 10, "me", true);
        let kept = rec(14, 11, "notes.txt", false);
        index.apply_watched(&[gone.clone(), back, kept]);
        assert_eq!(names(&index, "notes"), ["notes.txt"]);
        assert!(names(&index, "report").is_empty(), "not re-added, so gone");
        assert_eq!(index.len(), 3);

        index.apply_watched(&[gone]);
        assert!(names(&index, "notes").is_empty());
        assert_eq!(index.len(), 1, "only Users is left");
    }

    #[test]
    fn save_and_load_drop_dead_entries_and_keep_paths() {
        let mut index = sample();
        let mut gone = rec(14, 11, "notes.txt", false);
        gone.reason = USN_REASON_FILE_DELETE;
        index.apply(&gone);
        let mut file = Vec::new();
        index.save(&mut file).unwrap();
        let back = Index::load(&file[..]).unwrap();
        assert_eq!((back.journal_id, back.usn, back.len()), (1, 100, 4));
        assert_eq!(
            back.path('C', entry(&back, 13)),
            r"C:\Users\me\Documents\report.pdf"
        );
        assert!(Index::load(&b"garbage"[..]).is_err());
    }

    #[test]
    fn greedy_ranking_finds_mixed_case_names_in_large_sets() {
        let mut records: Vec<Record> = (0..GREEDY_ABOVE as u64 + 1)
            .map(|n| rec(1_000 + n, ROOT, &format!("File{n}.TXT"), false))
            .collect();
        records.push(rec(999, ROOT, "ReadMe.md", false));
        let index = Index::from_records(1, 0, records);
        assert_eq!(names(&index, "readme"), ["ReadMe.md"]);
        assert_eq!(names(&index, "file12345.txt")[0], "File12345.TXT");
    }

    #[test]
    fn cancellation_stops_the_search() {
        let index = sample();
        assert!(index.search("rep", None, 10, &|| true).is_none());
    }

    #[test]
    fn short_queries_match_as_substrings() {
        let index = sample();
        assert_eq!(
            names(&index, "re"),
            ["report.pdf"],
            "r...e scattered in Users is not a match"
        );
        assert!(can_narrow("r", "re") && can_narrow("r", "rep"));
        assert!(
            !can_narrow("re", "rep"),
            "fuzzy rep may match names without the substring re"
        );
        assert!(!can_narrow("rep", "re") && !can_narrow("re", "re!x"));
    }

    #[test]
    fn narrowing_within_previous_matches_gives_the_same_answer() {
        let index = sample();
        let wide = index.search("e", None, 10, &|| false).unwrap();
        let narrow = index.search("rep", Some(&wide.all), 10, &|| false).unwrap();
        let fresh = index.search("rep", None, 10, &|| false).unwrap();
        assert_eq!(narrow.top, fresh.top);
        assert_eq!(narrow.all, fresh.all);
    }
}

/// Timings on the real cache: `SEEKR_INDEX=%LOCALAPPDATA%\Seekr\files-C.idx cargo test -p fsindex
/// --release real_index_timings -- --ignored --nocapture`.
#[cfg(test)]
#[test]
#[ignore]
fn real_index_timings() {
    let path = std::env::var("SEEKR_INDEX").expect("SEEKR_INDEX");
    let index = Index::load(io::BufReader::new(std::fs::File::open(path).unwrap())).unwrap();
    let never = || false;
    for word in ["invoice", "screenshot", "readme"] {
        let mut levels: Vec<(&str, Vec<u32>)> = Vec::new();
        for end in 1..=word.len() {
            let q = &word[..end];
            let within = levels
                .iter()
                .rev()
                .find(|(p, _)| can_narrow(p, q))
                .map(|(_, all)| all.as_slice());
            let t = std::time::Instant::now();
            let m = index.search(q, within, 300, &never).unwrap();
            println!("{q:>12}: {:>8} matches in {:?}", m.all.len(), t.elapsed());
            levels.push((q, m.all));
        }
    }
}
