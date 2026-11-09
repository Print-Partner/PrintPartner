//! The slicer's filament estimate in grams, read from a sliced print file
//! (GRE-1165). PrusaSlicer, SuperSlicer, OrcaSlicer and BambuStudio write it
//! as a comment in text G-code, and PrusaSlicer's binary G-code keeps it in an
//! uncompressed metadata block. Cura writes only metres of filament, so its
//! files have no estimate here. Nothing is guessed: a file without a readable
//! gram value gives `None`.

/// Bytes read from each end of a text G-code file. Orca writes the estimate in
/// its header block and PrusaSlicer in the footer before its config dump.
const SCAN_BYTES: usize = 1024 * 1024;
const BGCODE_MAGIC: &[u8] = b"GCDE";
const BGCODE_GCODE_BLOCK: u16 = 1;
const BGCODE_PRINTER_METADATA: u16 = 3;
const BGCODE_PRINT_METADATA: u16 = 4;
const BGCODE_THUMBNAIL: u16 = 5;

/// The slicer's filament estimate for every extruder together.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Estimate {
    pub grams: f64,
    /// Grams per millimetre of filament: the estimated grams over the
    /// estimated length, when the file states a length too.
    pub grams_per_mm: Option<f64>,
}

/// The estimate the file states, if any.
pub fn estimate(bytes: &[u8]) -> Option<Estimate> {
    let found = if bytes.starts_with(BGCODE_MAGIC) {
        bgcode_metadata(bytes)
            .iter()
            .map(|text| from_lines(text.lines()))
            .find(|found| found.grams.is_some())?
    } else {
        let head = &bytes[..bytes.len().min(SCAN_BYTES)];
        let tail = &bytes[bytes.len().saturating_sub(SCAN_BYTES)..];
        [head, tail]
            .into_iter()
            .map(|part| from_lines(String::from_utf8_lossy(part).lines()))
            .find(|found| found.grams.is_some())?
    };
    let grams = found.grams.filter(|grams| positive(*grams))?;
    let grams_per_mm = found
        .mm
        .filter(|mm| positive(*mm))
        .map(|mm| grams / mm)
        .filter(|ratio| positive(*ratio));
    Some(Estimate {
        grams,
        grams_per_mm,
    })
}

fn positive(value: f64) -> bool {
    value.is_finite() && value > 0.0
}

#[derive(Default)]
struct Found {
    grams: Option<f64>,
    mm: Option<f64>,
}

/// The best gram and length values among `lines`: a total first, else the
/// per-extruder values summed.
fn from_lines<'a>(lines: impl Iterator<Item = &'a str>) -> Found {
    let mut slots: [Option<f64>; 5] = [None; 5];
    for line in lines {
        let line = line.trim_start_matches(';').trim();
        let Some((key, value)) = line.split_once(['=', ':']) else {
            continue;
        };
        let slot = match key.trim() {
            "total filament used [g]" => 0,
            "total filament weight [g]" => 1,
            "filament used [g]" => 2,
            "total filament length [mm]" => 3,
            "filament used [mm]" => 4,
            _ => continue,
        };
        if slots[slot].is_none() {
            slots[slot] = sum(value);
        }
    }
    Found {
        grams: slots[0].or(slots[1]).or(slots[2]),
        mm: slots[3].or(slots[4]),
    }
}

/// `1.5, 2` as 3.5. Any value that is not a number, or is negative, spoils
/// the sum.
fn sum(values: &str) -> Option<f64> {
    values
        .split(',')
        .map(|value| {
            value
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|value| *value >= 0.0)
        })
        .sum()
}

