//! Which folders content search reads: the user's choice, kept in `content-folders.txt` (one path
//! per line) so the app and the indexer agree, plus the notes folder, which is always included.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use windows::{
    Win32::{
        System::Com::CoTaskMemFree,
        UI::Shell::{
            FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, KF_FLAG_DEFAULT,
            SHGetKnownFolderPath,
        },
    },
    core::GUID,
};

use crate::{APP_NAME, data_dir};

fn list_path() -> PathBuf {
    data_dir().join("content-folders.txt")
}

/// The folders the user chose, or None until they have answered the first-run picker (an
/// empty list means they chose none).
pub fn chosen() -> Option<Vec<PathBuf>> {
    let text = fs::read_to_string(list_path()).ok()?;
    Some(parse(&text))
}

fn parse(text: &str) -> Vec<PathBuf> {
    // An editor may start the file with a byte-order mark, which would hide a first comment.
    text.trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(PathBuf::from)
        .collect()
}

/// Replaces the chosen folders, dropping duplicates and folders inside another chosen one.
pub fn set_chosen(folders: &[PathBuf]) -> io::Result<()> {
    let kept = outermost(folders);
    let mut text = String::from("# Folders Seekr searches inside. One path per line.\n");
    for folder in &kept {
        text.push_str(&folder.display().to_string());
        text.push('\n');
    }
    let path = list_path();
    fs::create_dir_all(data_dir())?;
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, text)?;
    fs::rename(tmp, path)
}

/// Adds one folder to the chosen list (creating it if the picker was never answered).
pub fn add(folder: &Path) -> io::Result<()> {
    let mut folders = chosen().unwrap_or_default();
    folders.push(folder.to_path_buf());
    set_chosen(&folders)
}

fn outermost(folders: &[PathBuf]) -> Vec<PathBuf> {
    let lower = |p: &Path| p.to_string_lossy().trim_end_matches('\\').to_lowercase();
    let mut kept: Vec<PathBuf> = Vec::new();
    for folder in folders {
        let f = lower(folder);
        let inside = |other: &PathBuf| {
            let o = lower(other);
            f == o || f.starts_with(&format!("{o}\\"))
        };
        if kept.iter().any(inside) {
            continue;
        }
        kept.retain(|k| {
            let k = lower(k);
            !k.starts_with(&format!("{f}\\"))
        });
        kept.push(folder.clone());
    }
    kept
}

/// `Documents\Seekr Notes`, where notes live; always searched, chosen or not.
pub fn notes_dir() -> Option<PathBuf> {
    Some(known_folder(&FOLDERID_Documents)?.join(format!("{APP_NAME} Notes")))
}

/// Everything content search reads: the chosen folders and the notes folder.
pub fn folders() -> Vec<PathBuf> {
    let mut all = chosen().unwrap_or_default();
    all.extend(notes_dir());
    outermost(&all)
}

/// Desktop, Documents and Downloads where Windows really keeps them (often inside OneDrive).
pub fn suggestions() -> Vec<PathBuf> {
    [FOLDERID_Documents, FOLDERID_Desktop, FOLDERID_Downloads]
        .iter()
        .filter_map(known_folder)
        .collect()
}

fn known_folder(id: &GUID) -> Option<PathBuf> {
    let raw = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()? };
    let path = unsafe { raw.to_string() };
    unsafe { CoTaskMemFree(Some(raw.0 as _)) };
    path.ok().map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_outermost_folders() {
        let p = |s: &str| PathBuf::from(s);
        let kept = outermost(&[
            p(r"C:\Work\a"),
            p(r"C:\Work"),
            p(r"c:\work\b\"),
            p(r"C:\Workshop"),
            p(r"C:\Work"),
        ]);
        assert_eq!(kept, [p(r"C:\Work"), p(r"C:\Workshop")]);
    }

    #[test]
    fn ignores_a_byte_order_mark() {
        assert!(parse("\u{feff}# none\n").is_empty());
        assert_eq!(parse("\u{feff}C:\\Notes\n"), [PathBuf::from(r"C:\Notes")]);
    }

    #[test]
    fn parses_lines_and_skips_comments() {
        assert_eq!(
            parse("# note\nC:\\A\n\n  D:\\B  \n"),
            [PathBuf::from(r"C:\A"), PathBuf::from(r"D:\B")]
        );
    }

    #[test]
    fn known_folders_resolve() {
        assert_eq!(suggestions().len(), 3);
        assert!(notes_dir().unwrap().ends_with("Seekr Notes"));
    }
}
