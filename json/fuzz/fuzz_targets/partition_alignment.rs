#![no_main]

//! Partition helpers must not panic and ranges stay ordered.

use libfuzzer_sys::fuzz_target;
use structury_json::{partition_adjacent, partition_json_seq, partition_ndjson};
use structury_json_fuzz::MAX_INPUT;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT {
        return;
    }
    for part in partition_ndjson(data, 1024) {
        assert!(part.start() <= part.end());
        assert!(part.end() <= data.len());
    }
    for part in partition_adjacent(data, 1024) {
        assert!(part.start() <= part.end());
        assert!(part.end() <= data.len());
    }
    for part in partition_json_seq(data, 1024) {
        assert!(part.start() <= part.end());
        assert!(part.end() <= data.len());
    }
});
