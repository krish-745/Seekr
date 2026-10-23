//! `cargo bench -p fsindex`: fuzzy search over a synthetic two-million-entry index, about the
//! size of a well-used Windows C: drive.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use fsindex::{index::Index, usn::Record};

const WORDS: [&str; 16] = [
    "report", "invoice", "photo", "setup", "config", "notes", "draft", "budget", "readme",
    "module", "index", "backup", "thesis", "slides", "data", "build",
];
const EXTS: [&str; 8] = ["pdf", "docx", "jpg", "txt", "rs", "json", "xlsx", "dll"];

/// 20,000 folders under the root, 100 files each, names from a fixed xorshift sequence.
fn synthetic(files: usize) -> Index {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let folders = files / 100;
    let mut records = Vec::with_capacity(files + folders);
    for d in 0..folders as u64 {
        let w = WORDS[(next() % 16) as usize];
        records.push(Record {
            frn: 1_000 + d,
            parent: 5,
            usn: 0,
            reason: 0,
            attributes: 0x10,
            name: format!("{w} {d}"),
        });
    }
    for f in 0..files as u64 {
        let (a, b) = (WORDS[(next() % 16) as usize], WORDS[(next() % 16) as usize]);
        let ext = EXTS[(next() % 8) as usize];
        let name = format!("{a}_{b}_{}.{ext}", next() % 10_000);
        records.push(Record {
            frn: 10_000_000 + f,
            parent: 1_000 + f % folders as u64,
            usn: 0,
            reason: 0,
            attributes: 0x20,
            name,
        });
    }
    Index::from_records(1, 0, records)
}

fn bench(c: &mut Criterion) {
    let index = synthetic(2_000_000);
    let never = || false;
    for query in ["r", "report", "rpt inv", "thesis_draft.docx"] {
        c.bench_function(&format!("search 2M {query:?}"), |b| {
            b.iter(|| black_box(index.search(black_box(query), None, 300, &never)))
        });
    }
    // Typing one more letter: only the previous query's matches are scored.
    let previous = index.search("repor", None, 300, &never).unwrap();
    c.bench_function("search 2M \"report\" within \"repor\"", |b| {
        b.iter(|| black_box(index.search("report", Some(&previous.all), 300, &never)))
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
