//! NTFS master file table enumeration and USN change-journal reading.

use std::{io, mem::size_of, os::windows::ffi::OsStrExt};

use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_HANDLE_EOF, GENERIC_READ, HANDLE},
        Storage::FileSystem::{
            CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
            OPEN_EXISTING,
        },
        System::{
            IO::DeviceIoControl,
            Ioctl::{
                FSCTL_ENUM_USN_DATA, FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL,
                MFT_ENUM_DATA_V0, READ_USN_JOURNAL_DATA_V0, USN_JOURNAL_DATA_V0,
            },
        },
    },
    core::PCWSTR,
};

/// Large reads keep the number of kernel round trips down on a two-million-file volume.
const BUFFER_BYTES: usize = 1 << 20;
/// Every reason bit: creations, deletions, renames, data and attribute changes.
const ALL_REASONS: u32 = u32::MAX;

/// One record from the MFT enumeration or the change journal (USN_RECORD_V2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// File reference number: MFT index plus sequence number.
    pub frn: u64,
    pub parent: u64,
    pub usn: i64,
    /// `USN_REASON_*` bits; zero in enumeration records.
    pub reason: u32,
    pub attributes: u32,
    pub name: String,
}

/// Identifies the journal a saved snapshot was taken from, and how far it had read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Journal {
    pub id: u64,
    pub first_usn: i64,
    pub next_usn: i64,
}

impl Journal {
    /// A snapshot saved at `usn` of journal `id` can catch up only if the journal is the same one
    /// and still holds every record since then; otherwise the volume must be enumerated again.
    pub fn can_resume(&self, id: u64, usn: i64) -> bool {
        id == self.id && usn >= self.first_usn && usn <= self.next_usn
    }
}

/// A read handle to an NTFS volume such as `C:`.
pub struct Volume(HANDLE);

// The handle is only ever used for synchronous DeviceIoControl calls.
unsafe impl Send for Volume {}

impl Drop for Volume {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

impl Volume {
    /// Needs administrator rights or LocalSystem; otherwise fails with access denied.
    pub fn open(letter: char) -> io::Result<Self> {
        let path: Vec<u16> = std::ffi::OsStr::new(&format!(r"\\.\{letter}:"))
            .encode_wide()
            .chain(Some(0))
            .collect();
        let handle = unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                GENERIC_READ.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )?
        };
        Ok(Self(handle))
    }

    pub fn journal(&self) -> io::Result<Journal> {
        let mut data = USN_JOURNAL_DATA_V0::default();
        self.control(FSCTL_QUERY_USN_JOURNAL, None, as_bytes_mut(&mut data))?;
        Ok(Journal {
            id: data.UsnJournalID,
            first_usn: data.FirstUsn,
            next_usn: data.NextUsn,
        })
    }

    /// Calls `on_record` for every file and folder in the MFT with a USN up to `journal.next_usn`.
    /// Records arrive in MFT order, not path order.
    pub fn enumerate(
        &self,
        journal: &Journal,
        mut on_record: impl FnMut(Record),
    ) -> io::Result<()> {
        let mut input = MFT_ENUM_DATA_V0 {
            StartFileReferenceNumber: 0,
            LowUsn: 0,
            HighUsn: journal.next_usn,
        };
        let mut buf = vec![0u8; BUFFER_BYTES];
        loop {
            let read = match self.control(FSCTL_ENUM_USN_DATA, Some(as_bytes(&input)), &mut buf) {
                Ok(n) => n,
                Err(e) if is_end_of_enumeration(&e) => return Ok(()),
                Err(e) => return Err(e),
            };
            let Some((next, records)) = split_output(&buf[..read]) else {
                return Ok(());
            };
            records.for_each(&mut on_record);
            input.StartFileReferenceNumber = next as u64;
        }
    }

    /// Reads journal records from `from` onwards. With `wait_secs` above zero, blocks until a new
    /// record exists or that many seconds pass, so a tailing thread costs nothing while the disk
    /// is quiet yet still notices a stop request. Returns the records and the USN to continue from.
    pub fn read_changes(
        &self,
        journal_id: u64,
        from: i64,
        wait_secs: u64,
    ) -> io::Result<(Vec<Record>, i64)> {
        let input = READ_USN_JOURNAL_DATA_V0 {
            StartUsn: from,
            ReasonMask: ALL_REASONS,
            ReturnOnlyOnClose: 0,
            Timeout: wait_secs,
            BytesToWaitFor: u64::from(wait_secs > 0),
            UsnJournalID: journal_id,
        };
        let mut buf = vec![0u8; BUFFER_BYTES];
        let read = self.control(FSCTL_READ_USN_JOURNAL, Some(as_bytes(&input)), &mut buf)?;
        match split_output(&buf[..read]) {
            Some((next, records)) => Ok((records.collect(), next)),
            None => Ok((Vec::new(), from)),
        }
    }

    fn control(&self, code: u32, input: Option<&[u8]>, output: &mut [u8]) -> io::Result<usize> {
        let mut returned = 0u32;
        unsafe {
            DeviceIoControl(
                self.0,
                code,
                input.map(|i| i.as_ptr().cast()),
                input.map_or(0, |i| i.len() as u32),
                Some(output.as_mut_ptr().cast()),
                output.len() as u32,
                Some(&mut returned),
                None,
            )?;
        }
        Ok(returned as usize)
    }
}

