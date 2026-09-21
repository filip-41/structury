//! Resolve an array index, including a negative one from the end.

use structury::{Value, resolve_index};

fn main() {
    let value = Value::Array(vec![Value::Bool(true), Value::Null]);
    assert_eq!(value.element(-1), Some(&Value::Null));
    assert_eq!(value.element(2), None);
    assert_eq!(resolve_index(3, -1), Some(2));
    println!("value = {value}");
}
