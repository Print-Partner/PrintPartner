//! `pp-server import`: the one-time import of a Node installation. Today it
//! runs only the dry run, which reads the source and writes nothing.

use anyhow::{Context, Result, bail, ensure};
use pp_core::{DesktopRebuild, node_import};
use std::path::PathBuf;

pub(crate) const USAGE: &str =
    "Usage: pp-server import --from <Node data folder or backup archive> --dry-run

Reports what importing a stopped Node installation would convert: the schema,
Builds, Sources, printers, rows per table and tenant, and every stored JSON
shape with its place in the conversion inventory. The source is never opened
for writing. Needs PP_DESKTOP_REBUILD=1. Exits 0 with the report, or 1 with
the reason the source is refused or could not be read.";

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Help,
    DryRun { from: PathBuf },
}

pub(crate) fn parse(mut args: impl Iterator<Item = String>) -> Result<Command> {
    let (mut from, mut dry_run) = (None, false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(Command::Help),
            "--dry-run" => dry_run = true,
            "--from" => from = Some(PathBuf::from(args.next().context("--from needs a value")?)),
            // The destination of the import. A dry run does not read or create it.
            "--data" => drop(args.next().context("--data needs a value")?),
            _ => bail!("Unknown argument"),
        }
    }
    let from = from.context("--from is required")?;
    ensure!(
        dry_run,
        "Only --dry-run is available: this release does not write an import yet"
    );
    Ok(Command::DryRun { from })
}

pub(crate) fn run(args: impl Iterator<Item = String>) -> Result<()> {
    let from = match parse(args)? {
        Command::Help => {
            println!("{USAGE}");
            return Ok(());
        }
        Command::DryRun { from } => from,
    };
    ensure!(
        DesktopRebuild::from_env().enabled(),
        "pp-server import needs PP_DESKTOP_REBUILD=1"
    );
    match node_import::dry_run(&from) {
        Ok(report) => {
            print!("{report}");
            Ok(())
        }
        Err(error) => {
            let outcome = match error {
                node_import::ImportError::Failed(_) => "failed",
                _ => "refused",
            };
            eprintln!("import {outcome}: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(args: &[&str]) -> Result<Command> {
        parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn a_dry_run_needs_a_source_and_ignores_the_destination() {
        let dry_run = Command::DryRun { from: "s".into() };
        assert_eq!(parsed(&["--from", "s", "--dry-run"]).unwrap(), dry_run);
        assert_eq!(
            parsed(&["--dry-run", "--data", "d", "--from", "s"]).unwrap(),
            dry_run
        );
        assert_eq!(parsed(&["--help"]).unwrap(), Command::Help);
        for (args, reason) in [
            (&["--dry-run"][..], "--from is required"),
            (&["--from"], "--from needs a value"),
            (&["--from", "s"], "Only --dry-run is available"),
            (
                &["--from", "s", "--data", "d"],
                "Only --dry-run is available",
            ),
            (&["--from", "s", "--force"], "Unknown argument"),
        ] {
            let error = parsed(args).unwrap_err().to_string();
            assert!(error.starts_with(reason), "{args:?}: {error}");
        }
    }
}
