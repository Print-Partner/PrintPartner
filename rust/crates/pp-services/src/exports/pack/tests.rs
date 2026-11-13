use super::*;

fn part(role: &str, path: &str, units: &[(u32, bool)]) -> PackPart {
    PackPart {
        role: role.into(),
        relative_path: path.into(),
        filename: path.rsplit('/').next().unwrap().into(),
        size: 10,
        units: units
            .iter()
            .map(|&(number, printed)| PackUnit { number, printed })
            .collect(),
    }
}

fn names(entries: &[Entry]) -> Vec<&str> {
    entries.iter().map(|entry| entry.name.as_str()).collect()
}

#[test]
fn names_each_unit_by_role_folder_and_number() {
    let parts = [
        part("primary", "frame/arm.stl", &[(1, false), (2, true)]),
        part("accent", "top.stl", &[(1, false)]),
        part("primary", "a b/c&d/x.stl", &[(12, false)]),
    ];
    let entries = layout(&parts, Scope::All, LIMITS).unwrap();
    assert_eq!(
        names(&entries),
        [
            "primary/frame/arm_01.stl",
            "primary/frame/arm_02.stl",
            "accent/_root/top_01.stl",
            "primary/a_b_c_d/x_12.stl",
        ]
    );
    assert_eq!(
        entries.iter().map(|e| e.part).collect::<Vec<_>>(),
        [0, 0, 1, 2]
    );
}

#[test]
fn remaining_leaves_out_printed_units() {
    let parts = [part(
        "primary",
        "arm.stl",
        &[(1, true), (2, false), (3, true)],
    )];
    let entries = layout(&parts, Scope::Remaining, LIMITS).unwrap();
    assert_eq!(names(&entries), ["primary/_root/arm_02.stl"]);
    let done = [part("primary", "arm.stl", &[(1, true)])];
    assert_eq!(
        layout(&done, Scope::Remaining, LIMITS),
        Err(PackRefusal::NothingToPack)
    );
    assert_eq!(layout(&done, Scope::All, LIMITS).unwrap().len(), 1);
}

#[test]
fn a_repeated_name_in_one_folder_gains_a_suffix() {
    // Two Sources can hold the same path; the folder is the same, so the
    // second Part's names collide with the first's.
    let parts = [
        part("primary", "parts/arm.stl", &[(1, false), (2, false)]),
        part("primary", "parts/arm.stl", &[(1, false)]),
        part("primary", "parts/ARM.stl", &[(1, false)]),
    ];
    let entries = layout(&parts, Scope::All, LIMITS).unwrap();
    assert_eq!(
        names(&entries),
        [
            "primary/parts/arm_01.stl",
            "primary/parts/arm_02.stl",
            "primary/parts/arm_01_2.stl",
            "primary/parts/ARM_01.stl",
        ]
    );
}

#[test]
fn caps_refuse_too_many_entries_and_too_many_bytes() {
    let parts = [part("primary", "arm.stl", &[(1, false), (2, false)])];
    let entries = Limits {
        entries: 1,
        bytes: LIMITS.bytes,
    };
    assert_eq!(
        layout(&parts, Scope::All, entries),
        Err(PackRefusal::TooManyEntries {
            entries: 2,
            limit: 1
        })
    );
    let exact = 22 + 2 * entry_bytes("primary/_root/arm_01.stl", 10);
    let fits = Limits {
        entries: 2,
        bytes: exact,
    };
    assert_eq!(layout(&parts, Scope::All, fits).unwrap().len(), 2);
    let over = Limits {
        entries: 2,
        bytes: exact - 1,
    };
    assert_eq!(
        layout(&parts, Scope::All, over),
        Err(PackRefusal::TooLarge {
            bytes: exact,
            limit: exact - 1
        })
    );
}

#[test]
fn the_archive_holds_each_entry_byte_for_byte() {
    let files: [(&str, &[u8]); 2] = [
        ("primary/_root/a_01.stl", b"solid a\n"),
        ("accent/x/b_01.stl", &[0, 1, 2, 255]),
    ];
    let mut zip = ZipStream::new(Vec::new());
    for (name, bytes) in files {
        zip.entry(name, &mut &*bytes).unwrap();
    }
    let archive = zip.finish().unwrap();
    let expected: u64 = 22
        + files
            .iter()
            .map(|(n, b)| entry_bytes(n, b.len() as u64))
            .sum::<u64>();
    assert_eq!(
        archive.len() as u64,
        expected,
        "layout's size is the archive's size"
    );

    let mut read = zip::ZipArchive::new(io::Cursor::new(archive)).unwrap();
    assert_eq!(read.len(), 2);
    for (index, (name, bytes)) in files.iter().enumerate() {
        let mut entry = read.by_index(index).unwrap();
        assert_eq!(entry.name().unwrap(), *name);
        assert_eq!(entry.compression(), zip::CompressionMethod::Stored);
        let mut got = Vec::new();
        entry.read_to_end(&mut got).unwrap();
        assert_eq!(got, *bytes);
    }
}

#[test]
fn a_reader_error_stops_the_archive() {
    struct Failing;
    impl Read for Failing {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("hash mismatch"))
        }
    }
    let mut zip = ZipStream::new(Vec::new());
    let error = zip
        .entry("primary/_root/a_01.stl", &mut Failing)
        .unwrap_err();
    assert_eq!(error.to_string(), "hash mismatch");
}

#[test]
fn the_file_name_carries_the_build_and_version() {
    assert_eq!(filename("Voron 2.4 Kit", 7), "voron-2-4-kit-v7-stl.zip");
}
