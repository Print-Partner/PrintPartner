use crate::desktop_settings::{self, DesktopSetting, DesktopSettings};
use crate::state_file;
use anyhow::{Context, Result, ensure};
use pp_storage::lease::StorageLease;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

const FILE: &str = "browser-preferences.json";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(try_from = "u8", into = "u8")]
pub enum PrinterPollInterval {
    FiveSeconds,
    TenSeconds,
    FifteenSeconds,
    ThirtySeconds,
}

impl From<PrinterPollInterval> for u8 {
    fn from(value: PrinterPollInterval) -> Self {
        match value {
            PrinterPollInterval::FiveSeconds => 5,
            PrinterPollInterval::TenSeconds => 10,
            PrinterPollInterval::FifteenSeconds => 15,
            PrinterPollInterval::ThirtySeconds => 30,
        }
    }
}

impl TryFrom<u8> for PrinterPollInterval {
    type Error = &'static str;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            5 => Ok(Self::FiveSeconds),
            10 => Ok(Self::TenSeconds),
            15 => Ok(Self::FifteenSeconds),
            30 => Ok(Self::ThirtySeconds),
            _ => Err("Unsupported printer poll interval"),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrowserPreferences {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar_collapsed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<Theme>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub printer_poll_interval: Option<PrinterPollInterval>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserPreference {
    SidebarCollapsed(Option<bool>),
    Theme(Option<Theme>),
    PrinterPollInterval(Option<PrinterPollInterval>),
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u8,
    preferences: BrowserPreferences,
}

fn decode(snapshot: Option<&state_file::Snapshot>) -> Result<BrowserPreferences> {
    let Some(snapshot) = snapshot else {
        return Ok(BrowserPreferences::default());
    };
    let document: Document =
        serde_json::from_slice(&snapshot.bytes).context("Invalid browser preferences")?;
    ensure!(
        document.version == 1,
        "Unsupported browser preferences version"
    );
    Ok(document.preferences)
}

fn read(owner: &StorageLease) -> Result<BrowserPreferences> {
    decode(state_file::read(owner, FILE)?.as_ref())
}

fn update(owner: &StorageLease, field: BrowserPreference) -> Result<BrowserPreferences> {
    update_with(owner, field, |_| Ok(()))
}

fn update_with(
    owner: &StorageLease,
    field: BrowserPreference,
    boundary: impl FnMut(state_file::Boundary) -> Result<()>,
) -> Result<BrowserPreferences> {
    let previous = state_file::read(owner, FILE)?;
    let mut preferences = decode(previous.as_ref())?;
    match field {
        BrowserPreference::SidebarCollapsed(value) => preferences.sidebar_collapsed = value,
        BrowserPreference::Theme(value) => preferences.theme = value,
        BrowserPreference::PrinterPollInterval(value) => preferences.printer_poll_interval = value,
    }
    let bytes = serde_json::to_vec(&Document {
        version: 1,
        preferences: preferences.clone(),
    })?;
    state_file::publish_with(owner, FILE, &bytes, previous.as_ref(), boundary)?;
    Ok(preferences)
}

pub(super) enum Command {
    Read(oneshot::Sender<Result<BrowserPreferences>>),
    Update(
        BrowserPreference,
        oneshot::Sender<Result<BrowserPreferences>>,
    ),
    ReadDesktop(oneshot::Sender<Result<DesktopSettings>>),
    UpdateDesktop(DesktopSetting, oneshot::Sender<Result<DesktopSettings>>),
}

impl Command {
    pub(super) fn execute(self, owner: &StorageLease) {
        match self {
            Self::Read(reply) => {
                let _ = reply.send(read(owner));
            }
            Self::Update(field, reply) => {
                let _ = reply.send(update(owner, field));
            }
            Self::ReadDesktop(reply) => {
                let _ = reply.send(desktop_settings::read(owner));
            }
            Self::UpdateDesktop(field, reply) => {
                let _ = reply.send(desktop_settings::update(owner, field));
            }
        }
    }
}

#[derive(Clone)]
pub(super) struct Client(mpsc::Sender<Command>);

impl Client {
    pub(super) fn channel() -> (Self, mpsc::Receiver<Command>) {
        let (sender, receiver) = mpsc::channel(16);
        (Self(sender), receiver)
    }

    pub(super) async fn read(&self) -> Result<BrowserPreferences> {
        let (reply, result) = oneshot::channel();
        self.0
            .send(Command::Read(reply))
            .await
            .context("Core owner stopped")?;
        result.await.context("Core owner stopped")?
    }

    pub(super) async fn update(&self, field: BrowserPreference) -> Result<BrowserPreferences> {
        let (reply, result) = oneshot::channel();
        self.0
            .send(Command::Update(field, reply))
            .await
            .context("Core owner stopped")?;
        result.await.context("Core owner stopped")?
    }

    pub(super) async fn read_desktop(&self) -> Result<DesktopSettings> {
        let (reply, result) = oneshot::channel();
        self.0
            .send(Command::ReadDesktop(reply))
            .await
            .context("Core owner stopped")?;
        result.await.context("Core owner stopped")?
    }

    pub(super) async fn update_desktop(&self, field: DesktopSetting) -> Result<DesktopSettings> {
        let (reply, result) = oneshot::channel();
        self.0
            .send(Command::UpdateDesktop(field, reply))
            .await
            .context("Core owner stopped")?;
        result.await.context("Core owner stopped")?
    }
}

pub(super) async fn settle(
    receiver: &mut mpsc::Receiver<Command>,
    mut execute: impl FnMut(Command),
) {
    receiver.close();
    while let Some(command) = receiver.recv().await {
        execute(command);
    }
}

#[cfg(test)]
mod tests;
