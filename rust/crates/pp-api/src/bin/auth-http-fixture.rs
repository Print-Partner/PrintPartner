use anyhow::{Result, ensure};
use pp_api::auth::{
    AuthHttpConfig, CookieTransport, OAuthCredentials, ProviderClient, ResetMailer, SmtpConfig,
    SmtpSecurity, auth_router,
};
use pp_storage::{
    Limits, WriterOwner,
    auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, Secret, SessionTenantPolicy},
};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderConfig {
    client_id: String,
    client_secret: String,
}
impl ProviderConfig {
    fn credentials(self) -> Result<OAuthCredentials> {
        OAuthCredentials::new(self.client_id, Secret::new(self.client_secret))
    }
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MailConfig {
    host: String,
    port: u16,
    from: String,
    start_tls: bool,
    credentials: Option<(String, String)>,
}
#[derive(Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    github: Option<ProviderConfig>,
    discord: Option<ProviderConfig>,
    smtp: Option<MailConfig>,
    app_public_url: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ensure!(
        !args.is_empty(),
        "Usage: auth-http-fixture DISPOSABLE_DIRECTORY [PORT] [single|open|closed] [dev-reset|production] [CONFIG_JSON]"
    );
    let file_config: ConfigFile = match args.get(4) {
        Some(path) => {
            let bytes = std::fs::read(path)?;
            ensure!(bytes.len() <= 16384, "Configuration file too large");
            serde_json::from_slice(&bytes)?
        }
        None => ConfigFile::default(),
    };
    let directory = std::path::PathBuf::from(&args[0]);
    std::fs::create_dir_all(&directory)?;
    let port: u16 = args.get(1).map(String::as_str).unwrap_or("0").parse()?;
    let mode = args.get(2).map(String::as_str).unwrap_or("single");
    let registration = match mode {
        "single" => RegistrationPolicy::FirstAccountOnly,
        "open" => RegistrationPolicy::Open,
        "closed" => RegistrationPolicy::Closed,
        _ => anyhow::bail!("Invalid registration mode"),
    };
    let (owner, _) = WriterOwner::open(&directory, Limits::default())?;
    let auth = owner.auth_with_policy(AuthPolicy {
        registration,
        session_tenant: if mode == "single" {
            SessionTenantPolicy::SingleAccountDefault
        } else {
            SessionTenantPolicy::AccountTenant
        },
        first_user: FirstUserTenant::NewUser,
    })?;
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
    let address = listener.local_addr()?;
    let config = AuthHttpConfig::new(
        &format!("http://{address}"),
        CookieTransport::LoopbackHttp,
        mode != "single",
        file_config.app_public_url.as_deref(),
        args.get(3).is_some_and(|arg| arg == "dev-reset"),
    )?;
    let providers = ProviderClient::new(
        file_config
            .github
            .map(ProviderConfig::credentials)
            .transpose()?,
        file_config
            .discord
            .map(ProviderConfig::credentials)
            .transpose()?,
    )?;
    let mail = match file_config.smtp {
        Some(smtp) => ResetMailer::smtp(SmtpConfig {
            security: if smtp.start_tls {
                SmtpSecurity::StartTls
            } else {
                SmtpSecurity::Tls
            },
            host: smtp.host,
            port: smtp.port,
            from: smtp.from,
            credentials: smtp.credentials,
        })?,
        None => ResetMailer::disabled(),
    };
    let router = auth_router(config, auth, providers, mail.clone());
    let stop_signal = stop_signal()?;
    println!(
        "{}",
        serde_json::json!({"origin":format!("http://{address}"),"writer_owners":1,"mode":"standalone-auth-http"})
    );
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(stop_signal)
    .await?;
    mail.drain_deliveries().await;
    owner.shutdown()?;
    Ok(())
}

/// SIGINT or SIGTERM.
#[cfg(unix)]
fn stop_signal() -> std::io::Result<impl Future<Output = ()>> {
    use tokio::signal::unix::{SignalKind, signal};
    let (mut interrupt, mut terminate) = (
        signal(SignalKind::interrupt())?,
        signal(SignalKind::terminate())?,
    );
    Ok(async move {
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
        }
    })
}

/// Ctrl+C or Ctrl+Break.
#[cfg(windows)]
fn stop_signal() -> std::io::Result<impl Future<Output = ()>> {
    use tokio::signal::windows::{ctrl_break, ctrl_c};
    let (mut interrupt, mut brk) = (ctrl_c()?, ctrl_break()?);
    Ok(async move {
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = brk.recv() => {}
        }
    })
}
