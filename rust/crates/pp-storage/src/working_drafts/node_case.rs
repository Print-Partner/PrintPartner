//! Node's case mapping for builder globs and part keys.
//!
//! Node 22.23.3 and 24.21.0 lowercase and match `/i` with ICU 78.3 (Unicode 17.0);
//! `tests/fixtures/manifest-case-node.jsonl` records the result. Rust's std moves its case
//! tables with the toolchain. Up to std's Unicode 18.0 (Rust 1.100.0), std differs from Node
//! only on scalars Node's Unicode leaves unassigned, which Node treats as caseless: they
//! lowercase to themselves, no unit canonicalizes to or from them, and they end a
//! `Final_Sigma` context as a lone surrogate does. This module applies that rule to
//! [`UNASSIGNED_IN_NODE`] and takes every other mapping from std. The glob tests compare
//! the result with Node and fail when a newer std differs anywhere else.

use std::ops::RangeInclusive;

/// Scalars std cases that Node's Unicode 17.0 leaves unassigned (`/\p{Cn}/u` in Node).
///
/// Unicode 18.0 (Rust 1.100.0) assigned all of these. An older std leaves them unassigned
/// too, so an entry never changes results there. A scalar std assigns but does not case needs
/// no entry. For a newer std, add each scalar Node reports as `/\p{Cn}/u` for which std changes
/// `char::to_lowercase` or `char::to_uppercase`, or the `Final_Sigma` result of `"Α" + c + "Σ"`,
/// `c + "Σ"`, or `"ΑΣ" + c`.
const UNASSIGNED_IN_NODE: &[RangeInclusive<char>] = &[
    '\u{558}'..='\u{558}',
    '\u{58B}'..='\u{58C}',
    '\u{5C8}'..='\u{5C9}',
    '\u{B53}'..='\u{B54}',
    '\u{1ADE}'..='\u{1ADF}',
    '\u{1AEC}'..='\u{1AF0}',
    '\u{208F}'..='\u{208F}',
    '\u{209D}'..='\u{209F}',
    '\u{A7DD}'..='\u{A7DD}',
    '\u{A7E2}'..='\u{A7E2}',
    '\u{AB6C}'..='\u{AB6D}',
    '\u{107BB}'..='\u{107BF}',
    '\u{10EC9}'..='\u{10ECF}',
    '\u{10EF0}'..='\u{10EF9}',
    '\u{11DF0}'..='\u{11DF0}',
    '\u{1D127}'..='\u{1D128}',
    '\u{1D25B}'..='\u{1D25C}',
    '\u{1D6A6}'..='\u{1D6A6}',
    '\u{1DF1F}'..='\u{1DF24}',
    '\u{1DF2B}'..='\u{1DF7F}',
    '\u{1DF90}'..='\u{1DF96}',
    '\u{1DFCD}'..='\u{1DFFF}',
];

// The binary search in `unassigned_in_node` needs sorted, disjoint ranges.
const _: () = {
    let mut i = 1;
    while i < UNASSIGNED_IN_NODE.len() {
        assert!(*UNASSIGNED_IN_NODE[i - 1].end() < *UNASSIGNED_IN_NODE[i].start());
        i += 1;
    }
};

/// `String.prototype.toLowerCase` on a JS string, which may hold lone surrogates.
pub(super) fn lowercase_units(units: impl IntoIterator<Item = u16>) -> Vec<u16> {
    let units = units.into_iter();
    let mut lower = Vec::with_capacity(units.size_hint().0);
    let mut run = String::new();
    for decoded in char::decode_utf16(units) {
        match decoded {
            Ok(c) if !unassigned_in_node(c) => run.push(c),
            Ok(c) => {
                flush(&mut run, &mut lower);
                lower.extend_from_slice(c.encode_utf16(&mut [0; 2]));
            }
            Err(lone) => {
                flush(&mut run, &mut lower);
                lower.push(lone.unpaired_surrogate());
            }
        }
    }
    flush(&mut run, &mut lower);
    lower
}

/// `String.prototype.toLowerCase` on well-formed text.
pub(in crate::working_drafts) fn lowercase(text: &str) -> String {
    // Well-formed text lowercases to well-formed units, so nothing is replaced.
    String::from_utf16_lossy(&lowercase_units(text.encode_utf16()))
}

/// `Canonicalize` for a RegExp with the `i` flag and without `u`: two units match when
/// they canonicalize equally.
pub(super) fn canonicalize(unit: u16) -> u16 {
    let Some(c) = char::from_u32(u32::from(unit)) else {
        return unit;
    };
    let mut uppercase = c.to_uppercase();
    let Some(upper) = uppercase.next() else {
        return unit;
    };
    if uppercase.next().is_some()
        || upper.len_utf16() != 1
        || unassigned_in_node(c)
        || unassigned_in_node(upper)
    {
        return unit;
    }
    let upper = upper as u16;
    if unit >= 0x80 && upper < 0x80 {
        unit
    } else {
        upper
    }
}

/// Lowercases the run with std and empties it. The run holds only scalars Node assigns, so
/// std's `Final_Sigma` context inside it matches Node's.
fn flush(run: &mut String, lower: &mut Vec<u16>) {
    lower.extend(run.to_lowercase().encode_utf16());
    run.clear();
}

fn unassigned_in_node(c: char) -> bool {
    c >= *UNASSIGNED_IN_NODE[0].start() && {
        let at = UNASSIGNED_IN_NODE.partition_point(|range| *range.end() < c);
        UNASSIGNED_IN_NODE
            .get(at)
            .is_some_and(|range| range.contains(&c))
    }
}
