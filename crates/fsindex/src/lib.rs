//! File-system sources for Seekr's file-name index.
//!
//! - [`usn`]: names and hierarchy of every file on an NTFS volume, read from the MFT via
//!   `FSCTL_ENUM_USN_DATA`, then kept current by tailing the USN change journal. Both need an
//!   administrator or LocalSystem handle to the volume.
//! - [`meta`]: sizes and dates, from a one-time low-priority directory walk that needs no rights.
//! - [`scan`]: the no-admin fallback for drives the service doesn't read: a walk for names, then
//!   `ReadDirectoryChangesW` for changes.
//! - [`wire`]: the snapshot file and pipe messages the index service sends the app.
//! - [`index`]: the app's packed in-memory index and fuzzy search over it.
//! - [`filter`]: the query language for filters such as `ext:` `in:` `dm:` and `size:`.

pub mod filter;
pub mod index;
pub mod meta;
pub mod scan;
pub mod usn;
pub mod wire;
