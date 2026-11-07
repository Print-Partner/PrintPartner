use crate::state_file;
use anyhow::{Context, Result};
use pp_storage::lease::StorageLease;
use std::{
    io::ErrorKind,
    net::{Ipv4Addr, SocketAddrV4},
    num::NonZeroU16,
};
use tokio::net::TcpListener;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OriginChange {
    PersistedPortInUse {
        previous_port: NonZeroU16,
        new_port: NonZeroU16,
    },
}

impl std::fmt::Display for OriginChange {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PersistedPortInUse {
                previous_port,
                new_port,
            } => write!(
                formatter,
                "The saved desktop port {previous_port} was in use. This launch uses port {new_port}."
            ),
        }
    }
}

pub(super) async fn bind_origin(
    owner: &StorageLease,
) -> Result<(TcpListener, Option<OriginChange>)> {
    bind_origin_with(owner, |_| Ok(())).await
}

pub(super) async fn bind_origin_with(
    owner: &StorageLease,
    boundary: impl FnMut(state_file::Boundary) -> Result<()>,
) -> Result<(TcpListener, Option<OriginChange>)> {
    bind_origin_using(owner, boundary, TcpListener::bind).await
}

pub(super) async fn bind_origin_using<F>(
    owner: &StorageLease,
    boundary: impl FnMut(state_file::Boundary) -> Result<()>,
    mut bind: impl FnMut(SocketAddrV4) -> F,
) -> Result<(TcpListener, Option<OriginChange>)>
where
    F: std::future::Future<Output = std::io::Result<TcpListener>>,
{
    let mut removed = false;
    for entry in std::fs::read_dir(owner.data_dir())? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(suffix) = name
            .to_str()
            .and_then(|name| name.strip_prefix("desktop.toml-"))
            .and_then(|name| name.strip_suffix(".new"))
        else {
            continue;
        };
        if suffix.len() == 16
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            && entry.file_type()?.is_file()
        {
            std::fs::remove_file(entry.path())?;
            removed = true;
        }
    }
    if removed {
        pp_os::sync_directory(owner.data_dir())?;
    }
    let config = state_file::read(owner, "desktop.toml")?;
    let port = config
        .as_ref()
        .map(|config| {
            let text = std::str::from_utf8(&config.bytes).context("Invalid desktop.toml")?;
            let value = text
                .trim()
                .strip_prefix("port = ")
                .context("Invalid desktop.toml")?;
            let port: u16 = value.parse().context("Invalid persisted port")?;
            NonZeroU16::new(port).context("Invalid persisted port")
        })
        .transpose()?;
    let bound = bind(SocketAddrV4::new(
        Ipv4Addr::LOCALHOST,
        port.map_or(0, NonZeroU16::get),
    ))
    .await;
    let listener = match (port, bound) {
        (_, Ok(listener)) => listener,
        (Some(previous_port), Err(error)) if error.kind() == ErrorKind::AddrInUse => {
            let listener = bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
                .await
                .context("Persisted desktop origin is unavailable")?;
            let new_port =
                NonZeroU16::new(listener.local_addr()?.port()).context("Invalid selected port")?;
            state_file::publish_with(
                owner,
                "desktop.toml",
                format!("port = {new_port}\n").as_bytes(),
                config.as_ref(),
                boundary,
            )?;
            return Ok((
                listener,
                Some(OriginChange::PersistedPortInUse {
                    previous_port,
                    new_port,
                }),
            ));
        }
        (_, Err(error)) => return Err(error).context("Persisted desktop origin is unavailable"),
    };
    if port.is_none() {
        persist_initial_origin(owner, listener.local_addr()?.port())?;
    }
    Ok((listener, None))
}

pub(super) fn persist_initial_origin(owner: &StorageLease, port: u16) -> Result<()> {
    state_file::publish(
        owner,
        "desktop.toml",
        format!("port = {port}\n").as_bytes(),
        None,
    )
}
