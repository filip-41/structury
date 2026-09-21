#![no_main]

//! JSON5/JSONC filters, projections, and oracles: the dialect and predicate
//! paths the whole-document targets never reach.

use libfuzzer_sys::fuzz_target;
use structury::{Demand, Name, Number, Oracle, Path, Predicate, Step, Strictness, Value};
use structury_json::{Dialect, JsonInput, ScanRequest, scan};
use structury_json_fuzz::MAX_INPUT;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT {
        return;
    }
    let selector = data.first().copied().unwrap_or(0);
    let dialect = match selector % 3 {
        0 => Dialect::Json5,
        1 => Dialect::Jsonc,
        _ => Dialect::Rfc8259,
    };
    let strictness = match (selector >> 2) % 3 {
        0 => Strictness::Strict,
        1 => Strictness::Structural,
        _ => Strictness::Lazy,
    };
    let rows: Name = "rows".into();
    let path = Path {
        steps: vec![Step::Key(rows.clone())],
    };
    let predicates = [
        Predicate::Eq {
            field: rows.clone(),
            value: Value::Str("a".into()),
        },
        Predicate::Gt {
            field: rows.clone(),
            value: Value::Number(Number::parse("1").expect("a one-digit literal parses")),
        },
        Predicate::And(
            Box::new(Predicate::Ne {
                field: rows.clone(),
                value: Value::Null,
            }),
            Box::new(Predicate::Not(Box::new(Predicate::Lt {
                field: rows.clone(),
                value: Value::Bool(true),
            }))),
        ),
    ];
    let demands = [
        Demand::Filter {
            path: path.clone(),
            predicate: predicates[usize::from(selector) % predicates.len()].clone(),
            project: vec!["name".into(), "n".into()],
        },
        Demand::Project {
            path,
            fields: vec!["name".into()],
        },
        Demand::Oracle(Oracle::MemberNames),
        Demand::Oracle(Oracle::Count),
        Demand::Whole,
    ];
    for facts in [false, true] {
        let req = ScanRequest::new(JsonInput::Text, &demands)
            .with_strictness(strictness)
            .with_dialect(dialect)
            .with_facts(facts);
        let _ = scan(data, &req);
    }
});
