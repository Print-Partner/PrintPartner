//! The Checkoff units a send links (GRE-1896): the object names in the
//! sliced file ([`pp_domain::sliced_objects::object_names`]) matched to the
//! Build's units still to print by Node's send-flow rule. The send sheet's
//! preview and Add with `link_checkoff` both use [`send_units`], so the
//! units the sheet shows are the units the queue item stores.

use crate::read_model::{AcceptedRead, views::remaining_parts};
use pp_domain::sliced_objects::{FileObjects, ProposedUnit, object_names, propose_units};

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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendUnits {
    pub file: FileRead,
    pub units: Vec<ProposedUnit>,
    /// `(part_id, filename, units linked)` per Part, in the order matched.
    pub parts: Vec<(i64, String, u64)>,
    /// Object names that took no unit, in file order.
    pub unmatched: Vec<String>,
}

/// The units a send of `file` links against the Build's Accepted Plan read.
pub fn send_units(read: &AcceptedRead, file: &[u8]) -> SendUnits {
    let remaining = remaining_parts(read);
    let (file, names) = match object_names(file) {
        FileObjects::Read(names) => (FileRead::Read, names),
        FileObjects::Unsupported => (FileRead::Unsupported, Vec::new()),
        FileObjects::Unreadable => (FileRead::Unreadable, Vec::new()),
    };
    let proposal = propose_units(&names, &remaining);
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
    SendUnits {
        file,
        units: proposal.units,
        parts,
        unmatched: proposal.unmatched,
    }
}