/// windows-rs turns its errors into io::Error with the HRESULT as the OS code, not the Win32 one.
fn is_end_of_enumeration(e: &io::Error) -> bool {
    e.raw_os_error() == Some(ERROR_HANDLE_EOF.to_hresult().0)
}

/// Both FSCTLs return the next cursor (an 8-byte USN or file reference) followed by records.
fn split_output(out: &[u8]) -> Option<(i64, impl Iterator<Item = Record> + '_)> {
    let next = i64::from_le_bytes(out.get(..8)?.try_into().ok()?);
    Some((next, parse_records(&out[8..])))
}

/// Parses back-to-back USN_RECORD_V2 structures, skipping other versions and stopping at the
/// first malformed length rather than reading past the buffer.
pub fn parse_records(mut buf: &[u8]) -> impl Iterator<Item = Record> + '_ {
    std::iter::from_fn(move || {
        loop {
            let len = read_u32(buf, 0)? as usize;
            if len < 60 || len > buf.len() {
                return None;
            }
            let (record, rest) = buf.split_at(len);
            buf = rest;
            if read_u16(record, 4)? != 2 {
                continue;
            }
            let name_len = read_u16(record, 56)? as usize;
            let name_offset = read_u16(record, 58)? as usize;
            let name_bytes = record.get(name_offset..name_offset + name_len)?;
            let units: Vec<u16> = name_bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .collect();
            return Some(Record {
                frn: read_u64(record, 8)?,
                parent: read_u64(record, 16)?,
                usn: read_u64(record, 24)? as i64,
                reason: read_u32(record, 40)?,
                attributes: read_u32(record, 52)?,
                name: String::from_utf16_lossy(&units),
            });
        }
    })
}

fn read_u16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn read_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn read_u64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

fn as_bytes<T>(value: &T) -> &[u8] {
    unsafe { std::slice::from_raw_parts((value as *const T).cast(), size_of::<T>()) }
}

