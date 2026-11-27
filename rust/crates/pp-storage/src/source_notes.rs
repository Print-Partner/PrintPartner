//! Imports one confirmed guide note for a Source in the caller's installation.

use crate::{
    SettingsClient, WriterFailure,
    auth::AuthPolicy,
    read_model::{Credential, ReadClient},
    write,
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{sync::atomic::AtomicBool, time::Duration};

pub const MAX_TITLE_BYTES: usize = 300;
pub const MAX_BODY_BYTES: usize = 20_000;

#[derive(Deserialize, Serialize)]
pub struct Source {
    id: i64,
    name: String,
}

impl Source {
    pub fn id(&self) -> i64 {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

pub struct Draft {
    title: String,
    body_markdown: String,
}

impl Draft {
    pub fn new(title: &str, body: &str) -> Option<Self> {
        if title.len() > MAX_TITLE_BYTES || body.len() > MAX_BODY_BYTES {
            return None;
        }
        let title = title.trim();
        if title.is_empty()
            || title.chars().any(char::is_control)
            || body.trim().is_empty()
            || body
                .chars()
                .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\t' | '\r'))
        {
            return None;
        }
        Some(Self {
            title: title.into(),
            body_markdown: body.into(),
        })
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn body_markdown(&self) -> &str {
        &self.body_markdown
    }
}

#[derive(Clone)]
pub struct Client {
    storage: SettingsClient,
    policy: AuthPolicy,
}

impl ReadClient {
    pub fn source_notes(&self) -> Client {
        Client {
            storage: self.storage.clone(),
            policy: self.policy,
        }
    }
}

impl Client {
    pub fn find_source(
        &self,
        credential: Credential,
        name: &str,
        wait: Duration,
    ) -> Result<Option<Source>, WriterFailure> {
        self.storage.read(
            "source_notes.find_source",
            &AtomicBool::new(false),
            wait,
            |tx| {
                let tenant = write::authorize(tx, &credential, self.policy)?;
                let source = tx
                    .query_row(
                        "SELECT id, name FROM projects WHERE tenant_id=?1 AND name=?2",
                        params![tenant.id(), name],
                        |row| {
                            Ok(Source {
                                id: row.get(0)?,
                                name: row.get(1)?,
                            })
                        },
                    )
                    .optional()?;
                Ok(source)
            },
        )?
    }

    /// Returns whether the note changed, or None if the proposed Source no longer matches.
    pub fn import(
        &self,
        credential: Credential,
        source: Source,
        draft: Draft,
        wait: Duration,
    ) -> Result<Option<bool>, WriterFailure> {
        let policy = self.policy;
        self.storage.shared.write("source_notes.import", &AtomicBool::new(false), wait, move |tx, _| {
            let tenant = write::authorize(tx, &credential, policy)?;
            let found = tx.query_row(
                "SELECT 1 FROM projects WHERE tenant_id=?1 AND id=?2 AND name=?3",
                params![tenant.id(), source.id, source.name],
                |_| Ok(()),
            ).optional()?;
            if found.is_none() {
                return Ok(None);
            }
            let existing: Option<(i64, String)> = tx.query_row(
                "SELECT id, body_markdown FROM source_notes
                 WHERE tenant_id=?1 AND project_id=?2 AND title=?3 AND profile_id IS NULL
                 ORDER BY updated_at DESC, id DESC LIMIT 1",
                params![tenant.id(), source.id, draft.title],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?;
            match existing {
                Some((_, body)) if body == draft.body_markdown => return Ok(Some(false)),
                Some((id, _)) => tx.execute(
                    "UPDATE source_notes SET body_markdown=?1, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                     WHERE tenant_id=?2 AND id=?3",
                    params![draft.body_markdown, tenant.id(), id],
                )?,
                None => tx.execute(
                    "INSERT INTO source_notes (tenant_id, project_id, title, body_markdown, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
                    params![tenant.id(), source.id, draft.title, draft.body_markdown],
                )?,
            };
            Ok(Some(true))
        })?
    }
}
