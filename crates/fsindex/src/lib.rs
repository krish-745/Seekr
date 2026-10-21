//! File-system sources for Seekr's file-name index.
//!
//! - [`usn`]: names and hierarchy of every file on an NTFS volume, read from the MFT via
//!   `FSCTL_ENUM_USN_DATA`, then kept current by tailing the USN change journal. Both need an
//!   administrator or LocalSystem handle to the volume.
//! - [`meta`]: sizes and dates, from a one-time low-priority directory walk that needs no rights.

pub mod meta;
pub mod usn;
