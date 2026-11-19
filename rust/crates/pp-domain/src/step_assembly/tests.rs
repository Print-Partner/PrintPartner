use super::*;

static IDLE: AtomicBool = AtomicBool::new(false);

const NESTED: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../fixtures/step/nested-assembly.step"
));

const HEADER: &str = "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\nFILE_NAME('t','',(''),(''),'','','');\nFILE_SCHEMA(('AUTOMOTIVE_DESIGN'));\nENDSEC;\n";

fn within(text: &[u8], limits: Limits) -> Result<ProductStructure, StepError> {
    product_structure(text, limits, &IDLE)
}

fn read(text: &str) -> Result<ProductStructure, StepError> {
    within(text.as_bytes(), Limits::default())
}

fn nested() -> &'static str {
    std::str::from_utf8(NESTED).unwrap()
}

/// A minimal file: `body` between `DATA;` and `ENDSEC;`.
fn file(body: &str) -> String {
    format!("{HEADER}DATA;\n{body}\nENDSEC;\nEND-ISO-10303-21;\n")
}

/// One part: PRODUCT #n, formation #n+1, definition #n+2.
fn part(n: u64, id: &str, name: &str) -> String {
    let (formation, definition) = (n + 1, n + 2);
    format!(
        "#{n}=PRODUCT('{id}','{name}','',(#2));\n#{formation}=PRODUCT_DEFINITION_FORMATION('','',#{n});\n#{definition}=PRODUCT_DEFINITION('design','',#{formation},#3);\n"
    )
}

/// `record` and then one part.
fn with_part(record: &str) -> String {
    file(&format!("{record}\n{}", part(10, "P", "Part")))
}

/// The result as a short label, for tables of cases.
fn outcome(result: Result<ProductStructure, StepError>) -> String {
    match result {
        Ok(_) => "Ok".into(),
        Err(StepError::Malformed { .. }) => "Malformed".into(),
        Err(StepError::Unsupported { .. }) => "Unsupported".into(),
        Err(StepError::InvalidStructure { .. }) => "InvalidStructure".into(),
        Err(error) => format!("{error:?}"),
    }
}

/// The fixture with `from` replaced by `to`, once.
fn mutated(from: &str, to: &str) -> String {
    assert_eq!(nested().matches(from).count(), 1, "{from}");
    nested().replacen(from, to, 1)
}

#[rustfmt::skip]
fn usage(instance: Option<&str>, parent: u64, child: u64) -> Usage {
    Usage { instance: instance.map(Into::into), parent, child }
}

#[test]
fn reads_the_product_structure_of_a_six_level_assembly() {
    let structure = product_structure(NESTED, Limits::default(), &IDLE).unwrap();
    let name = |id: u64| structure.products[&id].1.as_str();
    assert_eq!(structure.products.len(), 8);
    let names = [name(10), name(60), name(70), name(80)];
    assert_eq!(names, ["Printer", "D\u{fc}se 0.4", "", "Bed; 'heated'"]);
    assert_eq!(structure.formations[&21], 20);
    let definitions: Vec<u64> = structure.definitions.keys().copied().collect();
    assert_eq!(definitions, [12, 22, 32, 42, 52, 62, 72, 82]);
    assert_eq!(structure.usages.len(), 9);
    assert_eq!(structure.usages[&100], usage(Some("Frame:1"), 12, 22));
    // An occurrence with no name is labelled by its id.
    assert_eq!(structure.usages[&107], usage(Some("NAUO8"), 22, 72));
}

#[test]
fn a_file_with_only_geometry_has_an_empty_structure() {
    let structure = read(&file("#1=CARTESIAN_POINT('',(0.,0.,0.));")).unwrap();
    assert_eq!(structure, ProductStructure::default());
}

