//! Fixture and shape generators shared by the scan and control suites.

use structury::{CompactStr, Number, Value};

pub(crate) fn obj(fields: &[(&str, Value)]) -> Value {
    Value::Object(
        fields
            .iter()
            .map(|(name, value)| (CompactStr::from(*name), value.clone()))
            .collect(),
    )
}

pub(crate) fn num(text: &str) -> Value {
    Value::Number(Number::parse(text).expect("number"))
}

pub(crate) fn text_array(records: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(b'[');
    for (i, record) in records.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        out.extend_from_slice(record.as_bytes());
    }
    out.push(b']');
    out
}

pub(crate) fn ndjson(records: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for record in records {
        out.extend_from_slice(record.as_bytes());
        out.push(b'\n');
    }
    out
}

/// Element kinds for the demand-matrix differentials.
pub(crate) fn element_kinds() -> Vec<&'static str> {
    vec![
        r#"{"id":1,"n":5}"#,
        r#"{"id":2}"#,
        r#"{"n":-1,"x":1}"#,
        r#"{"a":{"b":{"c":{"d":1}}}}"#,
        r#"{"a":{"b":{"c":{}}}}"#,
        r#"{"a":{"b":{"c":[{"d":7}]}}}"#,
        r#"{"a":{"b":5}}"#,
        r#"{"x":[{"id":1},{"id":2}]}"#,
        r#"{"id":2,"id":3}"#,
        "5",
        r#""s""#,
        "true",
        "null",
        "[1,2]",
        "[[1],[2]]",
        r#"[{"id":9}]"#,
        "{}",
        "[]",
    ]
}

pub(crate) fn big_array(rows: usize) -> String {
    use std::fmt::Write as _;
    let mut src = String::from("[");
    for i in 0..rows {
        if i > 0 {
            src.push(',');
        }
        let _ = write!(src, r#"{{"id":{i},"name":"row-{i}","active":true}}"#);
    }
    src.push(']');
    src
}

/// `{"users":[{"id":i,"score":i%100}, ...]}`.
pub(crate) fn users_array(rows: usize) -> Vec<u8> {
    let mut src = Vec::from(&b"{\"users\":["[..]);
    for i in 0..rows {
        if i > 0 {
            src.push(b',');
        }
        src.extend_from_slice(format!("{{\"id\":{i},\"score\":{}}}", i % 100).as_bytes());
    }
    src.extend_from_slice(b"]}");
    src
}

/// A deterministic LCG, so a fuzz counterexample is reproducible.
pub(crate) struct Lcg(pub(crate) u64);

impl Lcg {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0
    }

    pub(crate) fn pick(&mut self, n: u64) -> u64 {
        (self.next() >> 33) % n
    }
}
