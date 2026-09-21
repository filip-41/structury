//! Differential wrappers over the two scan doors and the DOM oracle.

use structury::{Demand, Strictness, Value, stitch};
use structury_json::{Dialect, JsonInput, Plan, scan};

use crate::common::{Divergences, answers, dom_array, observe_all, req, req_with, text};

/// Compare `scan` of the text array `src` against the DOM oracle over `items`.
pub(crate) fn check_text(bad: &mut Divergences, label: &str, src: &[u8], items: &[Value], demands: &[Demand]) {
    let got = observe_all(&answers(src, &req(JsonInput::Text, demands, Dialect::Rfc8259)));
    for (i, demand) in demands.iter().enumerate() {
        if let Some(want) = dom_array(items, demand)
            && got[i] != want
        {
            bad.push(format!(
                "[{label}] text src={} demand[{i}]={demand:?} got={:?} want={want:?}",
                String::from_utf8_lossy(src),
                got[i]
            ));
        }
    }
}

/// The stream door must answer exactly what the equivalent text array does.
pub(crate) fn check_stream(bad: &mut Divergences, label: &str, text: &[u8], stream: &[u8], demands: &[Demand]) {
    let from_text = observe_all(&answers(text, &req(JsonInput::Text, demands, Dialect::Rfc8259)));
    let from_stream = observe_all(&answers(stream, &req(JsonInput::Ndjson, demands, Dialect::Rfc8259)));
    for (i, demand) in demands.iter().enumerate() {
        if from_stream[i] != from_text[i] {
            bad.push(format!(
                "[{label}] stream src={} demand[{i}]={demand:?} stream={:?} text={:?}",
                String::from_utf8_lossy(stream),
                from_stream[i],
                from_text[i]
            ));
        }
    }
}

/// Serial vs plan+stitch at `target = 1`, so any parallel shape splits fully.
/// `"AGREE"`, `"BOTH ERR (same)"`, or a `DIVERGE ...` diagnostic.
pub(crate) fn sharded(src: &[u8], demands: &[Demand], dialect: Dialect) -> String {
    let request = req_with(JsonInput::Text, demands, Strictness::Structural, dialect);
    let serial = scan(src, &request);
    let shard: Result<String, String> = match Plan::build(src, &request) {
        Ok(plan) => {
            let ranges = plan.ranges(1);
            let mut parts = Vec::new();
            let mut error = None;
            for range in &ranges {
                match plan.scan(src, *range) {
                    Ok(part) => parts.push(part),
                    Err(failure) => {
                        error = Some(format!("{failure:?}"));
                        break;
                    }
                }
            }
            match error {
                Some(failure) => Err(failure),
                None => Ok(format!("{:?}", observe_all(&stitch(parts).answers))),
            }
        }
        Err(failure) => Err(format!("{failure:?}")),
    };
    match (&serial, &shard) {
        (Ok(serial), Ok(shard)) => {
            let serial = format!("{:?}", observe_all(&serial.answers));
            if &serial == shard {
                "AGREE".to_string()
            } else {
                format!("DIVERGE serial_ok={} shard_ok={}", trunc(&serial), trunc(shard))
            }
        }
        (Err(serial), Err(shard)) => {
            if format!("{serial:?}") == *shard {
                "BOTH ERR (same)".to_string()
            } else {
                format!("BOTH ERR DIFFERENT serial={serial:?} shard={shard}")
            }
        }
        (Err(serial), Ok(shard)) => format!("DIVERGE serial_err={serial:?} shard_ok={}", trunc(shard)),
        (Ok(serial), Err(shard)) => format!(
            "DIVERGE serial_ok={} shard_err={shard}",
            trunc(&format!("{:?}", observe_all(&serial.answers)))
        ),
    }
}

pub(crate) fn trunc(s: &str) -> &str {
    &s[..s.len().min(200)]
}