/// Every construct the contract names is read and dropped without changing
/// what is kept from the one part beside it.
#[test]
#[rustfmt::skip]
fn each_supported_construct_leaves_the_result_unchanged() {
    let one = part(10, "P", "Part");
    let expected = read(&file(&one)).unwrap();
    assert_eq!(expected.products[&10], ("P".into(), "Part".into()));
    let header = |extra: &str| HEADER.replace("ENDSEC;", &format!("{extra}ENDSEC;"));
    let (product, rest) = one.split_once('\n').unwrap();
    let cases = [
        ("integers and reals", with_part("#1=X(0,-12,+3,1.,-2.5E-3,6.02E+23,7.E2);")),
        ("enumerations", with_part("#1=X(.T.,.F.,.U.,.NOT_KNOWN.,._A1.);")),
        ("binaries", with_part("#1=X(\"0\",\"3FA0\");")),
        ("omitted and derived", with_part("#1=X($,*,(*,$));")),
        ("an empty parameter list", with_part("#1=X();")),
        ("nested and empty lists", with_part("#1=X((),((1,2),(3,(4))));")),
        ("typed parameters", with_part("#1=X(LENGTH_MEASURE(1.),(RATIO(2.)),A(B((1))));")),
        ("references to any record", with_part("#1=X(#2,(#3,#99999));")),
        ("strings in a dropped record", with_part("#1=X('a''b;c \\Q\\ \u{e9}');")),
        ("a complex instance", with_part("#1=(A()B(1,'x')C((#2)));")),
        ("a user-defined keyword", with_part("#1=!MY_POINT(1.);")),
        ("comments between tokens", with_part("#1/**/=/* ; */X/**/(/**/1/**/,'a'/**/)/**/;")),
        ("tab, CR and LF", with_part("#1\t=\r\nX(\t1\r\n);")),
        ("spaces between tokens", with_part("#1 = X ( 1 , 2 ) ;")),
        ("a record longer than the buffer and the record cap", with_part(&format!("#1=X('{}');", "x;".repeat(200_000)))),
        ("blanks before the first ;", file(&one).replacen("ISO-10303-21;", "ISO-10303-21 /**/ ;", 1)),
        ("extra and user-defined header entities", format!("{}DATA;\n{one}ENDSEC;\nEND-ISO-10303-21;", header("FILE_POPULATION('S',*,());\n!NOTE('x');\n"))),
        ("two DATA sections with parameter lists", format!("{HEADER}DATA('a',('S'));\n{product}\nENDSEC;\nDATA('b',('S'));\n{rest}ENDSEC;\nEND-ISO-10303-21;")),
    ];
    for (name, text) in cases {
        assert_eq!(read(&text).ok().as_ref(), Some(&expected), "{name}");
    }
}

/// The kept records accept each form the contract names for them.
#[test]
fn kept_records_take_each_form_of_their_parameters() {
    let body = [
        // A comment counts as blank, and `$` is an empty name.
        "#10=PRODUCT('R',$,/* note */'',(#2));",
        "#11=PRODUCT_DEFINITION_FORMATION_WITH_SPECIFIED_SOURCE('','',#10,.MADE.);",
        "#12=PRODUCT_DEFINITION_WITH_ASSOCIATED_DOCUMENTS('','',#11,#3,(#4));",
        // A name folded across lines, with raw UTF-8 and a quote.
        "#20=PRODUCT('S','Sch\r\nr\u{e4}ube ''M3''','',(#2));",
        "#21=PRODUCT_DEFINITION_FORMATION('','',#20);",
        "#22=PRODUCT_DEFINITION('','',#21,#3);",
        "#30=NEXT_ASSEMBLY_USAGE_OCCURRENCE($,$,$,#12,#22,$);",
        "#31=NEXT_ASSEMBLY_USAGE_OCCURRENCE('U31',$,'',#12,#22,'d');",
    ]
    .join("\n");
    let structure = read(&file(&body)).unwrap();
    assert_eq!(structure.products[&10], ("R".into(), String::new()));
    assert_eq!(structure.products[&20].1, "Schr\u{e4}ube 'M3'");
    assert_eq!(structure.formations[&11], 10);
    assert_eq!(structure.definitions[&12], 11);
    assert_eq!(structure.usages[&30], usage(None, 12, 22));
    assert_eq!(structure.usages[&31], usage(Some("U31"), 12, 22));
}

