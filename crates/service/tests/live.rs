//! Against the installed SeekrIndex service, run as the signed-in user (not elevated):
//! `cargo test -p service --test live -- --ignored --nocapture`. Quit Seekr first: the pipe
//! serves one client at a time, and a running Seekr holds it.

use std::{
    fs,
    time::{Duration, Instant},
};

use fsindex::wire::{self, Message, PIPE_NAME};

#[test]
#[ignore]
fn serves_my_view_and_live_changes() {
    let mut pipe = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(PIPE_NAME)
        .expect("service pipe; is SeekrIndex running?");
    wire::write_message(
        &mut pipe,
        &Message::Hello {
            journal_id: 0,
            usn: 0,
        },
    )
    .unwrap();
    let Message::Snapshot { path, header } = wire::read_message(&mut pipe).unwrap() else {
        panic!("a client without a copy gets a snapshot first");
    };

    let started = Instant::now();
    let (mut count, mut ntuser, mut service_logs) = (0u64, 0, 0);
    let file = fs::File::open(&path).expect("the snapshot is readable by me");
    let read = wire::read_snapshot(file, |r| {
        count += 1;
        ntuser += usize::from(r.name.eq_ignore_ascii_case("NTUSER.DAT"));
        // The service's own logs live only in LocalSystem's profile, which must stay hidden.
        // (seekr-svc.exe itself is in Program Files and target\, so match the .log only.)
        service_logs += usize::from(r.name.starts_with("seekr-svc.") && r.name.ends_with(".log"));
    })
    .unwrap();
    println!(
        "{count} entries read in {:?}; NTUSER.DAT x{ntuser}; service logs x{service_logs}",
        started.elapsed()
    );
    assert_eq!(read, header);
    assert!(count > 100_000, "only {count} entries");
    assert_eq!(service_logs, 0, "LocalSystem's profile leaked into my view");
    assert!(
        fs::OpenOptions::new().write(true).open(&path).is_err(),
        "the snapshot must be read-only for me"
    );

    let name = format!("seekr-live-{}.txt", std::process::id());
    let probe = std::env::temp_dir().join(&name);
    let written = Instant::now();
    fs::write(&probe, "x").unwrap();
    let deadline = written + Duration::from_secs(10);
    let mut seen = None;
    while seen.is_none() && Instant::now() < deadline {
        if let Message::Changes { records, .. } = wire::read_message(&mut pipe).unwrap()
            && records.iter().any(|r| r.name == name)
        {
            seen = Some(written.elapsed());
        }
    }
    fs::remove_file(&probe).unwrap();
    println!("new file reported after {seen:?}");
    assert!(seen.is_some(), "no change record for {name}");
}
