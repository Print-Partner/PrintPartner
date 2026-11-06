use anyhow::{Result, bail, ensure};

pub const MAX_JS_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
pub const MAX_PLATE_UM: u32 = 2_147_483_647;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct RequiredUnitToken(String);

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct PlateId(String);

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct Digest(String);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct OffsetUm(u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct DimensionUm(u32);

macro_rules! hex_identity {
    ($name:ident, $prefix:literal, $length:expr) => {
        impl $name {
            pub fn parse(value: impl Into<String>) -> Result<Self> {
                let value = value.into();
                ensure!(
                    value.len() == $length && value.starts_with($prefix),
                    "invalid {}",
                    stringify!($name)
                );
                ensure!(
                    value[$prefix.len()..]
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')),
                    "invalid {}",
                    stringify!($name)
                );
                Ok(Self(value))
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

hex_identity!(RequiredUnitToken, "ppu_", 36);
hex_identity!(PlateId, "plate_", 38);

impl PlateId {
    /// A Plate id as stored. Node generates `plate_` ids but keeps any trimmed
    /// text of 1 to 200 UTF-16 units (its `normalizedText`).
    pub fn stored(value: &str) -> Option<Self> {
        trimmed_text(value).map(Self)
    }
}

impl Digest {
    pub(crate) fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(hex::encode(bytes))
    }

    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        ensure!(
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')),
            "invalid digest"
        );
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl OffsetUm {
    pub fn parse(value: i64) -> Result<Self> {
        ensure!(
            (0..=i64::from(MAX_PLATE_UM)).contains(&value),
            "offset outside stored range"
        );
        Ok(Self(value as u32))
    }
    pub fn get(self) -> u32 {
        self.0
    }
}

impl DimensionUm {
    pub fn parse(value: i64) -> Result<Self> {
        ensure!(
            (1..=i64::from(MAX_PLATE_UM)).contains(&value),
            "dimension outside stored range"
        );
        Ok(Self(value as u32))
    }
    pub fn get(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedPlanBasis {
    pub profile_id: u64,
    pub plan_version: u64,
    pub plan_revision_id: u64,
    pub plan_revision_digest: Digest,
    pub required_unit_mapping_digest: Digest,
}

impl AcceptedPlanBasis {
    pub fn validate(&self) -> Result<()> {
        for value in [self.profile_id, self.plan_version, self.plan_revision_id] {
            ensure!(
                (1..=MAX_JS_SAFE_INTEGER).contains(&value),
                "basis id is not a positive JavaScript-safe integer"
            );
        }
        Ok(())
    }
}

pub fn is_ecmascript_whitespace(value: char) -> bool {
    matches!(
        value,
        '\u{0009}'..='\u{000d}'
            | '\u{0020}'
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

pub fn trim_ecmascript(value: &str) -> &str {
    value.trim_matches(is_ecmascript_whitespace)
}

pub fn trimmed_text(value: &str) -> Option<String> {
    let value = trim_ecmascript(value);
    (1..=200)
        .contains(&value.encode_utf16().count())
        .then(|| value.to_owned())
}

pub fn millimetres_to_micrometres(value: f64) -> Option<i64> {
    let converted = value * 1_000.0;
    (converted.is_finite()
        && converted.fract() == 0.0
        && converted.abs() <= MAX_JS_SAFE_INTEGER as f64)
        .then_some(converted as i64)
}

pub fn checked_js_id(value: u64) -> Result<u64> {
    if value == 0 || value > MAX_JS_SAFE_INTEGER {
        bail!("not a positive JavaScript-safe integer")
    }
    Ok(value)
}