#[test]
fn decodes_string_directives() {
    assert_eq!(decode("plain"), Ok("plain".into()));
    assert_eq!(decode("D\\X2\\00FC\\X0\\se"), Ok("D\u{fc}se".into()));
    assert_eq!(decode("\\X2\\D83DDE00\\X0\\"), Ok("\u{1f600}".into()));
    assert_eq!(decode("\\X4\\0001F600\\X0\\"), Ok("\u{1f600}".into()));
    assert_eq!(decode("caf\\X\\E9"), Ok("caf\u{e9}".into()));
    assert_eq!(decode("\\S\\i"), Ok("\u{e9}".into()));
    assert_eq!(decode("a\\\\b"), Ok("a\\b".into()));
    assert_eq!(decode("\\PA\\x"), Ok("x".into()));
    // Another code page would change what `\S\` means.
    assert_eq!(decode("\\PB\\x"), Err(true));
    #[rustfmt::skip]
    let bad = ["\\X2\\00F\\X0\\", "\\X2\\00FC", "\\X2\\00fc\\X0\\", "\\X2\\D83D\\X0\\", "\\X2\\\\X0\\", "\\X4\\00110000\\X0\\", "\\X\\E", "\\S\\", "\\Q\\", "end\\"];
    for bad in bad {
        assert_eq!(decode(bad), Err(false), "{bad}");
    }
}

#[test]
fn refuses_a_file_that_is_not_step() {
    for text in ["solid cube\n", "", " ISO-10303-21;", "ISO-10303-21 HEADER;"] {
        assert_eq!(outcome(read(text)), "NotStep", "{text}");
    }
}

/// Scorer finding on 4c2445ee: the first 42 lines of the fixture returned a
/// two-level tree as a success. Every proper prefix is refused.
#[test]
fn every_truncation_of_the_fixture_is_refused() {
    let whole = NESTED.trim_ascii_end();
    assert!(within(whole, Limits::default()).is_ok());
    for length in 0..whole.len() {
        match within(&whole[..length], Limits::default()) {
            Err(StepError::Incomplete | StepError::NotStep) => {}
            other => panic!("prefix of {length} bytes: {other:?}"),
        }
    }
}

#[test]
#[rustfmt::skip]
fn a_cut_at_each_kind_of_point_is_incomplete() {
    let text = nested();
    let at = |needle: &str| text.find(needle).unwrap();
    let end = at("END-ISO-10303-21;");
    let cuts = [
        ("mid-record", &text[..at("#104=") + 20]),
        ("mid-string", &text[..at("'Bed; ''heated''") + 6]),
        ("mid-keyword", &text[..at("#105=") + 10]),
        ("mid-number", &text[..at("(0.,0.,0.)") + 2]),
        ("between records", &text[..at("#105=")]),
        ("before ENDSEC", &text[..text.rfind("ENDSEC;").unwrap()]),
        ("without the END line", &text[..end]),
        ("mid END line", &text[..end + 8]),
        ("in a comment", &format!("{}/* never closed", &text[..end])),
        ("after a /", &format!("{}/", &text[..end])),
    ];
    for (name, cut) in cuts {
        assert_eq!(outcome(read(cut)), "Incomplete", "{name}");
    }
}

