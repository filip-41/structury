//! Scan a nested demand and read the located value.

use structury::{Demand, Step};
use structury_json::{Dialect, Form, JsonInput, MaterializeOptions, Materialized, ScanRequest, materialize, scan};

fn main() {
    let src = br#"{"users":[{"id":1,"name":"a"},{"id":2,"name":"b"}]}"#;
    let demands = [Demand::Path {
        steps: vec![Step::key("users"), Step::index(1), Step::key("id")],
        nested: None,
    }];
    let request = ScanRequest::new(JsonInput::Text, &demands);
    let result = scan(src, &request).expect("valid JSON");
    let materialized = materialize(
        &result.answers[0],
        MaterializeOptions::new(Dialect::Rfc8259, Form::Value),
    )
    .expect("located value");
    let Materialized::Value(value) = materialized else {
        unreachable!("Form::Value yields a value");
    };
    assert_eq!(value.as_i64(), Some(2));
    println!("users[1].id = {value}");
}