fn as_bytes_mut<T>(value: &mut T) -> &mut [u8] {
    unsafe { std::slice::from_raw_parts_mut((value as *mut T).cast(), size_of::<T>()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a USN_RECORD_V2 the way NTFS lays it out, padded to 8 bytes.
    fn record(
        major: u16,
        frn: u64,
        parent: u64,
        usn: i64,
        reason: u32,
        attributes: u32,
        name: &str,
    ) -> Vec<u8> {
        let name: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let len = (60 + name.len()).div_ceil(8) * 8;
        let mut r = vec![0u8; len];
        r[0..4].copy_from_slice(&(len as u32).to_le_bytes());
        r[4..6].copy_from_slice(&major.to_le_bytes());
        r[8..16].copy_from_slice(&frn.to_le_bytes());
        r[16..24].copy_from_slice(&parent.to_le_bytes());
        r[24..32].copy_from_slice(&usn.to_le_bytes());
        r[40..44].copy_from_slice(&reason.to_le_bytes());
        r[52..56].copy_from_slice(&attributes.to_le_bytes());
        r[56..58].copy_from_slice(&(name.len() as u16).to_le_bytes());
        r[58..60].copy_from_slice(&60u16.to_le_bytes());
        r[60..60 + name.len()].copy_from_slice(&name);
        r
    }

    #[test]
    fn parses_v2_records_and_skips_other_versions() {
        let mut buf = record(2, 0x0005_0000_0000_1a2b, 5, 100, 0, 0x10, "Users");
        buf.extend(record(3, 9, 9, 9, 9, 9, "v3 is skipped"));
        buf.extend(record(
            2,
            7,
            0x0005_0000_0000_1a2b,
            200,
            0x100,
            0x20,
            "naïve résumé.txt",
        ));
        let records: Vec<Record> = parse_records(&buf).collect();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].name, "Users");
        assert_eq!(records[0].frn, 0x0005_0000_0000_1a2b);
        assert_eq!(records[0].attributes, 0x10);
        assert_eq!(records[1].parent, 0x0005_0000_0000_1a2b);
        assert_eq!(records[1].reason, 0x100);
        assert_eq!(records[1].usn, 200);
        assert_eq!(records[1].name, "naïve résumé.txt");
    }

    #[test]
    fn stops_at_truncated_or_bogus_lengths() {
        let full = record(2, 1, 2, 3, 0, 0, "a.txt");
        assert_eq!(parse_records(&full[..full.len() - 8]).count(), 0);
        let mut bogus = full.clone();
        bogus[0..4].copy_from_slice(&4u32.to_le_bytes());
        assert_eq!(parse_records(&bogus).count(), 0);
        assert_eq!(parse_records(&[]).count(), 0);
    }

    #[test]
    fn output_starts_with_the_next_cursor() {
        let mut out = 4242i64.to_le_bytes().to_vec();
        out.extend(record(2, 1, 2, 3, 0, 0, "x"));
        let (next, records) = split_output(&out).unwrap();
        assert_eq!(next, 4242);
        assert_eq!(records.count(), 1);
        assert!(split_output(&[1, 2, 3]).is_none());
    }

    #[test]
    fn recognises_the_end_of_enumeration_as_it_arrives() {
        let eof: io::Error = windows::core::Error::from(ERROR_HANDLE_EOF.to_hresult()).into();
        assert!(is_end_of_enumeration(&eof));
        let denied: io::Error = windows::core::Error::from(
            windows::Win32::Foundation::ERROR_ACCESS_DENIED.to_hresult(),
        )
        .into();
        assert!(!is_end_of_enumeration(&denied));
    }

    #[test]
    fn resumes_only_within_the_same_journal() {
        let j = Journal {
            id: 7,
            first_usn: 1000,
            next_usn: 5000,
        };
        assert!(j.can_resume(7, 1000));
        assert!(j.can_resume(7, 5000));
        assert!(!j.can_resume(8, 2000), "journal was recreated");
        assert!(!j.can_resume(7, 999), "records were purged");
        assert!(!j.can_resume(7, 5001), "snapshot is from the future");
    }

    /// Needs an elevated shell: `cargo test -p fsindex -- --ignored`.
    #[test]
    #[ignore]
    fn enumerates_the_system_drive() {
        let volume = Volume::open('C').unwrap();
        let journal = volume.journal().unwrap();
        let mut count = 0usize;
        let mut saw_windows = false;
        volume
            .enumerate(&journal, |r| {
                count += 1;
                saw_windows |= r.name.eq_ignore_ascii_case("Windows") && r.attributes & 0x10 != 0;
            })
            .unwrap();
        assert!(count > 10_000, "only {count} records");
        assert!(saw_windows);
    }
}
