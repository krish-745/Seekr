//! What the index service hands the app: a zstd snapshot file of every entry the signed-in user may
//! see, then live changes over a named pipe. Both use the same little-endian record encoding.

use std::io::{self, Read, Write};

use crate::usn::Record;

/// The service's pipe; its DACL admits only SYSTEM and the signed-in user.
pub const PIPE_NAME: &str = r"\\.\pipe\seekr-index";
const MAGIC: &[u8; 8] = b"SEEKRSN1";
/// Frames larger than this are treated as corrupt rather than allocated.
const MAX_FRAME: usize = 64 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub journal_id: u64,
    /// The snapshot holds everything up to this USN; changes continue from here.
    pub usn: i64,
    pub count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    /// App to service, first thing after connecting: where its own copy left off. Zeroes mean it
    /// has none and needs a snapshot.
    Hello { journal_id: u64, usn: i64 },
    /// Service to app: read this snapshot file, then expect changes from its USN.
    Snapshot { path: String, header: Header },
    /// Service to app: journal records in order; continue from `next_usn` after a reconnect.
    Changes { next_usn: i64, records: Vec<Record> },
}

pub fn write_snapshot<'a>(
    out: impl Write,
    header: Header,
    records: impl IntoIterator<Item = &'a Record>,
) -> io::Result<()> {
    let mut z = zstd::Encoder::new(out, 3)?;
    z.write_all(MAGIC)?;
    z.write_all(&header.journal_id.to_le_bytes())?;
    z.write_all(&header.usn.to_le_bytes())?;
    z.write_all(&header.count.to_le_bytes())?;
    for r in records {
        put_record(&mut z, r)?;
    }
    z.finish()?.flush()
}

/// Reads the header, then calls `on_record` for each of its `count` records.
pub fn read_snapshot(input: impl Read, mut on_record: impl FnMut(Record)) -> io::Result<Header> {
    let mut z = io::BufReader::new(zstd::Decoder::new(input)?);
    let mut magic = [0u8; 8];
    z.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(invalid("not a Seekr snapshot"));
    }
    let header = Header {
        journal_id: get_u64(&mut z)?,
        usn: get_u64(&mut z)? as i64,
        count: get_u64(&mut z)?,
    };
    for _ in 0..header.count {
        on_record(get_record(&mut z)?);
    }
    Ok(header)
}

pub fn write_message(out: &mut impl Write, message: &Message) -> io::Result<()> {
    let mut body = Vec::new();
    match message {
        Message::Hello { journal_id, usn } => {
            body.push(10);
            body.extend(journal_id.to_le_bytes());
            body.extend(usn.to_le_bytes());
        }
        Message::Snapshot { path, header } => {
            body.push(1);
            put_str(&mut body, path)?;
            body.extend(header.journal_id.to_le_bytes());
            body.extend(header.usn.to_le_bytes());
            body.extend(header.count.to_le_bytes());
        }
        Message::Changes { next_usn, records } => {
            body.push(2);
            body.extend(next_usn.to_le_bytes());
            body.extend((records.len() as u32).to_le_bytes());
            for r in records {
                put_record(&mut body, r)?;
            }
        }
    }
    out.write_all(&(body.len() as u32).to_le_bytes())?;
    out.write_all(&body)?;
    out.flush()
}

pub fn read_message(input: &mut impl Read) -> io::Result<Message> {
    let len = get_u32(input)? as usize;
    if len == 0 || len > MAX_FRAME {
        return Err(invalid("bad frame length"));
    }
    let mut body = vec![0u8; len];
    input.read_exact(&mut body)?;
    let mut b = &body[1..];
    let message = match body[0] {
        10 => Message::Hello {
            journal_id: get_u64(&mut b)?,
            usn: get_u64(&mut b)? as i64,
        },
        1 => Message::Snapshot {
            path: get_str(&mut b)?,
            header: Header {
                journal_id: get_u64(&mut b)?,
                usn: get_u64(&mut b)? as i64,
                count: get_u64(&mut b)?,
            },
        },
        2 => {
            let next_usn = get_u64(&mut b)? as i64;
            let count = get_u32(&mut b)?;
            let records = (0..count)
                .map(|_| get_record(&mut b))
                .collect::<io::Result<_>>()?;
            Message::Changes { next_usn, records }
        }
        kind => return Err(invalid(&format!("unknown message kind {kind}"))),
    };
    if !b.is_empty() {
        return Err(invalid("trailing bytes in frame"));
    }
    Ok(message)
}

