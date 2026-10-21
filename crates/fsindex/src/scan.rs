//! The no-admin fallback: names from a directory walk, kept current by `ReadDirectoryChangesW`.
//! It covers drives the index service doesn't read (other letters, FAT/exFAT) and C: when the
//! service isn't installed. Entries are keyed by a hash of their lower-case path, since a walk
//! has no file reference numbers.

use std::{
    collections::HashSet,
    io,
    os::windows::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
    sync::{Mutex, atomic::AtomicBool},
};

use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE},
        Storage::FileSystem::{
            CreateFileW, FILE_ACTION_ADDED, FILE_ACTION_REMOVED, FILE_ACTION_RENAMED_NEW_NAME,
            FILE_ACTION_RENAMED_OLD_NAME, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY,
            FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE, GetDriveTypeW, GetLogicalDrives, OPEN_EXISTING,
            ReadDirectoryChangesW,
        },
    },
    core::PCWSTR,
};

use crate::{index::Index, meta, usn::Record};

const DRIVE_FIXED: u32 = 3;
const DIRECTORY_ATTRIBUTE: u32 = 0x10;
const REPARSE_POINT_ATTRIBUTE: u32 = 0x400;
const USN_REASON_FILE_DELETE: u32 = 0x200;
/// Changes the kernel can hold between reads before it reports an overflow (a full rescan).
const WATCH_BUFFER_BYTES: usize = 512 * 1024;

/// Letters of the local fixed drives.
pub fn fixed_drives() -> Vec<char> {
    let mask = unsafe { GetLogicalDrives() };
    ('A'..='Z')
        .enumerate()
        .filter(|&(bit, _)| mask & (1 << bit) != 0)
        .map(|(_, letter)| letter)
        .filter(|&letter| {
            let root: Vec<u16> = format!(r"{letter}:\")
                .encode_utf16()
                .chain(Some(0))
                .collect();
            (unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) }) == DRIVE_FIXED
        })
        .collect()
}

/// The id an entry is stored under: FNV-1a of the lower-case path.
pub fn path_id(path: &Path) -> u64 {
    path.to_string_lossy()
        .to_lowercase()
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        })
}

fn record(path: &Path, attributes: u32, reason: u32) -> Option<Record> {
    Some(Record {
        frn: path_id(path),
        parent: path_id(path.parent()?),
        usn: 0,
        reason,
        attributes,
        name: path.file_name()?.to_string_lossy().into_owned(),
    })
}

/// Every name under `root` (such as `D:\`), from a low-priority parallel walk that never enters
/// reparse points. Folders this user can't list, like other people's profiles, are left out.
pub fn scan(root: &Path, cancel: &AtomicBool) -> io::Result<Index> {
    let records = Mutex::new(Vec::new());
    meta::walk(root, cancel, |listing| {
        let batch: Vec<Record> = listing
            .iter()
            .filter_map(|m| record(&m.path, m.attributes, 0))
            .collect();
        records.lock().unwrap().extend(batch);
    })?;
    Ok(Index::from_records(0, 0, records.into_inner().unwrap()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Added(PathBuf),
    Removed(PathBuf),
    /// Changes were lost; only a rescan can catch up.
    Overflow,
}

/// Reports name changes anywhere under `root` until `on_changes` returns false or the watch
/// fails. Start it before `scan` so nothing between the two is missed.
pub fn watch(root: &Path, mut on_changes: impl FnMut(Vec<Change>) -> bool) -> io::Result<()> {
    let wide: Vec<u16> = root.as_os_str().encode_wide().chain(Some(0)).collect();
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )?
    };
    let _close = Closer(handle);
    // u32s keep the buffer DWORD-aligned, as ReadDirectoryChangesW requires.
    let mut buf = vec![0u32; WATCH_BUFFER_BYTES / 4];
    loop {
        let mut returned = 0u32;
        unsafe {
            ReadDirectoryChangesW(
                handle,
                buf.as_mut_ptr().cast(),
                WATCH_BUFFER_BYTES as u32,
                true,
                FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME,
                Some(&mut returned),
                None,
                None,
            )?;
        }
        let bytes =
            unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), returned as usize) };
        let changes = if returned == 0 {
            vec![Change::Overflow]
        } else {
            parse_notifications(root, bytes)
        };
        if !on_changes(changes) {
            return Ok(());
        }
    }
}

struct Closer(HANDLE);

impl Drop for Closer {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// FILE_NOTIFY_INFORMATION entries: next offset, action, name length in bytes, UTF-16 name.
fn parse_notifications(root: &Path, mut bytes: &[u8]) -> Vec<Change> {
    let u32_at = |b: &[u8], at: usize| {
        b.get(at..at + 4)
            .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
    };
    let mut changes = Vec::new();
    while let (Some(next), Some(action), Some(len)) =
        (u32_at(bytes, 0), u32_at(bytes, 4), u32_at(bytes, 8))
    {
        let Some(name) = bytes.get(12..12 + len as usize) else {
            break;
        };
        let units: Vec<u16> = name
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_le_bytes(c))
            .collect();
        let path = root.join(String::from_utf16_lossy(&units));
        let action = windows::Win32::Storage::FileSystem::FILE_ACTION(action);
        if action == FILE_ACTION_ADDED || action == FILE_ACTION_RENAMED_NEW_NAME {
            changes.push(Change::Added(path));
        } else if action == FILE_ACTION_REMOVED || action == FILE_ACTION_RENAMED_OLD_NAME {
            changes.push(Change::Removed(path));
        }
        if next == 0 {
            break;
        }
        bytes = bytes.get(next as usize..).unwrap_or_default();
    }
    changes
}

