#[cfg(feature = "desktop_rebuild")]
pub(crate) mod authority;
#[cfg(feature = "desktop_rebuild")]
pub use authority::AuthorityFailure;
#[cfg(feature = "desktop_rebuild")]
pub(crate) use authority::queries::{
    StreamAuthority, StreamAuthorityIdentity, resolve_stream_authority,
};
mod crypto;
mod keys;
mod policy;
#[cfg(feature = "desktop_rebuild")]
pub mod rebuild;
pub use policy::{
    AuthFailure, AuthInputFailure, AuthPolicy, AuthStatus, RegistrationPolicy, SessionTenantPolicy,
};
pub(crate) const OWNED_SETTINGS: [&str; 1] = ["api_keys_v1"];

use crate::{Envelope, SettingsClient, WriterOwner};
use anyhow::{Result, anyhow, bail, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

pub struct Secret(String);
impl Secret {
    pub fn new(value: String) -> Self {
        Self(value)
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
/// A fixed placeholder, so a type that holds a secret can derive `Debug`.
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret([REDACTED])")
    }
}
#[derive(Clone, Copy)]
pub enum FirstUserTenant {
    NewUser,
    ClaimDefault,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Email,
    Github,
    Discord,
}
impl Provider {
    fn name(self) -> &'static str {
        match self {
            Self::Email => "email",
            Self::Github => "github",
            Self::Discord => "discord",
        }
    }
    fn from_name(value: &str) -> Result<Self> {
        match value {
            "email" => Ok(Self::Email),
            "github" => Ok(Self::Github),
            "discord" => Ok(Self::Discord),
            _ => Err(AuthFailure::Storage.into()),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct User {
    pub user_id: String,
    pub tenant_id: String,
    pub login: String,
    pub display_name: String,
    pub email: Option<String>,
    pub provider: Provider,
    pub is_admin: bool,
    pub has_password: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyInfo {
    pub id: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub expires_at: Option<String>,
    pub is_active: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyPrincipal {
    pub tenant_id: String,
    pub key_id: String,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub enum LocalCommit {
    ReadOnly,
    Committed,
}
pub enum Outcome {
    Status(AuthStatus),
    IdentityExists(bool),
    Session {
        user: User,
        token: Secret,
    },
    User(Option<User>),
    ResetToken(Option<Secret>),
    Changed(bool),
    Keys {
        keys: Vec<KeyInfo>,
        commit: LocalCommit,
    },
    KeyChanged {
        changed: bool,
        commit: LocalCommit,
    },
    KeyCreated {
        info: KeyInfo,
        key: Secret,
    },
    KeyResolved {
        principal: Option<KeyPrincipal>,
        commit: LocalCommit,
    },
}
pub enum Request {
    Status,
    IdentityExists {
        provider: Provider,
        provider_user_id: String,
    },
    Register {
        email: String,
        display_name: String,
        password: Secret,
    },
    Login {
        email: String,
        password: Secret,
    },
    ResolveSession {
        token: Secret,
    },
    Logout {
        token: Secret,
    },
    LogoutAll {
        session: Secret,
    },
    ChangePassword {
        session: Secret,
        current: Secret,
        replacement: Secret,
    },
    RequestReset {
        email: String,
    },
    ResetPassword {
        token: Secret,
        replacement: Secret,
    },
    OAuthLogin {
        provider: Provider,
        provider_user_id: String,
        email: Option<String>,
        display_name: String,
    },
    LinkIdentity {
        session: Secret,
        provider: Provider,
        provider_user_id: String,
    },
    ListKeys {
        session: Secret,
    },
    CreateKey {
        session: Secret,
    },
    RevokeKey {
        session: Secret,
        key_id: String,
    },
    RotateKey {
        session: Secret,
        key_id: String,
    },
    ResolveKey {
        tenant_id: String,
        key: Secret,
    },
}
impl Request {
    fn validate(&self) -> Result<()> {
        let fields: Vec<&str> = match self {
            Self::Status => vec![],
            Self::IdentityExists {
                provider_user_id, ..
            } => vec![provider_user_id],
            Self::Register {
                email,
                display_name,
                password,
            } => vec![email, display_name, password.expose()],
            Self::Login { email, password } => vec![email, password.expose()],
            Self::ResolveSession { token } | Self::Logout { token } => vec![token.expose()],
            Self::LogoutAll { session }
            | Self::ListKeys { session }
            | Self::CreateKey { session } => vec![session.expose()],
            Self::ChangePassword {
                session,
                current,
                replacement,
            } => vec![session.expose(), current.expose(), replacement.expose()],
            Self::RequestReset { email } => vec![email],
            Self::ResetPassword { token, replacement } => {
                vec![token.expose(), replacement.expose()]
            }
            Self::OAuthLogin {
                provider_user_id,
                email,
                display_name,
                ..
            } => vec![
                provider_user_id,
                email.as_deref().unwrap_or(""),
                display_name,
            ],
            Self::LinkIdentity {
                session,
                provider_user_id,
                ..
            } => vec![session.expose(), provider_user_id],
            Self::RevokeKey { session, key_id } | Self::RotateKey { session, key_id } => {
                vec![session.expose(), key_id]
            }
            Self::ResolveKey { tenant_id, key } => vec![tenant_id, key.expose()],
        };
        ensure!(
            fields.iter().all(|value| value.len() <= 4096),
            AuthFailure::InvalidInput(AuthInputFailure::TooLong)
        );
        Ok(())
    }
}
pub type AuthReply = mpsc::Receiver<Result<Outcome>>;
#[derive(Clone)]
pub struct AuthClient {
    storage: SettingsClient,
    policy: AuthPolicy,
}
struct Job {
    client: AuthClient,
    request: Request,
    cancelled: Arc<AtomicBool>,
    reply: mpsc::Sender<Result<Outcome>>,
}
struct Pool {
    sender: mpsc::SyncSender<Job>,
}
impl Pool {
    fn global() -> &'static Self {
        static POOL: OnceLock<Pool> = OnceLock::new();
        POOL.get_or_init(Self::new)
    }
    fn new() -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Job>(32);
        let receiver = Arc::new(Mutex::new(receiver));
        for _ in 0..2 {
            let receiver = receiver.clone();
            thread::spawn(move || {
                loop {
                    let job = match receiver.lock() {
                        Ok(receiver) => receiver.recv(),
                        Err(_) => break,
                    };
                    let Ok(job) = job else {
                        break;
                    };
                    let result = if job.cancelled.load(Ordering::Acquire) {
                        Err(anyhow!("Cancelled before authentication"))
                    } else {
                        job.client.perform(job.request, &job.cancelled)
                    };
                    let result = result.map_err(|error| {
                        if error.downcast_ref::<AuthFailure>().is_some() {
                            error
                        } else {
                            error.context(AuthFailure::Storage)
                        }
                    });
                    let _ = job.reply.send(result);
                }
            });
        }
        Pool { sender }
    }
}
impl WriterOwner {
    pub fn auth(&self, first_user: FirstUserTenant) -> AuthClient {
        AuthClient {
            storage: self.client(),
            policy: AuthPolicy {
                registration: RegistrationPolicy::Open,
                session_tenant: SessionTenantPolicy::AccountTenant,
                first_user,
            },
        }
    }
    pub fn auth_with_policy(&self, policy: AuthPolicy) -> Result<AuthClient> {
        validate_policy(policy)?;
        Ok(AuthClient {
            storage: self.client(),
            policy,
        })
    }
}
pub(crate) fn validate_policy(policy: AuthPolicy) -> Result<()> {
    ensure!(
        !matches!(
            (policy.session_tenant, policy.first_user),
            (
                SessionTenantPolicy::SingleAccountDefault,
                FirstUserTenant::ClaimDefault
            )
        ),
        AuthFailure::InvalidInput(AuthInputFailure::Policy)
    );
    Ok(())
}
impl AuthClient {
    pub fn submit(
        &self,
        request: Request,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<AuthReply> {
        self.submit_to_pool(request, cancelled, wait, Pool::global())
    }
    fn submit_to_pool(
        &self,
        request: Request,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
        pool: &Pool,
    ) -> Result<AuthReply> {
        request.validate()?;
        let (reply, receiver) = mpsc::channel();
        let mut job = Job {
            client: self.clone(),
            request,
            cancelled,
            reply,
        };
        let deadline = Instant::now() + wait;
        loop {
            ensure!(
                !job.cancelled.load(Ordering::Acquire),
                "Cancelled before admission"
            );
            match pool.sender.try_send(job) {
                Ok(()) => return Ok(receiver),
                Err(mpsc::TrySendError::Disconnected(_)) => bail!(AuthFailure::Stopped),
                Err(mpsc::TrySendError::Full(returned)) => job = returned,
            }
            ensure!(Instant::now() < deadline, AuthFailure::QueueFull);
            thread::sleep(Duration::from_millis(2));
        }
    }
    fn command(&self, command: Command, cancelled: &AtomicBool) -> Result<Reply> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let shared = &self.storage.shared;
        let mut queue = shared
            .queue
            .lock()
            .map_err(|_| anyhow!("Writer admission poisoned"))?;
        loop {
            ensure!(!queue.closed, AuthFailure::Stopped);
            ensure!(
                !cancelled.load(Ordering::Acquire),
                "Cancelled before write admission"
            );
            if queue.pending.len() < shared.capacity {
                break;
            }
            ensure!(Instant::now() < deadline, AuthFailure::QueueFull);
            queue = shared
                .changed
                .wait_timeout(queue, Duration::from_millis(5))
                .map_err(|_| anyhow!("Writer admission poisoned"))?
                .0;
        }
        let (reply, receiver) = mpsc::channel();
        queue.pending.push_back(Envelope::Auth {
            command,
            policy: self.policy,
            reply,
        });
        shared.changed.notify_all();
        drop(queue);
        receiver
            .recv()
            .map_err(|_| anyhow!(AuthFailure::CommitUnknown))?
    }
    fn login(
        &self,
        mut credential: Credential,
        password: &Secret,
        cancelled: &AtomicBool,
    ) -> Result<Outcome> {
        let user_id = credential.user.user_id.clone();
        let email = credential
            .user
            .email
            .clone()
            .ok_or_else(|| anyhow!(AuthFailure::InvalidCredentials))?;
        for attempt in 0..2 {
            ensure!(
                !cancelled.load(Ordering::Acquire),
                "Cancelled before verification"
            );
            ensure!(
                credential.user.user_id == user_id
                    && credential
                        .hash
                        .as_ref()
                        .is_some_and(|hash| crypto::verify(password, hash)),
                AuthFailure::InvalidCredentials
            );
            let replacement = if credential
                .hash
                .as_ref()
                .is_some_and(|hash| hash.starts_with("scrypt:"))
            {
                crypto::hash(password).ok()
            } else {
                None
            };
            match self.command(
                Command::Login {
                    credential,
                    replacement,
                },
                cancelled,
            ) {
                Ok(Reply::Outcome(outcome)) => return Ok(outcome),
                Ok(_) => bail!("Unexpected authentication reply"),
                Err(error)
                    if attempt == 0
                        && matches!(
                            error.downcast_ref::<AuthFailure>(),
                            Some(AuthFailure::CredentialChanged)
                        ) =>
                {
                    // A concurrent upgrade may preserve the password. Reverify the
                    // current hash outside the writer, then retain the transaction CAS.
                    let Reply::Credential(Some(current)) =
                        self.command(Command::CredentialByEmail(email.clone()), cancelled)?
                    else {
                        bail!(AuthFailure::InvalidCredentials);
                    };
                    credential = current;
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!("login returns after its bounded retry")
    }
    fn perform(&self, request: Request, cancelled: &AtomicBool) -> Result<Outcome> {
        let command = match request {
            Request::Status => Command::Status,
            Request::IdentityExists {
                provider,
                provider_user_id,
            } => {
                validate_identity(provider, &provider_user_id)?;
                Command::IdentityExists {
                    provider,
                    provider_user_id,
                }
            }
            Request::Register {
                email,
                display_name,
                password,
            } => {
                validate_email(&email)?;
                Command::Register {
                    email: email.to_lowercase(),
                    display_name,
                    hash: crypto::hash(&password)?,
                    first_user: self.policy.first_user,
                }
            }
            Request::Login { email, password } => {
                let Reply::Credential(Some(credential)) =
                    self.command(Command::CredentialByEmail(email.to_lowercase()), cancelled)?
                else {
                    bail!(AuthFailure::InvalidCredentials);
                };
                return self.login(credential, &password, cancelled);
            }
            Request::ChangePassword {
                session,
                current,
                replacement,
            } => {
                let Reply::Credential(Some(credential)) = self.command(
                    Command::CredentialBySession(crypto::digest(session.expose())),
                    cancelled,
                )?
                else {
                    bail!(AuthFailure::SessionRequired);
                };
                ensure!(credential.hash.is_some(), AuthFailure::OAuthOnlyAccount);
                ensure!(
                    credential
                        .hash
                        .as_ref()
                        .is_some_and(|hash| crypto::verify(&current, hash)),
                    AuthFailure::CurrentPasswordIncorrect
                );
                Command::ChangePassword {
                    session: crypto::digest(session.expose()),
                    credential,
                    replacement: crypto::hash(&replacement)?,
                }
            }
            Request::ResetPassword { token, replacement } => Command::ResetPassword {
                token: crypto::digest(token.expose()),
                replacement: crypto::hash(&replacement)?,
            },
            Request::ResolveSession { token } => {
                Command::ResolveSession(crypto::digest(token.expose()))
            }
            Request::Logout { token } => Command::Logout(crypto::digest(token.expose())),
            Request::LogoutAll { session } => Command::LogoutAll(crypto::digest(session.expose())),
            Request::RequestReset { email } => Command::RequestReset(email.to_lowercase()),
            Request::OAuthLogin {
                provider,
                provider_user_id,
                email,
                display_name,
            } => {
                validate_identity(provider, &provider_user_id)?;
                if let Some(email) = &email {
                    validate_email(email)?;
                }
                Command::OAuthLogin {
                    provider,
                    provider_user_id,
                    email: email.map(|email| email.to_lowercase()),
                    display_name,
                    first_user: self.policy.first_user,
                }
            }
            Request::LinkIdentity {
                session,
                provider,
                provider_user_id,
            } => {
                validate_identity(provider, &provider_user_id)?;
                Command::LinkIdentity {
                    session: crypto::digest(session.expose()),
                    provider,
                    provider_user_id,
                }
            }
            Request::ListKeys { session } => Command::Keys {
                session: crypto::digest(session.expose()),
                action: keys::Action::List,
            },
            Request::CreateKey { session } => Command::Keys {
                session: crypto::digest(session.expose()),
                action: keys::Action::Create,
            },
            Request::RevokeKey { session, key_id } => Command::Keys {
                session: crypto::digest(session.expose()),
                action: keys::Action::Revoke(key_id),
            },
            Request::RotateKey { session, key_id } => Command::Keys {
                session: crypto::digest(session.expose()),
                action: keys::Action::Rotate(key_id),
            },
            Request::ResolveKey { tenant_id, key } => Command::ResolveKey { tenant_id, key },
        };
        match self.command(command, cancelled)? {
            Reply::Outcome(outcome) => Ok(outcome),
            _ => bail!("Unexpected authentication reply"),
        }
    }
}
fn validate_email(email: &str) -> Result<()> {
    ensure!(
        !email.is_empty() && email.len() <= 320 && email.contains('@'),
        AuthFailure::InvalidInput(AuthInputFailure::Email)
    );
    Ok(())
}
fn validate_identity(provider: Provider, id: &str) -> Result<()> {
    ensure!(
        provider != Provider::Email && !id.is_empty() && id.len() <= 512,
        AuthFailure::InvalidInput(AuthInputFailure::ProviderIdentity)
    );
    Ok(())
}

pub(super) struct Credential {
    user: User,
    hash: Option<String>,
}
pub(super) enum Reply {
    Credential(Option<Credential>),
    Outcome(Outcome),
}
pub(super) enum Command {
    #[cfg(feature = "desktop_rebuild")]
    WithLifetimes {
        command: Box<Command>,
        lifetimes: rebuild::AuthLifetimes,
    },
    Status,
    IdentityExists {
        provider: Provider,
        provider_user_id: String,
    },
    CredentialByEmail(String),
    CredentialBySession(String),
    Register {
        email: String,
        display_name: String,
        hash: String,
        first_user: FirstUserTenant,
    },
    Login {
        credential: Credential,
        replacement: Option<String>,
    },
    ResolveSession(String),
    Logout(String),
    LogoutAll(String),
    ChangePassword {
        session: String,
        credential: Credential,
        replacement: String,
    },
    RequestReset(String),
    ResetPassword {
        token: String,
        replacement: String,
    },
    OAuthLogin {
        provider: Provider,
        provider_user_id: String,
        email: Option<String>,
        display_name: String,
        first_user: FirstUserTenant,
    },
    LinkIdentity {
        session: String,
        provider: Provider,
        provider_user_id: String,
    },
    Keys {
        session: String,
        action: keys::Action,
    },
    ResolveKey {
        tenant_id: String,
        key: Secret,
    },
}
pub(crate) fn timestamp(after_seconds: i64) -> String {
    timestamp_at(time::OffsetDateTime::now_utc() + time::Duration::seconds(after_seconds))
}

/// `at` in Node's `toISOString` form, the form Checkoff link times use.
pub(crate) fn timestamp_at(now: time::OffsetDateTime) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        now.millisecond()
    )
}
fn read_user(row: &rusqlite::Row<'_>) -> rusqlite::Result<Credential> {
    let id: String = row.get(0)?;
    let email: Option<String> = row.get(1)?;
    let display: String = row.get(2)?;
    Ok(Credential {
        user: User {
            user_id: id.clone(),
            tenant_id: id,
            login: email.clone().unwrap_or_else(|| display.clone()),
            display_name: display,
            email,
            provider: Provider::Email,
            is_admin: row.get(4)?,
            has_password: row.get::<_, Option<String>>(3)?.is_some(),
        },
        hash: row.get(3)?,
    })
}
fn by_id(tx: &Transaction<'_>, id: &str) -> Result<Option<Credential>> {
    Ok(tx
        .query_row(
            "SELECT id,email,display_name,password_hash,is_admin FROM users WHERE id=?1",
            [id],
            read_user,
        )
        .optional()?)
}
fn by_email(tx: &Transaction<'_>, email: &str) -> Result<Option<Credential>> {
    Ok(tx
        .query_row(
            "SELECT id,email,display_name,password_hash,is_admin FROM users WHERE email=?1",
            [email],
            read_user,
        )
        .optional()?)
}
fn session_user(tx: &Transaction<'_>, token: &str) -> Result<Option<Credential>> {
    let session: Option<(String, String)> = tx
        .query_row(
            "SELECT user_id,provider FROM sessions WHERE id=?1 AND expires_at>?2",
            params![token, timestamp(0)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    match session {
        Some((id, provider)) => {
            let provider = Provider::from_name(&provider)?;
            let mut credential = by_id(tx, &id)?;
            if let Some(credential) = &mut credential {
                credential.user.provider = provider;
            }
            Ok(credential)
        }
        None => Ok(None),
    }
}
fn actor(tx: &Transaction<'_>, token: &str) -> Result<User> {
    session_user(tx, token)?
        .map(|c| c.user)
        .ok_or_else(|| anyhow!(AuthFailure::SessionRequired))
}
fn create_user(
    tx: &Transaction<'_>,
    email: Option<String>,
    display_name: String,
    hash: Option<String>,
    first_user: FirstUserTenant,
) -> Result<User> {
    ensure!(
        display_name.len() <= 512,
        AuthFailure::InvalidInput(AuthInputFailure::TooLong)
    );
    let first: bool = tx.query_row("SELECT NOT EXISTS(SELECT 1 FROM users)", [], |row| {
        row.get(0)
    })?;
    let id = if first && matches!(first_user, FirstUserTenant::ClaimDefault) {
        "default".to_owned()
    } else {
        let mut bytes = crypto::random::<16>()?;
        bytes[6] = (bytes[6] & 15) | 64;
        bytes[8] = (bytes[8] & 63) | 128;
        let value = hex::encode(bytes);
        format!(
            "{}-{}-{}-{}-{}",
            &value[..8],
            &value[8..12],
            &value[12..16],
            &value[16..20],
            &value[20..]
        )
    };
    let display_name = display_name.trim_matches(|c: char| matches!(c, '\u{0009}'..='\u{000d}' | ' ' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'));
    let display_name = if display_name.is_empty() {
        "User"
    } else {
        display_name
    };
    tx.execute("INSERT INTO users(id,email,display_name,password_hash,is_admin,created_at) VALUES(?1,?2,?3,?4,?5,?6)", params![id,email,display_name,hash,first,timestamp(0)])?;
    Ok(by_id(tx, &id)?
        .ok_or_else(|| anyhow!("User insert failed"))?
        .user)
}
fn create_session(
    tx: &Transaction<'_>,
    mut user: User,
    provider: Provider,
    policy: AuthPolicy,
    #[cfg(feature = "desktop_rebuild")] session_seconds: i64,
) -> Result<Outcome> {
    #[cfg(not(feature = "desktop_rebuild"))]
    let session_seconds = 14 * 24 * 60 * 60;
    user.tenant_id = policy::tenant_for_authenticated_actor(tx, &user, policy)?;
    user.provider = provider;
    let token = crypto::token()?;
    tx.execute(
        "INSERT INTO sessions(id,user_id,expires_at,provider) VALUES(?1,?2,?3,?4)",
        params![
            crypto::digest(token.expose()),
            user.user_id,
            timestamp(session_seconds),
            provider.name()
        ],
    )?;
    Ok(Outcome::Session { user, token })
}
fn invalidate(tx: &Transaction<'_>, id: &str) -> Result<()> {
    tx.execute("DELETE FROM sessions WHERE user_id=?1", [id])?;
    tx.execute("DELETE FROM password_reset_tokens WHERE user_id=?1", [id])?;
    Ok(())
}
fn identity(tx: &Transaction<'_>, provider: Provider, provider_id: &str) -> Result<Option<String>> {
    Ok(tx
        .query_row(
            "SELECT user_id FROM auth_identities WHERE provider=?1 AND provider_user_id=?2",
            params![provider.name(), provider_id],
            |row| row.get(0),
        )
        .optional()?)
}
fn link(tx: &Transaction<'_>, user: &str, provider: Provider, provider_id: &str) -> Result<()> {
    if let Some(existing) = identity(tx, provider, provider_id)? {
        ensure!(existing == user, "Identity already belongs to another user");
    } else {
        tx.execute(
            "INSERT INTO auth_identities(user_id,provider,provider_user_id) VALUES(?1,?2,?3)",
            params![user, provider.name(), provider_id],
        )?;
    }
    Ok(())
}
pub(super) fn execute(
    connection: &mut Connection,
    command: Command,
    policy: AuthPolicy,
    admission: crate::writer_admission::Deadline,
) -> Result<Reply> {
    #[cfg(feature = "desktop_rebuild")]
    let (command, lifetimes) = match command {
        Command::WithLifetimes { command, lifetimes } => (*command, lifetimes),
        command => (command, rebuild::AuthLifetimes::default()),
    };
    let tx = crate::writer_admission::begin_immediate(connection, admission)?;
    let outcome = match command {
        #[cfg(feature = "desktop_rebuild")]
        Command::WithLifetimes { .. } => bail!(AuthFailure::Storage),
        Command::Status => {
            return Ok(Reply::Outcome(Outcome::Status(policy::status(
                &tx, policy,
            )?)));
        }
        Command::IdentityExists {
            provider,
            provider_user_id,
        } => {
            return Ok(Reply::Outcome(Outcome::IdentityExists(
                identity(&tx, provider, &provider_user_id)?.is_some(),
            )));
        }
        Command::CredentialByEmail(email) => return Ok(Reply::Credential(by_email(&tx, &email)?)),
        Command::CredentialBySession(token) => {
            return Ok(Reply::Credential(session_user(&tx, &token)?));
        }
        Command::Register {
            email,
            display_name,
            hash,
            first_user,
        } => {
            policy::registration_allowed(&tx, policy)?;
            ensure!(
                by_email(&tx, &email)?.is_none(),
                AuthFailure::DuplicateEmail
            );
            let user = create_user(&tx, Some(email), display_name, Some(hash), first_user)?;
            create_session(
                &tx,
                user,
                Provider::Email,
                policy,
                #[cfg(feature = "desktop_rebuild")]
                lifetimes.session_seconds,
            )?
        }
        Command::Login {
            credential,
            replacement,
        } => {
            let current = by_id(&tx, &credential.user.user_id)?
                .ok_or_else(|| anyhow!(AuthFailure::CredentialChanged))?;
            ensure!(
                current.hash == credential.hash,
                AuthFailure::CredentialChanged
            );
            if let Some(hash) = replacement {
                tx.execute(
                    "UPDATE users SET password_hash=?1 WHERE id=?2",
                    params![hash, current.user.user_id],
                )?;
            }
            create_session(
                &tx,
                current.user,
                Provider::Email,
                policy,
                #[cfg(feature = "desktop_rebuild")]
                lifetimes.session_seconds,
            )?
        }
        Command::ResolveSession(token) => Outcome::User(
            session_user(&tx, &token)?
                .map(|credential| {
                    let mut user = credential.user;
                    user.tenant_id = policy::tenant_for_authenticated_actor(&tx, &user, policy)?;
                    Ok::<_, anyhow::Error>(user)
                })
                .transpose()?,
        ),
        Command::Logout(token) => {
            Outcome::Changed(tx.execute("DELETE FROM sessions WHERE id=?1", [token])? > 0)
        }
        Command::LogoutAll(token) => {
            let user = actor(&tx, &token)?;
            tx.execute("DELETE FROM sessions WHERE user_id=?1", [user.user_id])?;
            Outcome::Changed(true)
        }
        Command::ChangePassword {
            session,
            credential,
            replacement,
        } => {
            let user = actor(&tx, &session)?;
            ensure!(
                user.user_id == credential.user.user_id,
                AuthFailure::CredentialChanged
            );
            let changed = tx.execute(
                "UPDATE users SET password_hash=?1 WHERE id=?2 AND password_hash IS ?3",
                params![replacement, user.user_id, credential.hash],
            )?;
            ensure!(changed == 1, AuthFailure::CredentialChanged);
            invalidate(&tx, &user.user_id)?;
            create_session(
                &tx,
                user,
                Provider::Email,
                policy,
                #[cfg(feature = "desktop_rebuild")]
                lifetimes.session_seconds,
            )?
        }
        Command::RequestReset(email) => {
            if let Some(credential) = by_email(&tx, &email)? {
                let token = Secret(URL_SAFE_NO_PAD.encode(crypto::random::<32>()?));
                tx.execute(
                    "DELETE FROM password_reset_tokens WHERE user_id=?1",
                    [&credential.user.user_id],
                )?;
                tx.execute("INSERT INTO password_reset_tokens(id,user_id,expires_at,created_at) VALUES(?1,?2,?3,?4)", params![crypto::digest(token.expose()),credential.user.user_id,timestamp(3600),timestamp(0)])?;
                Outcome::ResetToken(Some(token))
            } else {
                Outcome::ResetToken(None)
            }
        }
        Command::ResetPassword { token, replacement } => {
            let id: Option<String> = tx
                .query_row(
                    "SELECT user_id FROM password_reset_tokens WHERE id=?1 AND expires_at>?2",
                    params![token, timestamp(0)],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(id) = id {
                ensure!(
                    tx.execute(
                        "UPDATE users SET password_hash=?1 WHERE id=?2",
                        params![replacement, id]
                    )? == 1,
                    "Reset user missing"
                );
                invalidate(&tx, &id)?;
                let user = by_id(&tx, &id)?
                    .ok_or_else(|| anyhow!("Reset user missing"))?
                    .user;
                create_session(
                    &tx,
                    user,
                    Provider::Email,
                    policy,
                    #[cfg(feature = "desktop_rebuild")]
                    lifetimes.session_seconds,
                )?
            } else {
                Outcome::Changed(false)
            }
        }
        Command::OAuthLogin {
            provider,
            provider_user_id,
            email,
            display_name,
            first_user,
        } => {
            let existing = match identity(&tx, provider, &provider_user_id)? {
                Some(id) => by_id(&tx, &id)?,
                None => {
                    policy::registration_allowed(&tx, policy)?;
                    match &email {
                        Some(email) => by_email(&tx, email)?,
                        None => None,
                    }
                }
            };
            let user = match existing {
                Some(c) => c.user,
                None => create_user(&tx, email, display_name, None, first_user)?,
            };
            link(&tx, &user.user_id, provider, &provider_user_id)?;
            create_session(
                &tx,
                user,
                provider,
                policy,
                #[cfg(feature = "desktop_rebuild")]
                lifetimes.session_seconds,
            )?
        }
        Command::LinkIdentity {
            session,
            provider,
            provider_user_id,
        } => {
            let user = actor(&tx, &session)?;
            link(&tx, &user.user_id, provider, &provider_user_id)?;
            Outcome::Changed(true)
        }
        Command::Keys { session, action } => {
            let user = actor(&tx, &session)?;
            let tenant = policy::tenant_for_authenticated_actor(&tx, &user, policy)?;
            keys::manage(
                &tx,
                &tenant,
                action,
                #[cfg(feature = "desktop_rebuild")]
                lifetimes.key_seconds,
            )?
        }
        Command::ResolveKey { tenant_id, key } => keys::resolve(&tx, &tenant_id, key)?,
    };
    tx.commit()?;
    Ok(Reply::Outcome(outcome))
}

pub(crate) fn read_session_tenant(
    tx: &Transaction<'_>,
    secret: &Secret,
    policy: AuthPolicy,
) -> Result<String> {
    ensure!(session_sized(secret), "Authentication required");
    policy::tenant_for_authenticated_actor(
        tx,
        &actor(tx, &crypto::digest(secret.expose()))?,
        policy,
    )
}
/// Whether a session token has a size [`read_session_tenant`] looks up.
pub(crate) fn session_sized(secret: &Secret) -> bool {
    !secret.expose().is_empty() && secret.expose().len() <= 4096
}
pub(crate) fn read_key_tenant(
    tx: &Transaction<'_>,
    routed_tenant: &str,
    secret: &Secret,
) -> Result<String> {
    ensure!(
        key_proves(tx, routed_tenant, secret)?,
        "Authentication required"
    );
    Ok(routed_tenant.to_owned())
}
/// Whether `secret` is an active, unexpired API key of `tenant`.
pub(crate) fn key_proves(tx: &Transaction<'_>, tenant: &str, secret: &Secret) -> Result<bool> {
    keys::proves(tx, tenant, secret)
}
pub(crate) fn catalog_timestamp() -> String {
    timestamp(0)
}
pub(crate) fn catalog_tenant(
    tx: &Transaction<'_>,
    credentials: crate::catalog::Credentials,
    policy: AuthPolicy,
) -> Result<String> {
    match credentials {
        crate::catalog::Credentials::Session(token) => {
            ensure!(token.expose().len() <= 4096, "Invalid session");
            read_session_tenant(tx, &token, policy)
        }
        crate::catalog::Credentials::Key { tenant_id, key } => {
            match keys::resolve(tx, &tenant_id, key)? {
                Outcome::KeyResolved {
                    principal: Some(principal),
                    ..
                } => Ok(principal.tenant_id),
                _ => bail!("Authentication required"),
            }
        }
    }
}
pub(crate) fn job_session_actor(
    tx: &Transaction<'_>,
    token: &str,
    policy: AuthPolicy,
) -> Result<(String, String)> {
    ensure!(
        token.len() <= 4096,
        AuthFailure::InvalidInput(AuthInputFailure::TooLong)
    );
    let actor = actor(tx, &crypto::digest(token))?;
    Ok((
        policy::tenant_for_authenticated_actor(tx, &actor, policy)?,
        format!("user:{}", actor.user_id),
    ))
}
pub(crate) fn job_key_actor(
    tx: &Transaction<'_>,
    tenant: &str,
    key: Secret,
) -> Result<(String, String)> {
    match keys::resolve(tx, tenant, key)? {
        Outcome::KeyResolved {
            principal: Some(principal),
            ..
        } => Ok((principal.tenant_id, format!("key:{}", principal.key_id))),
        _ => Err(AuthFailure::SessionRequired.into()),
    }
}

#[cfg(test)]
mod tests;
