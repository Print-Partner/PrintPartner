use anyhow::{Context, Result, bail};
use pp_core::{CoreRuntime, CoreStatus, DesktopLaunch, VerifiedBundle};
use std::io::Write;

mod args;
#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as platform;

#[tokio::main]
async fn main() -> Result<()> {
    let args::Arguments {
        data,
        web,
        node,
        commit,
        credential_file,
    } = args::parse(std::env::args().skip(1))?;
    let web = web.context("--web is required")?.canonicalize()?;
    let package: serde_json::Value =
        serde_json::from_slice(&std::fs::read(web.join("package.json"))?)?;
    let version = package["version"]
        .as_str()
        .context("Package version missing")?;
    let bundle = VerifiedBundle {
        node: node.context("--node is required")?.canonicalize()?,
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
    println!("{}", runtime.origin());
    let shutdown_requested = runtime.shutdown_requested();
    tokio::select! {
        _ = stop_signal => {}
        _ = shutdown_requested => {}
    }
    let receipt = runtime.shutdown().await;
    println!("{}", serde_json::to_string(&receipt)?);
    anyhow::ensure!(receipt.complete(), "Desktop shutdown incomplete");
    Ok(())
}
