//! `pp-server import`: the one-time import of a Node installation, or with
//! `--dry-run` the check that reads the source and writes nothing.

use anyhow::{Context, Result, bail, ensure};
use pp_core::first_run::{self, AttemptResult};
use pp_core::{DesktopRebuild, node_import};
use std::path::PathBuf;

const USAGE: &str = "Usage: pp-server import --from <Node data folder or backup archive> --data <destination> [--dry-run]

Imports a stopped Node installation into <destination>, a new Rust-owned data
folder that must be absent or empty. The source is never opened for writing.
The report is printed and saved as import-report.json in the destination.

With --dry-run nothing is written: the report says what an import would
convert and, when --data is given, whether the import would accept that
folder. Needs PP_DESKTOP_REBUILD=1. Exits 0 with the report, or 1 with the
reason the import is refused or failed. An import ends with a line that says
what the destination now holds.";

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
    match command {
        Command::DryRun(from, data) => match node_import::dry_run(&from, data.as_deref()) {
            Ok(report) => print!("{report}"),
            Err(error) => {
                eprintln!("{}", AttemptResult::from(error));
                std::process::exit(1);
            }
        },
        Command::Import(from, data) => {
            let attempt = first_run::import_node(&from, &data);
            if let AttemptResult::Imported(_) = attempt.result {
                print!("{}", attempt.result);
                println!("destination: {}", attempt.destination);
            } else {
                eprintln!("{}", attempt.result);
                eprintln!("destination: {}", attempt.destination);
                std::process::exit(1);
            }
        }
        Command::Help => {}
    }
    Ok(())
}
