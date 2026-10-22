//! Mass file churn on a two-million-name index: how slots and search time change as files are
//! created and deleted, as a build or a package install does all day.
//! `cargo test --release -p fsindex --test churn -- --ignored --nocapture`

use std::time::Instant;

use fsindex::{index::Index, usn::Record};

const DELETE: u32 = 0x200;
const CREATE: u32 = 0x100;

fn file(frn: u64, parent: u64, name: String, reason: u32) -> Record {
    Record {
        frn,
        parent,
        usn: 0,
        reason,
        attributes: 0x20,
        name,
    }
}

fn synthetic(files: u64) -> Index {
    let folders = files / 100;
    let mut records: Vec<Record> = (0..folders)
        .map(|d| Record {
            frn: 1_000 + d,
            parent: 5,
            usn: 0,
            reason: 0,
            attributes: 0x10,
            name: format!("folder {d}"),
        })
        .collect();
    records.extend((0..files).map(|f| {
        file(
            10_000_000 + f,
            1_000 + f % folders,
            format!("report_{f}.pdf"),
            0,
        )
    }));
    Index::from_records(1, 0, records)
}

/// The median of five searches, in ms.
fn search_ms(index: &Index, query: &str) -> f64 {
    let mut ms: Vec<f64> = (0..5)
        .map(|_| {
            let start = Instant::now();
            index.search(query, None, 300, &|| false).unwrap();
            start.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    ms.sort_by(f64::total_cmp);
    ms[2]
}

#[test]
#[ignore]
fn churn() {
    let mut index = synthetic(2_000_000);
    println!(
        "start: {} names, {} slots, \"rpt\" {:.1} ms",
        index.len(),
        index.slots(),
        search_ms(&index, "rpt")
    );
    let mut next = 100_000_000u64;
    for round in 1..=4 {
        // A million temporary files created and deleted again.
        for _ in 0..1_000_000 {
            index.apply(&file(next, 1_000, format!("tmp{next}.obj"), CREATE));
            index.apply(&file(next, 1_000, format!("tmp{next}.obj"), DELETE));
            next += 1;
        }
        println!(
            "after {round} M created and deleted: {} names, {} slots, \"rpt\" {:.1} ms",
            index.len(),
            index.slots(),
            search_ms(&index, "rpt")
        );
    }
    assert_eq!(index.len(), 2_020_000);
    assert!(index.needs_compacting());
    let start = Instant::now();
    index.compact();
    println!(
        "compacted in {:?}: {} slots, \"rpt\" {:.1} ms",
        start.elapsed(),
        index.slots(),
        search_ms(&index, "rpt")
    );
    assert_eq!((index.len(), index.slots()), (2_020_000, 2_020_000));
    let hit = index
        .search("report_1999999", None, 1, &|| false)
        .unwrap()
        .top[0];
    assert_eq!(
        index.path('C', hit.entry),
        r"C:\folder 19999\report_1999999.pdf"
    );
}