/// Everything outside the grammar, including the cases Sol found on d15ed842
/// and ca6b79fb, is refused before anything is returned.
#[test]
#[rustfmt::skip]
fn an_exchange_outside_the_grammar_is_malformed() {
    let text = nested();
    let header = &text[text.find("HEADER;").unwrap()..text.find("DATA;").unwrap()];
    let data = &text[text.find("DATA;").unwrap()..text.rfind("END-ISO").unwrap()];
    let reordered = HEADER.replace("FILE_NAME", "TMP").replace("FILE_SCHEMA", "FILE_NAME").replace("TMP", "FILE_SCHEMA");
    let mut invalid_utf8 = NESTED.to_vec();
    invalid_utf8[text.find("Gantry").unwrap()] = 0xff;
    let cases = [
        ("no HEADER section", text.replacen(header, "", 1)),
        ("no HEADER keyword", mutated("HEADER;\n", "")),
        ("HEADER with a parameter list (Sol)", mutated("HEADER;", "HEADER();")),
        ("ENDSEC with a parameter list (Sol)", mutated("ENDSEC;\nDATA;", "ENDSEC();\nDATA;")),
        ("END with a parameter list (Sol)", mutated("END-ISO-10303-21;", "END-ISO-10303-21();")),
        ("no FILE_DESCRIPTION (Sol)", mutated("FILE_DESCRIPTION(", "X_DESCRIPTION(")),
        ("no FILE_NAME (Sol)", HEADER.replace("FILE_NAME", "FILE_POPULATION") + "DATA;\nENDSEC;\nEND-ISO-10303-21;"),
        ("header entities out of order", reordered + "DATA;\nENDSEC;\nEND-ISO-10303-21;"),
        ("a header entity without a parameter list", mutated("ENDSEC;\nDATA;", "NOTE;\nENDSEC;\nDATA;")),
        ("no DATA section", format!("{HEADER}END-ISO-10303-21;")),
        ("a partial record after the end", format!("{text}#109=PROD")),
        ("text after the end", format!("{text}x;")),
        ("a missing ; between records", mutated("#72,$);\n#108=", "#72,$)\n#108=")),
        ("DATA before HEADER", format!("ISO-10303-21;\n{data}{header}END-ISO-10303-21;\n")),
        ("text between ENDSEC and DATA", mutated("ENDSEC;\nDATA;", "ENDSEC;\nNOTE('x');\nDATA;")),
        ("an entity in the header", mutated("ENDSEC;\nDATA;", "#1=X('');\nENDSEC;\nDATA;")),
        ("END inside DATA", mutated("ENDSEC;\nEND-ISO", "END-ISO")),
        ("a kept id used twice", mutated("#20=PRODUCT(", "#10=PRODUCT(")),
        ("a bare word in DATA", mutated("#100=", "STRAY;\n#100=")),
        ("text after a parameter list", mutated("#12,#22,$);\n#101", "#12,#22,$) x;\n#101")),
        ("an unclosed parameter list", mutated("#12,#82,$);", "#12,#82,$;")),
        ("#N=; (Sol)", with_part("#109=;")),
        ("a record without a parameter list (Sol)", with_part("#109=CARTESIAN_POINT;")),
        ("PRODUCT without a parameter list (Sol)", mutated("#20=PRODUCT('FRM-1','Frame','',(#2));", "#20=PRODUCT;")),
        ("no =", with_part("#1 X();")),
        ("no number", with_part("#=X();")),
        ("a lowercase keyword", with_part("#1=x();")),
        ("a lowercase section", mutated("DATA;", "data;")),
        ("two values without a comma", with_part("#1=X(1 2);")),
        ("a leading comma", with_part("#1=X(,1);")),
        ("a trailing comma", with_part("#1=X(1,);")),
        ("two values in a typed parameter", with_part("#1=X(LENGTH(1.,2.));")),
        ("an empty typed parameter", with_part("#1=X(LENGTH());")),
        ("a typed parameter without a list", with_part("#1=X(LENGTH);")),
        ("an empty complex instance", with_part("#1=();")),
        ("a complex part without a list", with_part("#1=(A B());")),
        ("a real with no exponent digits", with_part("#1=X(1.E);")),
        ("a lowercase exponent", with_part("#1=X(1.e5);")),
        ("a sign alone", with_part("#1=X(-);")),
        ("a lowercase enumeration", with_part("#1=X(.t.);")),
        ("an empty enumeration", with_part("#1=X(..);")),
        ("a binary with a bad first digit", with_part("#1=X(\"4F\");")),
        ("a binary with a non-hex digit", with_part("#1=X(\"0G\");")),
        ("a control byte in a string", with_part("#1=X('a\u{1}b');")),
        ("a stray /", with_part("#1=X(1)/;")),
        ("an unknown character", with_part("#1=X(%);")),
        ("! without a keyword", with_part("#1=!();")),
        ("a bad directive in a kept string", mutated("'Gantry','',", "'Gan\\Q\\try','',")),
        ("lowercase hex in a kept string", mutated("\\X2\\00FC\\X0\\", "\\X2\\00fc\\X0\\")),
    ];
    for (name, text) in cases {
        assert_eq!(outcome(read(&text)), "Malformed", "{name}");
    }
    // Bytes that are not UTF-8 in a kept string.
    assert_eq!(outcome(within(&invalid_utf8, Limits::default())), "Malformed");
    // Whitespace, CRLF and closed comments after the end keep the result.
    let trailing = format!("{text}\r\n  /* closed */\r\n");
    assert_eq!(read(&trailing).unwrap(), read(text).unwrap());
}

