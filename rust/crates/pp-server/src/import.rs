//! `pp-server import`: the one-time import of a Node installation. Today it
//! runs only the dry run, which reads the source and writes nothing.

use anyhow::{Context, Result, bail, ensure};
use pp_core::{DesktopRebuild, node_import};
use std::path::PathBuf;

const USAGE: &str = "Usage: pp-server import --from <Node data folder or backup archive> --data <destination> [--dry-run]

Imports a stopped Node installation into <destination>, a new Rust-owned data
folder that must be absent or empty. The source is never opened for writing.
The report is printed and saved as import-report.json in the destination.

With --dry-run nothing is written: the report says what an import would
convert and, when --data is given, whether the import would accept that
folder. Needs PP_DESKTOP_REBUILD=1. Exits 0 with the report, or 1 with the
reason the import is refused or failed.";

/// What the arguments ask for.
enum Command {
    Help,
    DryRun(PathBuf, Option<PathBuf>),
    Import(PathBuf, PathBuf),
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Command> {
    let (mut from, mut data, mut dry_run) = (None, None, false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(Command::Help),
            "--dry-run" => dry_run = true,
            "--from" => from = Some(PathBuf::from(args.next().context("--from needs a value")?)),
            "--data" => data = Some(PathBuf::from(args.next().context("--data needs a value")?)),
            _ => bail!("Unknown argument"),
        }
    }
    let from = from.context("--from is required")?;
    if dry_run {
        return Ok(Command::DryRun(from, data));
    }
    Ok(Command::Import(
        from,
        data.context("--data is required without --dry-run")?,
    ))
}

pub(crate) fn run(args: impl Iterator<Item = String>) -> Result<()> {
    let command = parse(args)?;
    if matches!(command, Command::Help) {
        println!("{USAGE}");
        return Ok(());
    }
    ensure!(
        DesktopRebuild::from_env().enabled(),
        "pp-server import needs PP_DESKTOP_REBUILD=1"
    );
    let report = match &command {
        Command::DryRun(from, data) => {
            node_import::dry_run(from, data.as_deref()).map(|report| report.to_string())
        }
        Command::Import(from, data) => {
            node_import::import(from, data).map(|report| report.to_string())
        }
        Command::Help => Ok(String::new()),
    };
    match report {
        Ok(report) => {
            print!("{report}");
            Ok(())
        }
        Err(error) => {
            let outcome = match error {
                node_import::ImportError::Io { .. }
                | node_import::ImportError::Sqlite(_)
                | node_import::ImportError::CopyMismatch(_)
                | node_import::ImportError::Flush { .. } => "failed",
                _ => "refused",
            };
            eprintln!("import {outcome}: {error}");
            std::process::exit(1);
        }
    }
}
