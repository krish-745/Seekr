//! The service's work for one signed-in user: snapshot the volume as they may see it, then send
//! journal changes over the pipe until they sign out or the service stops.

use std::{
    fs,
    io::{self, BufWriter},
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

use fsindex::{
    usn::{Journal, Record, Volume},
    wire::{self, Header, Message, PIPE_NAME},
};

use crate::{security, tree::Tree};

/// How long a journal read waits for new records before re-checking for stop or sign-out.
const WAIT_SECS: u64 = 2;

#[derive(Default)]
pub struct Control {
    pub stop: AtomicBool,
    /// Set on any session change; the serving loop then checks whether the console user changed.
    pub session_changed: AtomicBool,
}

impl Control {
    fn should_leave(&self, user_sid: &str) -> bool {
        if self.stop.load(Ordering::SeqCst) {
            return true;
        }
        // Lock, unlock and remote connections also raise session changes; only a different
        // console user means a new view.
        self.session_changed.swap(false, Ordering::SeqCst)
            && security::console_user().as_deref() != Some(user_sid)
    }
}

/// Unblocks a pending ConnectNamedPipe by connecting to it ourselves.
pub fn wake() {
    let _ = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(PIPE_NAME);
}

pub fn serve_user(user_sid: &str, control: &Control) -> io::Result<()> {
    let volume = Volume::open('C')?;
    let journal = volume.journal()?;
    let mut tree = Tree::new(excluded_folders(user_sid));
    let started = std::time::Instant::now();
    let (path, header) = write_snapshot(&volume, &journal, &mut tree, user_sid)?;
    tracing::info!(
        "snapshot of {} entries ({} folders tracked) in {:?}",
        header.count,
        tree.folders(),
        started.elapsed()
    );
    while !control.should_leave(user_sid) {
        let mut pipe = security::create_pipe(PIPE_NAME, user_sid)?;
        security::accept(&pipe)?;
        if control.should_leave(user_sid) {
            break;
        }
        match session(
            &mut pipe, &volume, &mut tree, &path, header, control, user_sid,
        ) {
            Ok(()) => {}
            Err(e) => tracing::info!("client gone: {e}"),
        }
    }
    Ok(())
}

fn session(
    pipe: &mut fs::File,
    volume: &Volume,
    tree: &mut Tree,
    path: &str,
    header: Header,
    control: &Control,
    user_sid: &str,
) -> io::Result<()> {
    let Message::Hello { journal_id, usn } = wire::read_message(pipe)? else {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "expected Hello"));
    };
    let journal = volume.journal()?;
    let mut cursor = if journal.can_resume(journal_id, usn) {
        usn
    } else {
        let path = path.to_string();
        wire::write_message(pipe, &Message::Snapshot { path, header })?;
        header.usn
    };
    tracing::info!("client connected; streaming changes from USN {cursor}");
    while !control.should_leave(user_sid) {
        let (records, next) = volume.read_changes(journal.id, cursor, WAIT_SECS)?;
        let visible: Vec<Record> = records
            .into_iter()
            .filter(|r| {
                tree.apply(r);
                tree.visible(r)
            })
            .collect();
        if !visible.is_empty() {
            wire::write_message(
                pipe,
                &Message::Changes {
                    next_usn: next,
                    records: visible,
                },
            )?;
        }
        cursor = next;
    }
    Ok(())
}

/// shortcut: holds every record in memory while writing (about 150 MB for 2 M files, freed
/// afterwards); stream two passes over the MFT if that peak ever matters.
fn write_snapshot(
    volume: &Volume,
    journal: &Journal,
    tree: &mut Tree,
    user_sid: &str,
) -> io::Result<(String, Header)> {
    let mut records = Vec::new();
    volume.enumerate(journal, |r| {
        tree.apply(&r);
        records.push(r);
    })?;
    let visible: Vec<&Record> = records.iter().filter(|r| tree.visible(r)).collect();
    let header = Header {
        journal_id: journal.id,
        usn: journal.next_usn,
        count: visible.len() as u64,
    };
    // SYSTEM's own %LOCALAPPDATA%: no other account can create files or links there.
    let dir = common::data_dir();
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("snapshot-{user_sid}.zst"));
    let tmp = dir.join(format!("snapshot-{user_sid}.tmp"));
    let _ = fs::remove_file(&tmp);
    let file = security::create_private_file(&tmp, user_sid)?;
    wire::write_snapshot(BufWriter::new(file), header, visible)?;
    fs::rename(&tmp, &path)?;
    Ok((path.display().to_string(), header))
}

fn excluded_folders(user_sid: &str) -> Vec<u64> {
    let profiles: Vec<PathBuf> = security::other_profiles(user_sid).unwrap_or_else(|e| {
        tracing::error!("can't read ProfileList ({e}); no profiles excluded");
        Vec::new()
    });
    profiles
        .iter()
        .filter_map(|p| match security::frn_of(p) {
            Ok(frn) => Some(frn),
            // A profile can be listed but deleted from disk; nothing there to hide.
            Err(e) => {
                tracing::info!("profile {} not on disk: {e}", p.display());
                None
            }
        })
        .collect()
}