/// The text of the uncompressed print and printer metadata blocks of a binary
/// G-code file, print metadata first. Reading stops at the first G-code block
/// or at anything malformed.
fn bgcode_metadata(bytes: &[u8]) -> Vec<String> {
    let u16_at = |at: usize| Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?));
    let u32_at = |at: usize| Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    let mut found = Vec::new();
    let Some(checksum) = u16_at(8) else {
        return found;
    };
    let checksum_bytes = if checksum == 1 { 4 } else { 0 };
    let mut at = 10;
    while let (Some(kind), Some(compression), Some(size)) =
        (u16_at(at), u16_at(at + 2), u32_at(at + 4))
    {
        if kind == BGCODE_GCODE_BLOCK {
            break;
        }
        let (header, stored) = match compression {
            0 => (8, size),
            _ => match u32_at(at + 8) {
                Some(compressed) => (12, compressed),
                None => break,
            },
        };
        let parameters = if kind == BGCODE_THUMBNAIL { 6 } else { 2 };
        let data = at + header + parameters;
        let Some(end) = data.checked_add(stored as usize) else {
            break;
        };
        let Some(block) = bytes.get(data..end) else {
            break;
        };
        if compression == 0 && matches!(kind, BGCODE_PRINT_METADATA | BGCODE_PRINTER_METADATA) {
            let text = String::from_utf8_lossy(block).into_owned();
            match kind {
                BGCODE_PRINT_METADATA => found.insert(0, text),
                _ => found.push(text),
            }
        }
        at = end + checksum_bytes;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gre_1165_text_gcode_reads_each_slicers_comment() {
        let prusa = b"G28\nG1 X1\n; filament used [mm] = 1200.5\n; filament used [g] = 12.34\n; total filament used [g] = 12.34\n";
        assert_eq!(
            estimate(prusa),
            Some(Estimate {
                grams: 12.34,
                grams_per_mm: Some(12.34 / 1200.5)
            })
        );
        let multi = b"; filament used [g] = 1.5, 2.25, 0\nG28\n";
        assert_eq!(estimate(multi).map(|e| e.grams), Some(3.75));
        let total_wins = b"; filament used [g] = 1, 2\n; total filament used [g] = 4.5\n";
        assert_eq!(estimate(total_wins).map(|e| e.grams), Some(4.5));
        let orca = b"; HEADER_BLOCK_START\n; total filament length [mm] : 2600.00\n; total filament weight [g] : 7.80\n; HEADER_BLOCK_END\nG28\n";
        let orca_estimate = estimate(orca).unwrap();
        assert_eq!(orca_estimate.grams, 7.8);
        assert!((orca_estimate.grams_per_mm.unwrap() - 0.003).abs() < 1e-12);
        let lengths = b"; filament used [mm] = 1000, 500\n; filament used [g] = 3, 1.5\n";
        assert_eq!(
            estimate(lengths),
            Some(Estimate {
                grams: 4.5,
                grams_per_mm: Some(0.003)
            })
        );
        let bad_length = b"; filament used [mm] = 0\n; filament used [g] = 3\n";
        assert_eq!(
            estimate(bad_length),
            Some(Estimate {
                grams: 3.0,
                grams_per_mm: None
            })
        );
        let crlf = b"; filament used [g] = 2.5\r\nG28\r\n";
        assert_eq!(estimate(crlf).map(|e| e.grams), Some(2.5));
    }

    #[test]
    fn gre_1165_text_gcode_reads_the_footer_of_a_large_file() {
        let mut file = b"; generated by PrusaSlicer\n".to_vec();
        file.extend(std::iter::repeat_n(b"G1 X1 Y1 E0.1\n".as_slice(), 200_000).flatten());
        file.extend(b"; filament used [g] = 31.2\n; prusaslicer_config = begin\n");
        assert!(file.len() > 2 * SCAN_BYTES);
        assert_eq!(estimate(&file).map(|e| e.grams), Some(31.2));
    }

    #[test]
    fn gre_1165_no_gram_value_is_none() {
        for file in [
            b";Filament used: 1.23m\nG28\n".as_slice(),
            b"G28\nG1 X10\n",
            b"; filament used [g] = 0\n",
            b"; filament used [g] = -2\n",
            b"; filament used [g] = 1.5, n/a\n",
            b"; filament used [g] = 5, -1\n",
            b"; filament used [g] = NaN\n",
            b"; filament used [g] = inf\n",
            b"",
        ] {
            assert_eq!(estimate(file), None, "{:?}", String::from_utf8_lossy(file));
        }
    }

    /// A binary G-code block: header, encoding parameter, data, CRC bytes.
    fn block(kind: u16, compression: u16, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(kind.to_le_bytes());
        out.extend(compression.to_le_bytes());
        out.extend((data.len() as u32).to_le_bytes());
        if compression != 0 {
            out.extend((data.len() as u32).to_le_bytes());
        }
        let parameters: &[u8] = if kind == BGCODE_THUMBNAIL {
            &[0; 6]
        } else {
            &[0; 2]
        };
        out.extend(parameters);
        out.extend(data);
        out.extend([0; 4]);
        out
    }

    fn bgcode(blocks: &[Vec<u8>]) -> Vec<u8> {
        let mut out = b"GCDE".to_vec();
        out.extend(1u32.to_le_bytes());
        out.extend(1u16.to_le_bytes());
        blocks.iter().for_each(|b| out.extend(b));
        out
    }

    #[test]
    fn gre_1165_binary_gcode_reads_uncompressed_metadata() {
        let file = bgcode(&[
            block(0, 0, b"Producer=PrusaSlicer 2.8.0\n"),
            block(
                BGCODE_PRINTER_METADATA,
                0,
                b"printer_model=MK4\nfilament used [g]=99\n",
            ),
            block(BGCODE_THUMBNAIL, 0, b"png"),
            block(
                BGCODE_PRINT_METADATA,
                0,
                b"filament used [mm]=4000\nfilament used [g]=11.5\n",
            ),
            block(BGCODE_GCODE_BLOCK, 0, b"; filament used [g] = 50\n"),
        ]);
        assert_eq!(
            estimate(&file),
            Some(Estimate {
                grams: 11.5,
                grams_per_mm: Some(11.5 / 4000.0)
            })
        );
        let printer_only = bgcode(&[block(
            BGCODE_PRINTER_METADATA,
            0,
            b"filament used [g]=3.25\n",
        )]);
        assert_eq!(estimate(&printer_only).map(|e| e.grams), Some(3.25));
    }

    #[test]
    fn gre_1165_binary_gcode_never_guesses() {
        let compressed = bgcode(&[block(BGCODE_PRINT_METADATA, 1, b"filament used [g]=11.5\n")]);
        assert_eq!(estimate(&compressed), None);
        let after_gcode = bgcode(&[
            block(BGCODE_GCODE_BLOCK, 0, b"G28\n"),
            block(BGCODE_PRINT_METADATA, 0, b"filament used [g]=11.5\n"),
        ]);
        assert_eq!(estimate(&after_gcode), None);
        let mut truncated = bgcode(&[block(BGCODE_PRINT_METADATA, 0, b"filament used [g]=11.5\n")]);
        truncated.truncate(truncated.len() - 12);
        assert_eq!(estimate(&truncated), None);
        assert_eq!(estimate(b"GCDE"), None);
    }
}
