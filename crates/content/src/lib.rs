//! Content search: what Seekr reads inside files, and the index it searches.
//!
//! - [`extract`]: plain text from text, code, Markdown, notebooks, RTF and Office files.
//! - [`chunk`]: passages of a few hundred words, each with the line it starts on.
//! - [`store`]: the Tantivy full-text index (BM25) with highlighted snippets.
//! - [`state`]: what the indexer last did with each file, including quarantined ones.
//! - [`embed`]: passage and query embeddings (gte-modernbert-base through ONNX Runtime).
//! - [`vectors`]: the int8 meaning index the app searches.
//!
//! The `indexer` process writes; the app only searches.

use std::{
    fs, io,
    path::PathBuf,
    time::{Duration, SystemTime},
};

pub mod chunk;
pub mod embed;
pub mod extract;
pub mod state;
pub mod store;
pub mod vectors;

/// `%LOCALAPPDATA%\Seekr\content`: the index and the indexer's state.
pub fn dir() -> PathBuf {
    common::data_dir().join("content")
}

pub fn index_dir() -> PathBuf {
    dir().join("index")
}

pub fn state_path() -> PathBuf {
    dir().join("state.db")
}

/// Every passage's vector, for meaning search.
pub fn vectors_path() -> PathBuf {
    dir().join("vectors.bin")
}

/// While this file exists the indexer doesn't run, and a running one stops at the next file.
fn paused_path() -> PathBuf {
    dir().join("paused")
}

pub fn is_paused() -> bool {
    paused_path().exists()
}

pub fn set_paused(paused: bool) -> io::Result<()> {
    if paused {
        fs::create_dir_all(dir())?;
        fs::write(paused_path(), b"")
    } else {
        match fs::remove_file(paused_path()) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// Left by the app's "Rebuild content index"; the next indexer run, which holds the index's
/// lock, empties the index and its state and reads every file again.
fn rebuild_path() -> PathBuf {
    dir().join("rebuild")
}

pub fn request_rebuild() -> io::Result<()> {
    fs::create_dir_all(dir())?;
    fs::write(rebuild_path(), b"")
}

/// Whether a rebuild was asked for; asking again needs a new request.
pub fn take_rebuild() -> bool {
    fs::remove_file(rebuild_path()).is_ok()
}

fn progress_path() -> PathBuf {
    dir().join("progress.txt")
}

/// What a running indexer is doing, for the panel: a phase ("reading" files or "embedding"
/// passages) and how far it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    pub phase: String,
    pub done: u64,
    pub total: Option<u64>,
}

pub fn write_progress(progress: Option<&Progress>) {
    let _ = match progress {
        Some(p) => fs::write(
            progress_path(),
            format!(
                "{} {} {}",
                p.phase,
                p.done,
                p.total.map_or(-1, |t| t as i64)
            ),
        ),
        None => fs::remove_file(progress_path()),
    };
}

/// The indexer's progress, if one wrote it in the last half minute (an older file is left by a
/// run that died).
pub fn read_progress() -> Option<Progress> {
    let path = progress_path();
    let age = fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())?;
    if age > Duration::from_secs(30) {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;
    let mut parts = text.split_whitespace();
    let phase = parts.next()?.to_string();
    let done = parts.next()?.parse().ok()?;
    let total = parts
        .next()?
        .parse::<i64>()
        .ok()
        .and_then(|t| u64::try_from(t).ok());
    Some(Progress { phase, done, total })
}