/// Index records for a batch of changes. A folder that appears (created, renamed or moved in) is
/// walked, since the watcher doesn't report what it already held. Names under folders that are
/// not in the index, such as other users' profiles, are dropped.
pub fn records(index: &Index, root: &Path, changes: &[Change]) -> Vec<Record> {
    let root_id = path_id(root);
    let mut added = HashSet::new();
    let mut out = Vec::new();
    for change in changes {
        match change {
            Change::Removed(path) => out.extend(record(path, 0, USN_REASON_FILE_DELETE)),
            Change::Added(path) => {
                // Gone again already, as temporary files often are.
                let Ok(attributes) = path.symlink_metadata().map(|m| m.file_attributes()) else {
                    continue;
                };
                let Some(r) = record(path, attributes, 0) else {
                    continue;
                };
                let known =
                    r.parent == root_id || added.contains(&r.parent) || index.contains(r.parent);
                if !known {
                    continue;
                }
                added.insert(r.frn);
                out.push(r);
                let folder = attributes & DIRECTORY_ATTRIBUTE != 0
                    && attributes & REPARSE_POINT_ATTRIBUTE == 0;
                if folder {
                    let inside = Mutex::new(Vec::new());
                    let _ = meta::walk(path, &AtomicBool::new(false), |listing| {
                        let mut inside = inside.lock().unwrap();
                        inside.extend(
                            listing
                                .iter()
                                .filter_map(|m| record(&m.path, m.attributes, 0)),
                        );
                    });
                    let inside = inside.into_inner().unwrap();
                    added.extend(inside.iter().map(|r| r.frn));
                    out.extend(inside);
                }
            }
            Change::Overflow => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, sync::mpsc, time::Duration};

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("seekr-scan-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(r"docs\deep")).unwrap();
        fs::write(root.join(r"docs\deep\report.pdf"), b"").unwrap();
        fs::write(root.join("notes.txt"), b"").unwrap();
        root
    }

    fn found(index: &Index, query: &str, drive_root: &Path) -> Vec<String> {
        index
            .search(query, None, 10, &|| false)
            .unwrap()
            .top
            .iter()
            .map(|h| index.path('X', h.entry))
            .map(|p| p.replacen(r"X:\", &format!(r"{}\", drive_root.display()), 1))
            .collect()
    }

    /// `cargo test -p fsindex --release scans_the_system_drive -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn scans_the_system_drive() {
        let started = std::time::Instant::now();
        let index = scan(Path::new(r"C:\"), &AtomicBool::new(false)).unwrap();
        println!("{} names in {:?}", index.len(), started.elapsed());
        assert!(index.len() > 100_000);
    }

    /// The whole system drive can be watched without admin rights.
    #[test]
    #[ignore]
    fn watches_the_system_drive() {
        let probe = std::env::temp_dir().join(format!("seekr-watch-{}", std::process::id()));
        let wanted = probe.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            fs::write(&probe, b"").unwrap();
            fs::remove_file(&probe).unwrap();
        });
        let mut seen = false;
        watch(Path::new(r"C:\"), |changes| {
            seen |= changes.contains(&Change::Added(wanted.clone()));
            !seen
        })
        .unwrap();
        writer.join().unwrap();
    }

    #[test]
    fn ids_ignore_case() {
        assert_eq!(
            path_id(Path::new(r"C:\Users\Me")),
            path_id(Path::new(r"c:\users\me"))
        );
        assert_ne!(path_id(Path::new(r"C:\a")), path_id(Path::new(r"C:\b")));
    }

    #[test]
    fn parses_notifications() {
        let mut buf = Vec::new();
        for (next, action, name) in [(24u32, 1u32, "a.txt"), (0, 4, "b")] {
            let units: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
            buf.extend(next.to_le_bytes());
            buf.extend(action.to_le_bytes());
            buf.extend((units.len() as u32).to_le_bytes());
            buf.extend(&units);
            buf.resize(buf.len().next_multiple_of(4), 0);
        }
        assert_eq!(
            parse_notifications(Path::new(r"C:\r"), &buf),
            [
                Change::Added(r"C:\r\a.txt".into()),
                Change::Removed(r"C:\r\b".into())
            ]
        );
    }

    #[test]
    fn scans_then_follows_creates_moves_and_deletes() {
        let root = temp_root("follow");
        let (tx, rx) = mpsc::channel();
        let watched = root.clone();
        std::thread::spawn(move || watch(&watched, |c| tx.send(c).is_ok()));
        std::thread::sleep(Duration::from_millis(200));

        let mut index = scan(&root, &AtomicBool::new(false)).unwrap();
        // Paths are relative to the scanned root, which `path` shows as the drive.
        assert_eq!(index.len(), 4);
        assert_eq!(
            found(&index, "report", &root),
            [root.join(r"docs\deep\report.pdf").display().to_string()]
        );

        // A folder moved in from outside the watched tree arrives as one name.
        let outside = root.with_extension("outside");
        let _ = fs::remove_dir_all(&outside);
        fs::create_dir_all(outside.join("inner")).unwrap();
        fs::write(outside.join(r"inner\thesis.docx"), b"").unwrap();
        fs::rename(&outside, root.join("moved")).unwrap();
        fs::remove_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("fresh.md"), b"").unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let Ok(changes) = rx.recv_timeout(Duration::from_millis(300)) else {
                if found(&index, "fresh", &root).len() == 1 {
                    break;
                }
                continue;
            };
            let batch = records(&index, &root, &changes);
            index.apply_watched(&batch);
        }
        assert_eq!(
            found(&index, "thesis", &root),
            [root.join(r"moved\inner\thesis.docx").display().to_string()]
        );
        assert!(found(&index, "report", &root).is_empty());
        assert_eq!(found(&index, "fresh", &root).len(), 1);
        fs::remove_dir_all(&root).unwrap();
    }
}
