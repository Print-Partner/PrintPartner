#![cfg(feature = "desktop_rebuild")]
//! Replays the Node classifier goldens in
//! `fixtures/parity/print-file-classification/cases.json` through
//! `pp_domain::print_file`. Regenerate from `web/` with
//! `node --conditions=development --import tsx ../fixtures/parity/print-file-classification/generate.mjs`.
use pp_domain::print_file::{MAX_CLASSIFIABLE_BYTES, Rejection, classify};
use serde_json::{Value, json};

fn decode_base64(text: &str) -> Vec<u8> {
    let value = |byte: u8| match byte {
        b'A'..=b'Z' => u32::from(byte - b'A'),
        b'a'..=b'z' => u32::from(byte - b'a') + 26,
        b'0'..=b'9' => u32::from(byte - b'0') + 52,
        b'+' => 62,
        b'/' => 63,
        _ => panic!("not base64: {byte}"),
    };
    let mut out = Vec::new();
    for chunk in text.trim_end_matches('=').as_bytes().chunks(4) {
        let bits = chunk.iter().enumerate().fold(0, |bits, (index, &byte)| {
            bits | value(byte) << (18 - 6 * index)
        });
        out.extend(bits.to_be_bytes()[1..chunk.len()].iter());
    }
    out
}

#[test]
fn print_file_classification_matches_every_node_golden() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../fixtures/parity/print-file-classification/cases.json"
    );
    let golden: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let cases = golden["cases"].as_array().unwrap();
    assert!(cases.len() >= 70, "{} cases", cases.len());
    let mut mismatches = Vec::new();
    for case in cases {
        let bytes = decode_base64(case["bytes"].as_str().unwrap());
        let actual = match classify(&bytes) {
            Ok(classified) => json!({
                "outcome": "classified",
                "classification": classified.classification,
                "sha256": classified.sha256,
                "size_bytes": classified.size_bytes,
                "next_action": classified.classification.next_action(),
            }),
            Err(reason) => json!({
                "outcome": "rejected",
                "reason": reason,
                "message": reason.message(),
            }),
        };
        if actual != case["expected"] {
            mismatches.push(format!("{}: {actual} != {}", case["id"], case["expected"]));
        }
    }
    assert!(mismatches.is_empty(), "{mismatches:#?}");
}

#[test]
fn print_file_classification_refuses_past_the_size_cap_and_reports_readiness() {
    let mut big = vec![b'G'; MAX_CLASSIFIABLE_BYTES + 1];
    big[1] = b'1';
    assert_eq!(classify(&big), Err(Rejection::FileTooLarge));
    big.truncate(MAX_CLASSIFIABLE_BYTES);
    let classified = classify(&big).unwrap();
    assert!(classified.classification.is_print_ready());
    assert_eq!(classified.size_bytes, MAX_CLASSIFIABLE_BYTES as u64);
}
