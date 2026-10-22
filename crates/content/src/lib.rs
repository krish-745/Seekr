//! Content search: what Seekr reads inside files, and the index it searches.
//!
//! - [`extract`]: plain text from text, code, Markdown, notebooks, RTF and Office files.
//! - [`chunk`]: passages of a few hundred words, each with the line it starts on.
//! - [`store`]: the Tantivy full-text index (BM25) with highlighted snippets.
//! - [`state`]: what the indexer last did with each file, including quarantined ones.
//!
//! The `indexer` process writes; the app only searches.

use std::path::PathBuf;

pub mod chunk;
pub mod extract;
pub mod state;
pub mod store;

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
