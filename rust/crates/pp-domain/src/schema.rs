use crate::{Digest, WireInteger, WireText, build_identity::BuildName};
use schemars::{JsonSchema, r#gen::SchemaGenerator, schema::*};

impl<const MIN: u64, const MAX: u64> JsonSchema for WireInteger<MIN, MAX> {
    fn schema_name() -> String {
        format!("WireInteger_{MIN}_{MAX}")
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        SchemaObject {
            instance_type: Some(InstanceType::Integer.into()),
            // Keep safe-integer limits as integer JSON tokens. Schemars'
            // f64 NumberValidation produces decimal tokens that can round
            // differently when read by another JSON implementation.
            extensions: [
                ("minimum".into(), MIN.into()),
                ("maximum".into(), MAX.into()),
            ]
            .into(),
            ..Default::default()
        }
        .into()
    }
}
impl<const MIN: usize, const MAX: usize> JsonSchema for WireText<MIN, MAX> {
    fn schema_name() -> String {
        format!("WireText_{MIN}_{MAX}")
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        scalar_text(MIN as u32, MAX as u32)
    }
}
impl JsonSchema for Digest {
    fn schema_name() -> String {
        "Digest".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        SchemaObject {
            instance_type: Some(InstanceType::String.into()),
            string: Some(Box::new(StringValidation {
                pattern: Some("^[a-f0-9]{64}$".into()),
                ..Default::default()
            })),
            ..Default::default()
        }
        .into()
    }
}

/// A Build name as `BuildName::parse` takes it: 1 to 4096 Unicode scalars. The core also
/// trims JavaScript whitespace first, so a blank name passes this schema and is refused.
impl JsonSchema for BuildName {
    fn schema_name() -> String {
        "BuildName".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        scalar_text(1, 4096)
    }
}

/// A string of `min..=max` Unicode scalars, as the client generator reads `x-unicode-scalar`.
fn scalar_text(min: u32, max: u32) -> Schema {
    SchemaObject {
        instance_type: Some(InstanceType::String.into()),
        string: Some(Box::new(StringValidation {
            min_length: Some(min),
            max_length: Some(max),
            ..Default::default()
        })),
        extensions: [("x-unicode-scalar".into(), true.into())].into(),
        ..Default::default()
    }
    .into()
}
