//! The farm facts the Build workflow projection takes (GRE-1166): beds that
//! still hold a Build's finished print, and unmatched prints whose suggestion
//! names the Build. Read in the workflow read's own transaction, so the task
//! feed, `GET /plans/{id}/workflow` and MCP see the same counts.

use super::{
    CheckoffResultState, Farm, MatchConfidence, MatchLibrary, instant, setting_list, text,
};
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
        let unattributed = list("printer.unattributed_prints")?;
        let finished: Vec<(String, i128)> = unattributed
            .iter()
            .filter_map(|print| {
                Some((
                    text(print, "printer_id")?,
                    instant(text(print, "completed_at").as_deref())?,
                ))
            })
            .collect();
        let farm = Farm::parse(
            list("printer.fleet")?,
            list("integrations")?,
            list("printer.checkoff_links")?,
            unattributed,
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
            beds: farm.beds_to_clear(&finished),
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
    /// Awaiting results still on their bed, per Build. Each result's bed time
    /// is when its print came off (`completed_at`) or, while it runs, when it
    /// started (`created_at`); a claimed print's link is created at claim time
    /// but keeps the print's `completed_at`. A result leaves its bed when
    /// another result on the same printer has a later bed time, or an
    /// unmatched print (open, claimed or dismissed, in `finished`) completed
    /// there later. Result times compare when both parse; otherwise the result
    /// stored later is the later one. Evidence lasts only as long as the
    /// settings keep it: pruned unmatched prints and trimmed links no longer
    /// count.
    fn beds_to_clear(&self, finished: &[(String, i128)]) -> BTreeMap<i64, u64> {
        let bed_time = |index: usize| {
            let result = &self.results[index];
            instant(result.completed_at.as_deref()).or_else(|| instant(Some(&result.created_at)))
        };
        let mut beds = BTreeMap::new();
        for (index, result) in self.results.iter().enumerate() {
            if result.state != CheckoffResultState::AwaitingVerify {
                continue;
            }
            let this = bed_time(index);
            let later_result = self.results.iter().enumerate().any(|(other, next)| {
                other != index
                    && next.printer_id == result.printer_id
                    && match (this, bed_time(other)) {
                        (Some(this), Some(that)) => that > this || (that == this && other > index),
                        _ => other > index,
                    }
            });
            let later_print = this.is_some_and(|this| {
                finished
                    .iter()
                    .any(|(printer, at)| *printer == result.printer_id && *at > this)
            });
            if !later_result && !later_print {
                *beds.entry(result.profile_id).or_default() += 1;
            }
        }
        beds
    }
}
