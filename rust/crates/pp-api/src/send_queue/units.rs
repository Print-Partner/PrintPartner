//! The Checkoff units a send links (GRE-1896), read for the send sheet's
//! preview and for Add with `link_checkoff` through one function, [`link`],
//! so both name the same units.

use pp_storage::{
    read_model::{AcceptedRead, Credential, ReadClient},
    send_queue::units::{FileRead, SendUnits, send_units},
};
use serde::Serialize;
use std::{sync::atomic::AtomicBool, time::Duration};

/// What reading the file's object names came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SlicedFileRead {
    /// The names were read; none of them may match.
    Read,
    /// A binary G-code block uses a compression or encoding the core does
    /// not decode (Heatshrink, MeatPack), so its names are not known.
    Unsupported,
    /// The binary G-code structure or a block checksum is wrong.
    Unreadable,
}

/// One Part and how many of its units the send links.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct SendUnitsPart {
    pub part_id: i64,
    pub filename: String,
    pub count: u64,
}

/// `POST /api/v2/printer-send-queue/checkoff-preview`: the units Add with
/// `link_checkoff` links for this file and Build, read now.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct SendUnitsPreview {
    pub file: SlicedFileRead,
    pub unit_count: u64,
    /// Per Part, in the order the units were matched.
    pub parts: Vec<SendUnitsPart>,
    /// Object names that matched no unit still to print, in file order.
    pub unmatched: Vec<String>,
}

/// The units a send of `file` to Build `plan` links. `None` when the
/// caller's tenant has no such Build; an error when its Accepted Plan fails
/// its integrity checks.
pub(super) fn link(
    reads: &ReadClient,
    credential: Credential,
    plan: i64,
    file: &[u8],
    cancelled: &AtomicBool,
    wait: Duration,
) -> anyhow::Result<Option<SendUnits>> {
    let batch = reads.read(credential, &[plan], cancelled, wait)?;
    match batch.builds.first().map(|build| &build.accepted) {
        None | Some(AcceptedRead::Missing) => Ok(None),
        Some(AcceptedRead::IntegrityFailure { message, .. }) => anyhow::bail!("{message}"),
        // A Build with no Accepted Plan, or one still being repaired, has no units to link.
        Some(read) => Ok(Some(send_units(read, file))),
    }
}

pub(super) fn preview(units: SendUnits) -> SendUnitsPreview {
    SendUnitsPreview {
        file: match units.file {
            FileRead::Read => SlicedFileRead::Read,
            FileRead::Unsupported => SlicedFileRead::Unsupported,
            FileRead::Unreadable => SlicedFileRead::Unreadable,
        },
        unit_count: units.units.len() as u64,
        parts: units
            .parts
            .into_iter()
            .map(|(part_id, filename, count)| SendUnitsPart {
                part_id,
                filename,
                count,
            })
            .collect(),
        unmatched: units.unmatched,
    }
}
