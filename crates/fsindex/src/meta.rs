//! Sizes and dates from a directory walk: `FindFirstFileExW` with basic info and large fetches, on
//! worker threads in background mode (very low CPU and I/O priority). Reparse points (junctions,
//! symlinks, compatibility links) are reported but never entered, so the walk can't loop or leave
//! the tree. Listing a folder never downloads cloud-only files.

use std::{
    io,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::{
        Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use windows::{
    Win32::{
        Storage::FileSystem::{
            FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FIND_FIRST_EX_LARGE_FETCH,
            FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW,
            WIN32_FIND_DATAW,
        },
        System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
            THREAD_MODE_BACKGROUND_END,
        },
    },
    core::PCWSTR,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meta {
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    /// FILETIME: 100 ns ticks since 1601-01-01 UTC.
    pub modified: i64,
    pub attributes: u32,
}

impl Meta {
    pub fn is_reparse_point(&self) -> bool {
        self.attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
    }
}

/// Calls `on_listing` with each folder's entries, from several threads at once, for everything
/// under `root` (not `root`
/// itself). Folders that can't be read, such as other users' profiles, are skipped. Returns
/// `Interrupted` once `cancel` is set.
pub fn walk(
    root: &Path,
    cancel: &AtomicBool,
    on_listing: impl Fn(Vec<Meta>) + Sync,
) -> io::Result<()> {
    // One thread took 110 s for 2.2 M entries on this NVMe drive; listing parallelises well.
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    // Folders waiting to be listed, and how many workers are listing one right now.
    let work = Mutex::new((vec![root.to_path_buf()], 0usize));
    let changed = Condvar::new();
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                let _priority = Background::begin();
                worker(&work, &changed, cancel, &on_listing);
            });
        }
    });
    if cancel.load(Ordering::Relaxed) {
        return Err(io::ErrorKind::Interrupted.into());
    }
    Ok(())
}

fn worker(
    work: &Mutex<(Vec<PathBuf>, usize)>,
    changed: &Condvar,
    cancel: &AtomicBool,
    on_listing: &(impl Fn(Vec<Meta>) + Sync),
) {
    loop {
        let dir = {
            let mut guard = work.lock().unwrap();
            loop {
                if cancel.load(Ordering::Relaxed) {
                    changed.notify_all();
                    return;
                }
                if let Some(dir) = guard.0.pop() {
                    guard.1 += 1;
                    break dir;
                }
                // Nothing queued and nobody listing: the walk is complete.
                if guard.1 == 0 {
                    changed.notify_all();
                    return;
                }
                guard = changed.wait(guard).unwrap();
            }
        };
        let mut listing = Vec::new();
        list(&dir, &mut |meta| listing.push(meta));
        let found: Vec<PathBuf> = listing
            .iter()
            .filter(|m| m.is_dir && !m.is_reparse_point())
            .map(|m| m.path.clone())
            .collect();
        // One call per folder rather than per entry keeps callers' locks uncontended.
        if !listing.is_empty() {
            on_listing(listing);
        }
        let mut guard = work.lock().unwrap();
        guard.0.extend(found);
        guard.1 -= 1;
        changed.notify_all();
    }
}

fn list(dir: &Path, on_entry: &mut impl FnMut(Meta)) {
    let pattern: Vec<u16> = long_path(dir)
        .join("*")
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let mut data = WIN32_FIND_DATAW::default();
    let Ok(find) = (unsafe {
        FindFirstFileExW(
            PCWSTR(pattern.as_ptr()),
            FindExInfoBasic,
            (&mut data as *mut WIN32_FIND_DATAW).cast(),
            FindExSearchNameMatch,
            None,
            FIND_FIRST_EX_LARGE_FETCH,
        )
    }) else {
        return;
    };
    loop {
        let len = data
            .cFileName
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(data.cFileName.len());
        let name = String::from_utf16_lossy(&data.cFileName[..len]);
        if name != "." && name != ".." {
            let modified = (u64::from(data.ftLastWriteTime.dwHighDateTime) << 32)
                | u64::from(data.ftLastWriteTime.dwLowDateTime);
            on_entry(Meta {
                path: dir.join(&name),
                is_dir: data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0,
                size: (u64::from(data.nFileSizeHigh) << 32) | u64::from(data.nFileSizeLow),
                modified: modified as i64,
                attributes: data.dwFileAttributes,
            });
        }
        if unsafe { FindNextFileW(find, &mut data) }.is_err() {
            break;
        }
    }
    unsafe {
        let _ = FindClose(find);
    }
}

