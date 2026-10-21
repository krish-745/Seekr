//! Which records the signed-in user may see: nothing inside another account's profile, and none of
//! NTFS's own metadata files. Only folders are tracked (file reference number to parent), which is
//! all the ancestry checks need.

use std::collections::{HashMap, HashSet};

use fsindex::usn::Record;

const DIRECTORY: u32 = 0x10;
const USN_REASON_FILE_DELETE: u32 = 0x200;
/// MFT records 0-23 are NTFS metadata ($MFT, $LogFile, $Extend, ...), except 5, the root folder.
const ROOT_INDEX: u64 = 5;
const RESERVED_INDEXES: u64 = 24;
/// Deeper than any real path; stops a corrupt parent cycle from spinning forever.
const MAX_DEPTH: usize = 512;

#[derive(Default)]
pub struct Tree {
    parents: HashMap<u64, u64>,
    excluded: HashSet<u64>,
}

impl Tree {
    /// `excluded` are folders whose whole subtree is hidden: other users' profiles.
    pub fn new(excluded: impl IntoIterator<Item = u64>) -> Self {
        Self {
            parents: HashMap::new(),
            excluded: excluded.into_iter().collect(),
        }
    }

    /// Keeps the folder map current; call for every enumeration and journal record, in order.
    pub fn apply(&mut self, r: &Record) {
        if r.attributes & DIRECTORY == 0 {
            return;
        }
        if r.reason & USN_REASON_FILE_DELETE != 0 {
            self.parents.remove(&r.frn);
        } else {
            self.parents.insert(r.frn, r.parent);
        }
    }

    pub fn visible(&self, r: &Record) -> bool {
        if is_metadata(r.frn) || self.excluded.contains(&r.frn) {
            return false;
        }
        let mut at = r.parent;
        for _ in 0..MAX_DEPTH {
            if self.excluded.contains(&at) || is_metadata(at) {
                return false;
            }
            match self.parents.get(&at) {
                Some(&up) if up != at => at = up,
                // Reached the root, or a folder the map doesn't know yet: nothing excluded above.
                _ => return true,
            }
        }
        false
    }

    pub fn folders(&self) -> usize {
        self.parents.len()
    }
}

fn is_metadata(frn: u64) -> bool {
    let index = frn & 0x0000_FFFF_FFFF_FFFF;
    index < RESERVED_INDEXES && index != ROOT_INDEX
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: u64 = 0x0005_0000_0000_0005;

    fn dir(frn: u64, parent: u64) -> Record {
        Record {
            frn,
            parent,
            usn: 0,
            reason: 0,
            attributes: DIRECTORY,
            name: String::new(),
        }
    }

    fn file(frn: u64, parent: u64) -> Record {
        Record {
            attributes: 0x20,
            ..dir(frn, parent)
        }
    }

    /// C:\Users\{me=101, other=102}, C:\Users\other\Documents=103.
    fn tree() -> Tree {
        let mut t = Tree::new([102]);
        for r in [dir(100, ROOT), dir(101, 100), dir(102, 100), dir(103, 102)] {
            t.apply(&r);
        }
        t
    }

    #[test]
    fn hides_other_profiles_at_any_depth() {
        let t = tree();
        assert!(t.visible(&file(200, 101)), "my own files");
        assert!(t.visible(&dir(100, ROOT)), "C:\\Users itself");
        assert!(!t.visible(&dir(102, 100)), "the other profile folder");
        assert!(!t.visible(&file(201, 102)), "a file in it");
        assert!(!t.visible(&file(202, 103)), "a file deeper in it");
    }

    #[test]
    fn hides_ntfs_metadata_but_not_the_root() {
        let t = tree();
        assert!(!t.visible(&file(0x0001_0000_0000_0000, ROOT)), "$MFT");
        assert!(
            !t.visible(&file(300, 0x000B_0000_0000_000B)),
            "inside $Extend"
        );
        assert!(t.visible(&file(301, ROOT)), "a file in C:\\");
    }

    #[test]
    fn follows_moves_and_deletes() {
        let mut t = tree();
        let mut moved = dir(103, 101);
        moved.reason = 0x2000; // renamed into my profile
        t.apply(&moved);
        assert!(t.visible(&file(202, 103)));
        let mut gone = dir(103, 101);
        gone.reason = USN_REASON_FILE_DELETE;
        t.apply(&gone);
        assert_eq!(t.folders(), 3);
    }

    #[test]
    fn survives_parent_cycles() {
        let mut t = Tree::new([]);
        t.apply(&dir(400, 401));
        t.apply(&dir(401, 400));
        assert!(!t.visible(&file(402, 400)));
    }
}
