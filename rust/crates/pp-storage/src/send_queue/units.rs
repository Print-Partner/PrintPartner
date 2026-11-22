//! The Checkoff units a send links (GRE-1896): the object names in the
//! sliced file ([`pp_domain::sliced_objects::object_names`]) matched to the
//! Build's units still to print by Node's send-flow rule. The send sheet's
//! preview and Add with `link_checkoff` both use [`send_units`], so the
//! units the sheet shows are the units the queue item stores, and Add
//! refuses them with [`StaleUnits`] once they differ from the preview's.

use super::writes::checkoff_units;
use crate::read_model::{AcceptedRead, views::remaining_parts};
use pp_domain::sliced_objects::{FileObjects, RemainingPart, object_names, propose_units};
use serde_json::json;
use sha2::{Digest, Sha256};

/// At most this many units, in at most [`MAX_LINKED_BYTES`] of JSON, ride in
/// one send. The printer-upload job refuses a payload over 64 KiB or over
/// 1,000 units, and its other fields fit in what this leaves.
pub const MAX_LINKED_UNITS: usize = 1000;
pub const MAX_LINKED_BYTES: usize = 48 * 1024;

/// Units normalized as `checkoff_units` stores them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedUnits {
    pub json: String,
    pub count: usize,
}

impl LinkedUnits {
    /// Whether one send can carry these units to its printer-upload job.
    pub fn fits(&self) -> bool {
        self.count <= MAX_LINKED_UNITS && self.json.len() <= MAX_LINKED_BYTES
    }
}

/// What reading the file's object names came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileRead {
    /// The names were read; none of them may match.
    Read,
    /// A binary G-code block uses a compression or encoding the core does
    /// not decode, so the names are not known.
    Unsupported,
    /// The binary G-code structure or a block checksum is wrong.
    Unreadable,
    /// The file names more distinct objects than the core reads.
    TooManyObjects,
    /// The matched units are more than one send can carry.
    TooManyUnits,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendUnits {
    pub file: FileRead,
    /// The units linked, as `checkoff_units` stores them; `[]` for none.
    pub json: String,
    pub count: usize,
    /// `(part_id, filename, units linked)` per Part, in the order matched.
    pub parts: Vec<(i64, String, u64)>,
    /// Object names that took no unit, in file order.
    pub unmatched: Vec<String>,
    /// A digest of the Build, its Accepted Plan revision and the units
    /// linked. Add with `link_checkoff` must send the preview's.
    pub binding: String,
}

/// Add with `link_checkoff` named a preview whose units are no longer the
/// units the file links: a unit was printed or the Accepted Plan changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaleUnits;

/// The units a send of `file` to Build `plan` links against its Accepted
/// Plan read.
pub fn send_units(plan: i64, read: &AcceptedRead, file: &[u8]) -> SendUnits {
    let revision = match read {
        AcceptedRead::Ready { snapshot } => json!([
            snapshot.revision_digest,
            snapshot.required_unit_mapping_digest
        ]),
        _ => json!(null),
    };
    link_parts(plan, &revision, &remaining_parts(read), file)
}

/// [`send_units`] over the Build's units still to print, `remaining`, and
/// its Accepted Plan `revision`.
pub(crate) fn link_parts(
    plan: i64,
    revision: &serde_json::Value,
    remaining: &[RemainingPart],
    file: &[u8],
) -> SendUnits {
    let (mut file, names) = match object_names(file) {
        FileObjects::Read(names) => (FileRead::Read, names),
        FileObjects::Unsupported => (FileRead::Unsupported, Vec::new()),
        FileObjects::Unreadable => (FileRead::Unreadable, Vec::new()),
        FileObjects::TooMany => (FileRead::TooManyObjects, Vec::new()),
    };
    let mut proposal = propose_units(&names, remaining);
    let wire: Vec<_> = proposal
        .units
        .iter()
        .map(|u| json!({"part_id": u.part_id, "unit_index": u.unit_index, "object_name": u.object_name}))
        .collect();
    let mut linked = checkoff_units(&wire);
    if !linked.fits() {
        file = FileRead::TooManyUnits;
        proposal.units.clear();
        linked = checkoff_units(&[]);
    }
    let mut parts: Vec<(i64, String, u64)> = Vec::new();
    for unit in &proposal.units {
        match parts.iter_mut().find(|(id, ..)| *id == unit.part_id) {
            Some((.., count)) => *count += 1,
            None => {
                let filename = remaining
                    .iter()
                    .find(|part| part.part_id == unit.part_id)
                    .map(|part| part.filename.clone())
                    .unwrap_or_default();
                parts.push((unit.part_id, filename, 1));
            }
        }
    }
    let binding = hex::encode(Sha256::digest(
        json!([plan, revision, linked.json]).to_string(),
    ));
    SendUnits {
        file,
        json: linked.json,
        count: linked.count,
        parts,
        unmatched: proposal.unmatched,
        binding,
    }
}
