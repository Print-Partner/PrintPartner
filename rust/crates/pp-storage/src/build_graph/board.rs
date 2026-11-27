use super::create_with_brief::save;
use crate::{
    SettingsClient, WriterFailure,
    auth::{AuthPolicy, catalog_timestamp},
    read_model::{Credential, ReadClient},
    write::authorize,
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{sync::atomic::AtomicBool, time::Duration};

const PREFIX: &str = "board_notes:";
const MAX_NOTES: usize = 20;
const MAX_BODY_BYTES: usize = 2_000;

pub(super) fn key(id: i64) -> String {
    format!("{PREFIX}{id}")
}

#[derive(Deserialize, Serialize)]
pub struct Note {
    pub id: String,
    pub body: String,
    pub created_at: String,
}

pub struct Entry {
    pub build_id: i64,
    pub build_name: String,
    pub note: Note,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Build not found")]
    BuildNotFound,
    #[error("Enter a note of 1 to 2,000 UTF-8 bytes")]
    InvalidNote,
    #[error("This Build already has 20 Board notes")]
    BuildFull,
    #[error(transparent)]
    Storage(#[from] WriterFailure),
}

#[derive(Clone)]
pub struct Client {
    storage: SettingsClient,
    policy: AuthPolicy,
}

impl ReadClient {
    pub fn board(&self) -> Client {
        Client {
            storage: self.storage.clone(),
            policy: self.policy,
        }
    }
}

impl Client {
    pub fn list(
        &self,
        credential: Credential,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Vec<Entry>, WriterFailure> {
        self.storage.read("board.list", cancelled, wait, |tx| {
            let tenant = authorize(tx, &credential, self.policy)?;
            let mut query = tx.prepare(
                "SELECT b.id, b.name, a.value
                 FROM build_profiles b JOIN app_settings a
                   ON a.tenant_id=b.tenant_id AND a.key=?2 || b.id
                 WHERE b.tenant_id=?1",
            )?;
            let mut rows = query.query(params![tenant.id(), PREFIX])?;
            let mut entries = Vec::new();
            while let Some(row) = rows.next()? {
                let build_id = row.get(0)?;
                let build_name: String = row.get(1)?;
                let raw = row.get_ref(2)?.as_str().map_err(anyhow::Error::from)?;
                let notes: Vec<Note> = serde_json::from_str(raw).map_err(anyhow::Error::from)?;
                entries.extend(notes.into_iter().map(|note| Entry {
                    build_id,
                    build_name: build_name.clone(),
                    note,
                }));
            }
            entries.sort_unstable_by(|a, b| {
                (&b.note.created_at, &b.note.id).cmp(&(&a.note.created_at, &a.note.id))
            });
            Ok(entries)
        })?
    }

    pub fn create(
        &self,
        credential: Credential,
        build_id: i64,
        body: String,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Entry, Error> {
        let policy = self.policy;
        self.storage
            .shared
            .write("board.create", cancelled, wait, move |tx, _| {
                let tenant = authorize(tx, &credential, policy)?;
                let body = body.trim();
                if body.is_empty()
                    || body.len() > MAX_BODY_BYTES
                    || body
                        .chars()
                        .any(|c| c.is_control() && !matches!(c, '\r' | '\n' | '\t'))
                {
                    return Err(Error::InvalidNote);
                }
                let key = key(build_id);
                let (build_name, raw): (String, Option<String>) = tx
                    .query_row(
                        "SELECT b.name, a.value FROM build_profiles b
                         LEFT JOIN app_settings a ON a.tenant_id=b.tenant_id AND a.key=?3
                         WHERE b.tenant_id=?1 AND b.id=?2",
                        params![tenant.id(), build_id, key],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()
                    .map_err(WriterFailure::from)?
                    .ok_or(Error::BuildNotFound)?;
                let notes: Vec<Note> = serde_json::from_str(raw.as_deref().unwrap_or("[]"))
                    .map_err(anyhow::Error::from)
                    .map_err(WriterFailure::from)?;
                if notes.len() >= MAX_NOTES {
                    return Err(Error::BuildFull);
                }
                let note = Note {
                    id: hex::encode(rand::random::<[u8; 16]>()),
                    body: body.into(),
                    created_at: catalog_timestamp(),
                };
                let mut stored: Vec<&Note> = notes.iter().collect();
                stored.push(&note);
                let value = serde_json::to_string(&stored)
                    .map_err(anyhow::Error::from)
                    .map_err(WriterFailure::from)?;
                save(tx, tenant.id(), &key, &value).map_err(WriterFailure::from)?;
                Ok(Entry {
                    build_id,
                    build_name,
                    note,
                })
            })?
    }
}
