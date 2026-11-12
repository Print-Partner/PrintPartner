//! `pp-server import`: the one-time import of a Node installation. Today it
//! runs only the dry run, which reads the source and writes nothing.

use anyhow::{Context, Result, bail, ensure};
use pp_core::{DesktopRebuild, node_import};
use std::path::PathBuf;

const USAGE: &str = "Usage: pp-server import --from <Node data folder or backup archive> --dry-run

Reports what importing a stopped Node installation would convert: the schema,
Builds, Sources, printers, rows per table and tenant, and every stored JSON
shape with its place in the conversion inventory. The source is never opened
for writing. Needs PP_DESKTOP_REBUILD=1. Exits 0 with the report, or 1 with
the reason the source is refused or could not be read.";

/// The source of the dry run to run, or `None` when the arguments ask for help.
fn parse(mut args: impl Iterator<Item = String>) -> Result<Option<PathBuf>> {
    let (mut from, mut dry_run) = (None, false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(None),
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
    Ok(Some(from))
}

pub(crate) fn run(args: impl Iterator<Item = String>) -> Result<()> {
    let Some(from) = parse(args)? else {
        println!("{USAGE}");
        return Ok(());
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
