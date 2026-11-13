use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct CaptureId(String);

impl CaptureId {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        ensure!(
            !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control),
            "Invalid capture identifier"
        );
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for CaptureId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl fmt::Debug for CaptureId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CaptureId([redacted])")
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RelativePathsTransitionV1 {
    NodeJsonArrayStringFilterBooleanV1,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum LabelSelectionV1 {
    TrimmedRelativeThenSlashFilenameThenOrdinalStlV1,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum BackslashBoundaryV1 {
    ReplaceWithSlashAtLabelBoundaryV1,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CanonicalPathsV1 {
    SourcePathNfcCaseFoldPrefixV1,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ArchiveLabelsV1 {
    NfcAtAcquisition,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct FilePolicyV1 {
    policy_version: u32,
    relative_paths_transition: RelativePathsTransitionV1,
    label_selection: LabelSelectionV1,
    backslash_boundary: BackslashBoundaryV1,
    canonical_paths: CanonicalPathsV1,
    max_raw_input_bytes: u64,
    max_prepared_bytes: u64,
    max_files: u32,
    max_multipart_parts: u32,
    max_metadata_bytes: u64,
}

impl FilePolicyV1 {
    fn fixed() -> Self {
        Self {
            policy_version: 1,
            relative_paths_transition:
                RelativePathsTransitionV1::NodeJsonArrayStringFilterBooleanV1,
            label_selection: LabelSelectionV1::TrimmedRelativeThenSlashFilenameThenOrdinalStlV1,
            backslash_boundary: BackslashBoundaryV1::ReplaceWithSlashAtLabelBoundaryV1,
            canonical_paths: CanonicalPathsV1::SourcePathNfcCaseFoldPrefixV1,
            max_raw_input_bytes: 268_435_456,
            max_prepared_bytes: 1_073_741_824,
            max_files: 10_000,
            max_multipart_parts: 10_001,
            max_metadata_bytes: 65_536,
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PolicyBindingV1<'a> {
    Files { policy: &'a FilePolicyV1 },
    Zip { policy: &'a ZipPolicyV1 },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ZipPolicyV1 {
    policy_version: u32,
    archive_labels: ArchiveLabelsV1,
    canonical_paths: CanonicalPathsV1,
    max_raw_input_bytes: u64,
    max_prepared_bytes: u64,
    max_zip_entries: u32,
}

impl ZipPolicyV1 {
    fn fixed() -> Self {
        Self {
            policy_version: 1,
            archive_labels: ArchiveLabelsV1::NfcAtAcquisition,
            canonical_paths: CanonicalPathsV1::SourcePathNfcCaseFoldPrefixV1,
            max_raw_input_bytes: 268_435_456,
            max_prepared_bytes: 1_073_741_824,
            max_zip_entries: 10_000,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum CapturedPayloadKindV1 {
    Files {
        paths: Vec<String>,
        policy: FilePolicyV1,
    },
    Zip {
        path: String,
        policy: ZipPolicyV1,
    },
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct CapturedPayloadV1(CapturedPayloadKindV1);

impl fmt::Debug for CapturedPayloadV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CapturedPayloadV1([redacted])")
    }
}

impl CapturedPayloadV1 {
    pub fn files(paths: Vec<String>) -> Self {
        Self(CapturedPayloadKindV1::Files {
            paths,
            policy: FilePolicyV1::fixed(),
        })
    }

    pub fn zip(path: String) -> Self {
        Self(CapturedPayloadKindV1::Zip {
            path,
            policy: ZipPolicyV1::fixed(),
        })
    }

    /// The captured ZIP's path inside the capture, when the input is one ZIP.
    pub fn zip_path(&self) -> Option<&str> {
        match &self.0 {
            CapturedPayloadKindV1::Zip { path, .. } => Some(path),
            CapturedPayloadKindV1::Files { .. } => None,
        }
    }

    pub fn input_version(&self) -> u32 {
        2
    }

    pub(super) fn requested_paths(&self) -> Vec<&str> {
        match &self.0 {
            CapturedPayloadKindV1::Files { paths, .. } => {
                paths.iter().map(String::as_str).collect()
            }
            CapturedPayloadKindV1::Zip { path, .. } => vec![path.as_str()],
        }
    }

    pub(super) fn policy_digest(&self) -> Result<super::Sha256Digest> {
        let binding = match &self.0 {
            CapturedPayloadKindV1::Files { policy, .. } => PolicyBindingV1::Files { policy },
            CapturedPayloadKindV1::Zip { policy, .. } => PolicyBindingV1::Zip { policy },
        };
        super::Sha256Digest::new(super::serialized_digest(&(
            "pp-source-acquisition-policy-v1",
            binding,
        ))?)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            match &self.0 {
                CapturedPayloadKindV1::Files { policy, .. } => policy == &FilePolicyV1::fixed(),
                CapturedPayloadKindV1::Zip { policy, .. } => policy == &ZipPolicyV1::fixed(),
            },
            "Unsupported capture policy"
        );
        Ok(())
    }
}
