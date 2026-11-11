use anyhow::{Context, Result, bail};
use pp_core::{
    CoreRuntime, CoreStatus, DesktopLaunch, DesktopRebuild, IncompatibleSchema, VerifiedBundle,
};
use std::io::Write;

mod args;
mod health;
#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as platform;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().map(String::as_str) == Some("healthcheck") {
        args.next();
        return healthcheck(args);
    }
    let result = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(serve(args.collect()));
    // An unsupported schema stays unsupported until someone acts, so exit
    // with a code a restart policy can tell apart from a crash.
    if let Some(refusal) = result.as_ref().err().and_then(|error| {
        error
            .chain()
            .find_map(|cause| cause.downcast_ref::<IncompatibleSchema>())
    }) {
        eprintln!("{}", refusal.diagnostic());
        eprintln!("{refusal}");
        std::process::exit(INCOMPATIBLE_STORAGE_EXIT);
    }
    result
}

/// `EX_CONFIG` from sysexits.h: the data folder needs a person to act.
const INCOMPATIBLE_STORAGE_EXIT: i32 = 78;

/// `pp-server healthcheck`: exits 0 when the probe passes, 1 when it fails.
fn healthcheck(args: impl Iterator<Item = String>) -> Result<()> {
    let (probe, status_file, timeout) = match health::parse(args)? {
        health::Command::Help => {
            println!("{}", health::USAGE);
            return Ok(());
        }
        health::Command::Check {
            probe,
            status_file,
            timeout,
        } => (probe, status_file, timeout),
    };
    match health::check(probe, &status_file, timeout) {
        Ok(line) => {
            println!("{line}");
            Ok(())
        }
        Err(error) => {
            println!("unhealthy: {error}");
            std::process::exit(1);
        }
    }
}

async fn serve(args: Vec<String>) -> Result<()> {
    let args::Arguments {
        data,
        web,
        node,
        commit,
        credential_file,
        status_file,
    } = args::parse(args.into_iter())?;
    let web = web.context("--web is required")?.canonicalize()?;
    let package: serde_json::Value =
        serde_json::from_slice(&std::fs::read(web.join("package.json"))?)?;
    let version = package["version"]
        .as_str()
        .context("Package version missing")?;
    // A Rust-only start runs no Node child and does not measure Node, so the
    // Docker image ships none and omits --node.
    let node = match node {
        Some(node) => node.canonicalize()?,
        None if DesktopRebuild::from_env().enabled() => std::path::PathBuf::new(),
        None => bail!("--node is required"),
    };
    let status = status_file.map(health::StatusFile::create).transpose()?;
    let bundle = VerifiedBundle {
        node,
        entry: web.join("apps/server/dist/current/desktop.js"),
        web_root: web.clone(),
        runtime_version: format!("{version}-web"),
        commit: commit.context("--commit is required")?,
    };
    let mut runtime = CoreRuntime::start_for_launcher(DesktopLaunch {
        data_dir: data.context("--data is required")?,
        assets: web.join("apps/web/dist"),
        bundle,
    })
    .await?;
    if let Some(path) = credential_file {
        let url = runtime.take_launch_target()?.into_url();
        let child_pid = match runtime.handle().compat_status() {
            None => None,
            Some(status) => match *status.borrow() {
                CoreStatus::Ready { pid, .. } => Some(pid),
                _ => bail!("Compatibility child is not ready for the test receipt"),
            },
        };
        let result = (|| -> Result<()> {
            let mut file = platform::create_private_file(&path)?;
            file.write_all(
                serde_json::json!({"bootstrap_url":url, "child_pid":child_pid, "runtime_dir":runtime.runtime_dir()})
                    .to_string()
                    .as_bytes(),
            )?;
            file.sync_all()?;
            Ok(())
        })();
        if let Err(error) = result {
            let receipt = runtime.shutdown().await;
            anyhow::ensure!(receipt.complete(), "Desktop shutdown incomplete");
            return Err(error);
        }
    }
    // Register signal streams before announcing the origin: a signal sent right after
    // the origin line is queued here instead of taking the default action.
    let stop_signal = platform::stop_signal()?;
    if let Some(Err(error)) = status
        .as_ref()
        .map(|status| status.publish(health::State::Ready, Some(runtime.origin())))
    {
        let receipt = runtime.shutdown().await;
        anyhow::ensure!(receipt.complete(), "Desktop shutdown incomplete");
        return Err(error);
    }
    println!("{}", runtime.origin());
    let shutdown_requested = runtime.shutdown_requested();
    tokio::select! {
        _ = stop_signal => {}
        _ = shutdown_requested => {}
    }
    // Readiness fails from here on. A failed write still finishes the shutdown.
    if let Some(Err(error)) = status
        .as_ref()
        .map(|status| status.publish(health::State::Stopping, None))
    {
        eprintln!("{error:#}");
    }
    let receipt = runtime.shutdown().await;
    println!("{}", serde_json::to_string(&receipt)?);
    anyhow::ensure!(receipt.complete(), "Desktop shutdown incomplete");
    Ok(())
}
