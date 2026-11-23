//! Node's case mapping for builder globs and part keys.
//!
//! Node 22.23.3 and 24.21.0 lowercase and match `/i` with ICU 78.3 (Unicode 17.0);
//! `tests/fixtures/manifest-case-node.jsonl` records the result. This module takes every
//! mapping from Rust's std, and the glob tests compare the result with Node.

/// `String.prototype.toLowerCase` on a JS string, which may hold lone surrogates.
pub(super) fn lowercase_units(units: impl IntoIterator<Item = u16>) -> Vec<u16> {
    let units = units.into_iter();
    let mut lower = Vec::with_capacity(units.size_hint().0);
    let mut run = String::new();
    for decoded in char::decode_utf16(units) {
        match decoded {
            Ok(c) => run.push(c),
            Err(lone) => {
                flush(&mut run, &mut lower);
                lower.push(lone.unpaired_surrogate());
            }
        }
    }
    flush(&mut run, &mut lower);
    lower
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
    if uppercase.next().is_some() || upper.len_utf16() != 1 {
        return unit;
    }
    let upper = upper as u16;
    if unit >= 0x80 && upper < 0x80 {
        unit
    } else {
        upper
    }
}

/// Lowercases the run with std and empties it.
fn flush(run: &mut String, lower: &mut Vec<u16>) {
    lower.extend(run.to_lowercase().encode_utf16());
    run.clear();
}
