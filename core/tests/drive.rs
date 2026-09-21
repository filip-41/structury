//! The host drive: per-part scans, serial fallback, and control stops.

mod common;

use common::range;
use structury::{Answer, ColumnCell, Columns, Demand, Drive, Error, ErrorClass, Oracle, Path, ScanResult, Shard, Step};

fn part(rows: usize) -> ScanResult<'static> {
    let mut columns = Columns::new(b"", vec![String::from("$")]);
    for i in 0..rows {
        columns.push(ColumnCell::Span(range(i, i + 1)));
    }
    ScanResult::new(vec![Answer::Columns(columns)], Vec::new())
}

fn hitch() -> Error {
    Error::new(ErrorClass::Syntax, "syntax", "worker hitch", 0)
}

#[test]
fn a_hitch_reruns_serially_and_returns_the_serial_answer() {
    let drive = Drive::from(vec![range(0, 2), range(2, 4)]);
    let mut calls = 0;
    let out = drive
        .run(
            |_| {
                calls += 1;
                if calls == 2 {
                    return Err(hitch());
                }
                Ok(part(2))
            },
            || Ok(part(9)),
        )
        .expect("hitch falls back");
    let Answer::Columns(columns) = &out.answers[0] else {
        panic!("columns");
    };
    assert_eq!(columns.rows(), 9, "the serial answer served");
}

#[test]
fn a_control_stop_surfaces_without_a_serial_redrive() {
    let drive = Drive::from(vec![range(0, 2)]);
    let out = drive.run(
        |_| Err(Error::new(ErrorClass::Control, "cancelled", "request cancelled", 0)),
        || panic!("a control stop must not redrive"),
    );
    assert_eq!(out.unwrap_err().class(), ErrorClass::Control);
}

#[test]
fn a_clean_drive_stitches_the_parts() {
    let drive = Drive::from(vec![range(0, 2), range(2, 4)]);
    let out = drive.run(|_| Ok(part(2)), || panic!("no hitch")).expect("clean");
    let Answer::Columns(columns) = &out.answers[0] else {
        panic!("columns");
    };
    assert_eq!(columns.rows(), 4);
}

#[test]
fn the_shard_law_admits_only_parallel_demands() {
    let concat = Demand::Project {
        path: Path::key("rows"),
        fields: vec![String::from("id")],
    };
    let sum = Demand::Oracle(Oracle::Count);
    let serial = Demand::Oracle(Oracle::DescendCount);
    assert!(Drive::eligible(&[concat.clone(), sum.clone()]));
    assert!(!Drive::eligible(&[concat, serial]));
    assert_eq!(sum.shard(), Shard::Sum);
}

#[test]
fn nested_path_inherits_the_leaf_shard_kind() {
    let spine = |nested| Demand::Path {
        steps: vec![Step::Key(String::from("rows"))],
        nested: Some(Box::new(nested)),
    };
    assert!(Drive::eligible(&[spine(Demand::Oracle(Oracle::Count))]));
    assert!(!Drive::eligible(&[spine(Demand::Oracle(Oracle::Kind))]));
    assert!(!Drive::eligible(&[spine(Demand::Whole)]));
}

#[test]
fn an_empty_drive_returns_the_serial_answer() {
    let drive = Drive::from(Vec::new());
    let out = drive
        .run(|_| panic!("an empty drive has no parts to scan"), || Ok(part(4)))
        .expect("serial");
    let Answer::Columns(columns) = &out.answers[0] else {
        panic!("columns");
    };
    assert_eq!(columns.rows(), 4);
}

#[test]
fn a_serial_failure_surfaces() {
    let drive = Drive::from(Vec::new());
    let failure = Error::new(ErrorClass::Limit, "limit", "serial refused", 0);
    let out = drive.run(|_| panic!("an empty drive has no parts"), || Err(failure));
    assert_eq!(out.unwrap_err().class(), ErrorClass::Limit);
}