/// The `\\?\` form lifts the 260-character limit for deep trees.
fn long_path(path: &Path) -> PathBuf {
    let s = path.as_os_str().to_string_lossy();
    if s.starts_with(r"\\?\") {
        path.to_path_buf()
    } else if let Some(unc) = s.strip_prefix(r"\\") {
        PathBuf::from(format!(r"\\?\UNC\{unc}"))
    } else {
        PathBuf::from(format!(r"\\?\{s}"))
    }
}

/// Background mode lowers this thread's CPU and I/O priority until dropped.
struct Background;

impl Background {
    fn begin() -> Self {
        // Fails harmlessly if the caller already put the thread in background mode.
        let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) };
        Self
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_END) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_tree(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("seekr-walk-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(r"a\b")).unwrap();
        fs::write(root.join(r"a\b\file.txt"), b"hello").unwrap();
        fs::write(root.join("top.txt"), b"").unwrap();
        root
    }

    #[test]
    fn reports_sizes_and_does_not_enter_junctions() {
        let root = temp_tree("junction");
        let made = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(root.join("j"))
            .arg(root.join("a"))
            .output()
            .unwrap();
        assert!(made.status.success(), "{made:?}");

        let seen = Mutex::new(Vec::new());
        walk(&root, &AtomicBool::new(false), |l| {
            seen.lock().unwrap().extend(l)
        })
        .unwrap();
        let seen = seen.into_inner().unwrap();
        let file = seen
            .iter()
            .find(|m| m.path.ends_with(r"a\b\file.txt"))
            .unwrap();
        assert_eq!((file.size, file.is_dir), (5, false));
        assert!(file.modified > 0);
        let junction = seen.iter().find(|m| m.path == root.join("j")).unwrap();
        assert!(junction.is_dir && junction.is_reparse_point());
        assert!(
            !seen
                .iter()
                .any(|m| m.path.starts_with(root.join("j")) && m.path != root.join("j"))
        );
        assert_eq!(seen.len(), 5, "a, a\\b, a\\b\\file.txt, top.txt, j");

        // Removing a junction never touches its target.
        fs::remove_dir(root.join("j")).unwrap();
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn stops_when_cancelled() {
        let root = temp_tree("cancel");
        let err = walk(&root, &AtomicBool::new(true), |_| {}).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Interrupted);
        fs::remove_dir_all(&root).unwrap();
    }

    /// Timing on the real drive: `cargo test -p fsindex --release walks_the_system_drive -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn walks_the_system_drive() {
        use std::sync::atomic::AtomicU64;
        let started = std::time::Instant::now();
        let (entries, bytes) = (AtomicU64::new(0), AtomicU64::new(0));
        walk(Path::new(r"C:\"), &AtomicBool::new(false), |l| {
            entries.fetch_add(l.len() as u64, Ordering::Relaxed);
            bytes.fetch_add(l.iter().map(|m| m.size).sum(), Ordering::Relaxed);
        })
        .unwrap();
        let entries = entries.into_inner();
        println!(
            "{entries} entries, {} GB, {:?}",
            bytes.into_inner() >> 30,
            started.elapsed()
        );
        assert!(entries > 100_000);
    }

    #[test]
    fn long_path_prefixes() {
        assert_eq!(long_path(Path::new(r"C:\x")), PathBuf::from(r"\\?\C:\x"));
        assert_eq!(
            long_path(Path::new(r"\\srv\share\x")),
            PathBuf::from(r"\\?\UNC\srv\share\x")
        );
        assert_eq!(
            long_path(Path::new(r"\\?\C:\x")),
            PathBuf::from(r"\\?\C:\x")
        );
    }
}
