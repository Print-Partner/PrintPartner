#[cfg(feature = "desktop_rebuild")]
pub mod build_identity;
#[cfg(feature = "desktop_rebuild")]
pub mod features;
pub mod gcode_objects;
#[cfg(feature = "desktop_rebuild")]
pub mod print_file;
#[cfg(feature = "desktop_rebuild")]
pub mod printer_display_state;
pub mod slicer_grams;

use serde::{Deserialize, Deserializer, Serialize, de};
use std::fmt;

pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct WireInteger<const MIN: u64, const MAX: u64>(u64);

impl<'de, const MIN: u64, const MAX: u64> Deserialize<'de> for WireInteger<MIN, MAX> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct IntegerVisitor<const MIN: u64, const MAX: u64>;
        impl<'de, const MIN: u64, const MAX: u64> de::Visitor<'de> for IntegerVisitor<MIN, MAX> {
            type Value = WireInteger<MIN, MAX>;
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                write!(formatter, "a JSON integer in {MIN}..={MAX}")
            }
            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
                WireInteger::new(value).map_err(E::custom)
            }
            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
                let value = u64::try_from(value).map_err(|_| E::custom("integer_bounds"))?;
                self.visit_u64(value)
            }
            fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
                if !value.is_finite() || value.fract() != 0.0 {
                    return Err(E::custom("integer_type"));
                }
                if value < MIN as f64 || value > MAX as f64 {
                    return Err(E::custom("integer_bounds"));
                }
                self.visit_u64(value as u64)
            }
        }
        deserializer.deserialize_any(IntegerVisitor::<MIN, MAX>)
    }
}

pub type PositiveId = WireInteger<1, MAX_SAFE_INTEGER>;
pub type Version = WireInteger<0, MAX_SAFE_INTEGER>;
pub type Quantity = WireInteger<1, 10_000>;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct WireText<const MIN: usize, const MAX: usize>(String);

impl<'de, const MIN: usize, const MAX: usize> Deserialize<'de> for WireText<MIN, MAX> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Digest(String);

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub enum DraftState {
    Open,
    Abandoned,
    Consumed,
}

impl DraftState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Abandoned => "abandoned",
            Self::Consumed => "consumed",
        }
    }
}

impl<const MIN: u64, const MAX: u64> WireInteger<MIN, MAX> {
    pub fn new(value: u64) -> Result<Self, &'static str> {
        if (MIN..=MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err("integer_bounds")
        }
    }
    pub fn get(self) -> u64 {
        self.0
    }
}
impl<const MIN: usize, const MAX: usize> WireText<MIN, MAX> {
    pub fn new(value: String) -> Result<Self, &'static str> {
        if (MIN..=MAX).contains(&value.chars().count()) {
            Ok(Self(value))
        } else {
            Err("text_codepoint_bounds")
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl Digest {
    pub fn new(value: String) -> Result<Self, &'static str> {
        if value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            Ok(Self(value))
        } else {
            Err("digest_pattern")
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(feature = "codegen")]
mod schema;