#[test]
#[rustfmt::skip]
fn edition_3_forms_this_reader_does_not_read_are_unsupported() {
    let one = part(10, "P", "Part");
    let section = |name: &str| format!("{HEADER}{name};\nENDSEC;\nDATA;\n{one}ENDSEC;\nEND-ISO-10303-21;");
    let cases = [
        ("an ANCHOR section", section("ANCHOR")),
        ("a REFERENCE section", section("REFERENCE")),
        ("a SIGNATURE section", file(&one) + "SIGNATURE;\nENDSEC;\n"),
        ("a value instance name", with_part("#1=X(@2);")),
        ("a constant instance name", with_part("#1=X(#PI);")),
        ("a URI reference", with_part("#1=X(<a.stp#2>);")),
        ("a kept kind in a complex instance", with_part("#1=(A()PRODUCT('Q','Q','',(#2)));")),
        ("another code page in a kept string", mutated("'Gantry','',", "'\\PB\\Gantry','',")),
    ];
    for (name, text) in cases {
        assert_eq!(outcome(read(&text)), "Unsupported", "{name}");
    }
}

/// Sol on ca6b79fb: `#20=PRODUCT;` changed the tree. A kept record with the
/// wrong parameters, or a reference to anything but a kept record of the
/// right kind, is refused.
#[test]
#[rustfmt::skip]
fn a_kept_record_with_the_wrong_parameters_or_references_is_invalid() {
    let cases = [
        ("PRODUCT with 3 parameters", mutated("'Gantry','',(#2))", "'Gantry',(#2))")),
        ("PRODUCT with $ for its id", mutated("('GNT-1',", "($,")),
        ("PRODUCT with a number for its name", mutated("'Gantry'", "1")),
        ("a formation of a geometry record", mutated("'',#30);", "'',#4);")),
        ("a formation with $ for its product", mutated("'',#30);", "'',$);")),
        ("a definition of a missing formation", mutated("'',#31,#3)", "'',#39,#3)")),
        ("an occurrence of a product, not a definition", mutated("#12,#22,$", "#12,#20,$")),
        ("an occurrence of a missing definition", mutated("#12,#22,$", "#12,#999,$")),
        ("an occurrence with 5 parameters", mutated("#12,#22,$)", "#12,#22)")),
        ("a definition with documents and 4 parameters", mutated("PRODUCT_DEFINITION('design','',#31", "PRODUCT_DEFINITION_WITH_ASSOCIATED_DOCUMENTS('design','',#31")),
        ("a plain formation with 4 parameters", mutated("FORMATION('','',#30)", "FORMATION('','',#30,.NOT_KNOWN.)")),
    ];
    for (name, text) in cases {
        assert_eq!(outcome(read(&text)), "InvalidStructure", "{name}");
    }
}

/// Each cap passes an input at the cap and refuses one a byte or record over
/// it.
#[test]
#[rustfmt::skip]
fn each_limit_accepts_its_cap_and_refuses_one_more() {
    type Set = fn(&mut Limits, usize);
    let one = part(10, "P", "Part");
    let base = file(&one);
    let longest_record = one.lines().map(str::len).max().unwrap();
    // 98 bytes and a doubled quote: 100 bytes as written.
    let string = with_part(&format!("#1=X('{}''');", "x".repeat(98)));
    let comment = with_part(&format!("/*{}*/", "x".repeat(96)));
    let nesting = with_part("#1=X(((A((1)))));");
    let cases: [(Limit, &str, usize, Set); 8] = [
        (Limit::FileBytes, &base, base.len(), |l, n| l.max_file_bytes = n as u64),
        (Limit::Entities, &base, 3, |l, n| l.max_entities = n as u64),
        (Limit::Nesting, &nesting, 5, |l, n| l.max_nesting = n),
        (Limit::StringBytes, &string, 100, |l, n| l.max_string_bytes = n),
        (Limit::CommentBytes, &comment, 100, |l, n| l.max_comment_bytes = n),
        (Limit::RecordBytes, &base, longest_record, |l, n| l.max_record_bytes = n),
        (Limit::Records, &base, 3, |l, n| l.max_records = n),
        // "P" and "Part".
        (Limit::KeptText, &base, 5, |l, n| l.max_kept_text = n),
    ];
    for (limit, text, cap, set) in cases {
        let mut limits = Limits::default();
        set(&mut limits, cap);
        assert_eq!(outcome(within(text.as_bytes(), limits)), "Ok", "{limit:?} at {cap}");
        set(&mut limits, cap - 1);
        assert_eq!(outcome(within(text.as_bytes(), limits)), format!("TooLarge({limit:?})"), "{limit:?} at {}", cap - 1);
    }
}