fn put_record(out: &mut impl Write, r: &Record) -> io::Result<()> {
    out.write_all(&r.frn.to_le_bytes())?;
    out.write_all(&r.parent.to_le_bytes())?;
    out.write_all(&r.usn.to_le_bytes())?;
    out.write_all(&r.reason.to_le_bytes())?;
    out.write_all(&r.attributes.to_le_bytes())?;
    put_str(out, &r.name)
}

fn get_record(input: &mut impl Read) -> io::Result<Record> {
    Ok(Record {
        frn: get_u64(input)?,
        parent: get_u64(input)?,
        usn: get_u64(input)? as i64,
        reason: get_u32(input)?,
        attributes: get_u32(input)?,
        name: get_str(input)?,
    })
}

fn put_str(out: &mut impl Write, s: &str) -> io::Result<()> {
    let len = u16::try_from(s.len()).map_err(|_| invalid("string longer than 64 KiB"))?;
    out.write_all(&len.to_le_bytes())?;
    out.write_all(s.as_bytes())
}

fn get_str(input: &mut impl Read) -> io::Result<String> {
    let mut len = [0u8; 2];
    input.read_exact(&mut len)?;
    let mut bytes = vec![0u8; u16::from_le_bytes(len) as usize];
    input.read_exact(&mut bytes)?;
    String::from_utf8(bytes).map_err(|_| invalid("name is not UTF-8"))
}

fn get_u32(input: &mut impl Read) -> io::Result<u32> {
    let mut b = [0u8; 4];
    input.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn get_u64(input: &mut impl Read) -> io::Result<u64> {
    let mut b = [0u8; 8];
    input.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(frn: u64, name: &str) -> Record {
        Record {
            frn,
            parent: 5,
            usn: frn as i64 * 10,
            reason: 0x100,
            attributes: 0x20,
            name: name.into(),
        }
    }

    #[test]
    fn snapshot_round_trips() {
        let records = vec![rec(1, "Users"), rec(2, "naïve résumé.txt"), rec(3, "")];
        let header = Header {
            journal_id: 7,
            usn: 12345,
            count: 3,
        };
        let mut file = Vec::new();
        write_snapshot(&mut file, header, &records).unwrap();
        let mut back = Vec::new();
        assert_eq!(read_snapshot(&file[..], |r| back.push(r)).unwrap(), header);
        assert_eq!(back, records);
    }

    #[test]
    fn rejects_other_files_and_truncation() {
        let mut file = Vec::new();
        let header = Header {
            journal_id: 1,
            usn: 1,
            count: 2,
        };
        write_snapshot(&mut file, header, &[rec(1, "only one")]).unwrap();
        assert!(
            read_snapshot(&file[..], |_| {}).is_err(),
            "count says 2, file has 1"
        );
        let not_ours = zstd::encode_all(&b"SOMETHING ELSE"[..], 3).unwrap();
        assert_eq!(
            read_snapshot(&not_ours[..], |_| {}).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn messages_round_trip() {
        let messages = [
            Message::Hello {
                journal_id: 9,
                usn: -1,
            },
            Message::Snapshot {
                path: r"C:\ProgramData\Seekr\snapshot-S-1-5-21-1.zst".into(),
                header: Header {
                    journal_id: 9,
                    usn: 77,
                    count: 2_000_000,
                },
            },
            Message::Changes {
                next_usn: 99,
                records: vec![rec(4, "new.txt"), rec(5, "x")],
            },
        ];
        let mut pipe = Vec::new();
        for m in &messages {
            write_message(&mut pipe, m).unwrap();
        }
        let mut input = &pipe[..];
        for m in &messages {
            assert_eq!(&read_message(&mut input).unwrap(), m);
        }
        assert!(input.is_empty());
    }

    #[test]
    fn rejects_corrupt_frames() {
        let huge = ((MAX_FRAME + 1) as u32).to_le_bytes();
        assert!(read_message(&mut &huge[..]).is_err());
        let unknown = [1u8, 0, 0, 0, 42];
        assert!(read_message(&mut &unknown[..]).is_err());
        let trailing = [2u8, 0, 0, 0, 10, 0];
        assert!(read_message(&mut &trailing[..]).is_err());
    }
}
