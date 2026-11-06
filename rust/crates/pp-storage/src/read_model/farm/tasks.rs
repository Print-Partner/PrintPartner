//! The farm facts the Build workflow projection takes (GRE-1166): beds that
//! still hold a Build's finished print, and unmatched prints whose suggestion
//! names the Build. Read in the workflow read's own transaction, so the task
//! feed, `GET /plans/{id}/workflow` and MCP see the same counts.

use super::{CheckoffResultState, Farm, MatchConfidence, MatchLibrary, instant, setting_list};
use anyhow::Result;
use rusqlite::Transaction;
use std::collections::BTreeMap;

/// Per-Build counts from the tenant's farm settings.
#[derive(Debug, Default)]
pub(crate) struct FarmTasks {
    beds: BTreeMap<i64, u64>,
    matches: BTreeMap<i64, u64>,
}

impl FarmTasks {
    pub(crate) fn read(tx: &Transaction<'_>, tenant: &str) -> Result<Self> {
        let list = |key: &str| setting_list(tx, tenant, key);
        let farm = Farm::parse(
            list("printer.fleet")?,
            list("integrations")?,
            list("printer.checkoff_links")?,
            list("printer.unattributed_prints")?,
            list("printer.send_queue")?,
        );
        let mut matches = BTreeMap::new();
        if !farm.unmatched.is_empty() {
            let library = MatchLibrary::read(tx, tenant, &farm)?;
            for suggestion in farm.suggest(&library).suggestions {
                if let (Some(build), MatchConfidence::Exact | MatchConfidence::Likely) =
                    (suggestion.build, suggestion.confidence)
                {
                    *matches.entry(build.id).or_default() += 1;
                }
            }
        }
        Ok(Self {
            beds: farm.beds_to_clear(),
            matches,
        })
    }

    /// Beds holding this Build's finished, unconfirmed prints.
    pub(crate) fn beds(&self, build: i64) -> u64 {
        self.beds.get(&build).copied().unwrap_or(0)
    }

    /// Unmatched prints suggested for this Build as `exact` or `likely`.
    pub(crate) fn matches(&self, build: i64) -> u64 {
        self.matches.get(&build).copied().unwrap_or(0)
    }
}

impl Farm {
    /// Awaiting results still on their bed, per Build. A result leaves its bed
    /// when a later result on the same printer started (the bed was cleared to
    /// start it) or a later unmatched print finished there. Times compare when
    /// both parse; otherwise the result stored later is the later one.
    fn beds_to_clear(&self) -> BTreeMap<i64, u64> {
        let mut beds = BTreeMap::new();
        for (index, result) in self.results.iter().enumerate() {
            if result.state != CheckoffResultState::AwaitingVerify {
                continue;
            }
            let started = instant(Some(&result.created_at));
            let finished = instant(result.completed_at.as_deref()).or(started);
            let later_print = self.results.iter().enumerate().any(|(other, next)| {
                other != index
                    && next.printer_id == result.printer_id
                    && match (started, instant(Some(&next.created_at))) {
                        (Some(this), Some(that)) => that > this || (that == this && other > index),
                        _ => other > index,
                    }
            });
            let later_unmatched = self.unmatched.iter().any(|open| {
                open.print.printer_id == result.printer_id
                    && matches!(
                        (finished, instant(Some(&open.print.completed_at))),
                        (Some(this), Some(that)) if that > this
                    )
            });
            if !later_print && !later_unmatched {
                *beds.entry(result.profile_id).or_default() += 1;
            }
        }
        beds
    }
}