#[test]
#[rustfmt::skip]
fn keywords_and_instance_numbers_have_fixed_caps() {
    let record = |record: String| outcome(read(&with_part(&record)));
    let keyword = |length| record(format!("#1={}();", "K".repeat(length)));
    assert_eq!(keyword(MAX_KEYWORD), "Ok");
    assert_eq!(keyword(MAX_KEYWORD + 1), "TooLarge(KeywordBytes)");
    assert_eq!(record(format!("#{}=X();", u64::MAX)), "Ok");
    assert_eq!(record("#18446744073709551616=X();".into()), "TooLarge(InstanceNumber)");
}

/// Sol on d15ed842: the record cap counted only the buffered body. It counts
/// the whole record, `#` through `;`, comments included.
#[test]
#[rustfmt::skip]
fn the_record_cap_counts_the_whole_record() {
    let cap = Limits::default().max_record_bytes;
    // A PRODUCT record of exactly `length` bytes, with a comment inside.
    let product = |length: usize| {
        let name = "N".repeat(length - "#10=PRODUCT('P','',/**/'',(#2));".len());
        let record = format!("#10=PRODUCT('P','{name}',/**/'',(#2));");
        assert_eq!(record.len(), length);
        file(&format!("{record}\n#11=PRODUCT_DEFINITION_FORMATION('','',#10);\n#12=PRODUCT_DEFINITION('design','',#11,#3);"))
    };
    assert!(read(&product(cap)).is_ok());
    assert_eq!(outcome(read(&product(cap + 1))), "TooLarge(RecordBytes)");
}

/// A reader that sets `flag` once `after` bytes have been read, and counts
/// what it hands out in `at`.
struct Tripwire<'a> {
    bytes: &'a [u8],
    at: usize,
    after: usize,
    flag: &'a AtomicBool,
}

impl Read for Tripwire<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let count = out.len().min(self.bytes.len() - self.at).min(4096);
        out[..count].copy_from_slice(&self.bytes[self.at..self.at + count]);
        self.at += count;
        if self.at >= self.after {
            self.flag.store(true, Ordering::Relaxed);
        }
        Ok(count)
    }
}

/// Reads `text` through a [`Tripwire`]; answers the outcome and the bytes read.
fn tripwire(text: &str, after: usize, limits: Limits) -> (String, usize) {
    let flag = AtomicBool::new(after == 0);
    let (bytes, at) = (text.as_bytes(), 0);
    let mut reader = Tripwire {
        bytes,
        at,
        after,
        flag: &flag,
    };
    let result = product_structure(&mut reader, limits, &flag);
    (outcome(result), reader.at)
}

/// Sol on ca6b79fb: an 8 MiB comment inside a PRODUCT record was read in
/// full before the refusal. The read now stops within one buffer of the cap,
/// inside a kept record and in a dropped one.
#[test]
fn an_oversized_comment_is_refused_before_it_is_read() {
    let comment = format!("/*{}*/", "x".repeat(8 << 20));
    let product = format!("#10=PRODUCT('P',{comment}'Part','',(#2));");
    let geometry = format!("#1=X({comment});");
    for (limit, record) in [("RecordBytes", product), ("CommentBytes", geometry)] {
        let text = with_part(&record);
        let (result, read) = tripwire(&text, usize::MAX, Limits::default());
        assert_eq!(result, format!("TooLarge({limit})"));
        let most = text.find(&record).unwrap() + (64 << 10) + READ_CHUNK;
        assert!(read <= most, "{limit}: read {read} bytes");
    }
}

#[test]
fn cancellation_stops_the_read_within_one_chunk() {
    assert_eq!(
        tripwire(&with_part(""), 0, Limits::default()).0,
        "Cancelled"
    );
    let text = file(&format!("#1=POLYLINE('{}',(#2));", "x".repeat(8 << 20)));
    let limits = Limits {
        max_string_bytes: 16 << 20,
        ..Limits::default()
    };
    let (result, read) = tripwire(&text, 1 << 20, limits);
    assert_eq!(result, "Cancelled");
    assert!(read <= (1 << 20) + READ_CHUNK, "read {read} bytes");
}
