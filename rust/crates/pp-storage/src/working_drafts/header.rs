//! Draft header fields (GRE-1197): a Build's name, order number, customer and
//! due date as a draft proposes them. `PUT /plans/{id}/drafts/header` saves
//! them into the open draft the caller observed, or into a new draft when
//! none is open, in one immediate transaction. Nothing reaches the Build until
//! Accept.
//!
//! A draft with no `plan_draft_headers` row leaves the Build's header
//! unchanged. Every path that writes a new draft from an older one copies the
//! row ([`carry`]). A save names the header version it observed, so a stale
//! write is refused; a save that asks for the header the draft already holds
//! leaves the header alone. Every committed save stores an immutable receipt
//! under its key, so a retry replays its first answer and a reused key with a
//! different payload is refused.
//!
//! Compiled with `desktop_rebuild`; service callers must check `PP_DESKTOP_REBUILD`.
use super::save::{Refusal, accepted_base, observed_draft, prepare_new, tenant_actor};
use super::*;
pub use pp_contracts::draft_header::{DraftHeader, SaveDraftHeaderRequest};

/// A stored Build header: trimmed and checked, as a draft holds it and as
/// the Build reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct BuildHeader {
    pub name: String,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub order_number: Option<String>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub customer: Option<String>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub due_date: Option<String>,
}
use pp_domain::build_identity::trim_js;

/// The longest header text in characters, as for a Build name.
pub const MAX_HEADER_TEXT: usize = 4096;
const KEY_PREFIX: &str = "draft-header-v1:";

/// A draft's saved header and its version.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct DraftHeaderState {
    pub header_version: u32,
    pub header: BuildHeader,
    pub updated_at: String,
}

/// A draft's header read: the Build's current header beside the header the
/// draft proposes. A draft with no header row (`draft: null`) leaves the
/// Build's header unchanged.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct HeaderView {
    pub draft_id: i64,
    pub build: BuildHeader,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<DraftHeaderState>")
    )]
    pub draft: Option<DraftHeaderState>,
}

/// What a header save wrote. A replay answers the same draft and header.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct DraftHeaderReceipt {
    pub profile_id: i64,
    pub draft_id: i64,
    pub header: DraftHeaderState,
}

/// A header save's answer: the receipt and the draft's header read, both
/// from the save's transaction.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct DraftHeaderSaved {
    pub receipt: DraftHeaderReceipt,
    pub header: HeaderView,
}

#[derive(Debug)]
pub enum HeaderOutcome {
    Saved(Box<DraftHeaderSaved>),
    Refused { reason: HeaderRefusal },
}

pub struct HeaderCommand {
    credential: Credential,
    profile: PositiveId,
    request: SaveDraftHeaderRequest,
    key: String,
}
impl HeaderCommand {
    /// The key is the `Idempotency-Key` header, trimmed as JavaScript trims,
    /// 1 to 160 UTF-16 units.
    pub fn new(
        credential: Credential,
        profile: PositiveId,
        request: SaveDraftHeaderRequest,
        key: &str,
    ) -> Result<Self> {
        let key = JsText::scalar(key)
            .trim_ecmascript()
            .as_scalar()
            .ok_or_else(|| anyhow!("Invalid draft header idempotency key"))?;
        ensure!(
            !key.is_empty() && key.encode_utf16().count() <= 160,
            "Invalid draft header idempotency key"
        );
        Ok(Self {
            credential,
            profile,
            request,
            key,
        })
    }
}

impl WorkingDraftClient {
    pub fn save_header(
        &self,
        input: HeaderCommand,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<HeaderOutcome> {
        self.enqueue_observed(
            input.credential,
            input.profile,
            cancelled,
            wait,
            |observations, reply| Operation::Header {
                request: input.request,
                key: input.key,
                observations,
                reply,
            },
        )
    }
}

#[derive(Debug)]
pub enum HeaderRefusal {
    /// The header failed a field rule; the field and the rule.
    Invalid {
        field: &'static str,
        detail: &'static str,
    },
    /// The draft's header version is not the one the caller observed.
    HeaderChanged {
        current: Option<Box<DraftHeaderState>>,
    },
    Draft(Refusal),
}

/// Trims every text and checks each field, or names the first that fails.
pub fn normalized(
    header: &DraftHeader,
) -> std::result::Result<BuildHeader, (&'static str, &'static str)> {
    let text = |field, value: &str| {
        let value = trim_js(value);
        if value.is_empty() || value.chars().count() > MAX_HEADER_TEXT {
            return Err((field, "text_length"));
        }
        Ok(value.to_owned())
    };
    let optional =
        |field, value: &Option<String>| value.as_deref().map(|v| text(field, v)).transpose();
    let name = text("name", &header.name)?;
    let order_number = optional("order_number", &header.order_number)?;
    let customer = optional("customer", &header.customer)?;
    let due_date = header.due_date.as_deref().map(trim_js);
    if due_date.is_some_and(|date| !calendar_date(date)) {
        return Err(("due_date", "calendar_date"));
    }
    Ok(BuildHeader {
        name,
        order_number,
        customer,
        due_date: due_date.map(str::to_owned),
    })
}

/// `YYYY-MM-DD` naming a real day.
fn calendar_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    let digits = |range: std::ops::Range<usize>| {
        bytes[range.clone()]
            .iter()
            .all(u8::is_ascii_digit)
            .then(|| text[range].parse::<u32>().ok())
            .flatten()
    };
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let (Some(year), Some(month), Some(day)) = (digits(0..4), digits(5..7), digits(8..10)) else {
        return false;
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    year >= 1 && (1..=days).contains(&day)
}

/// The saved header of draft `draft`, if it has one.
pub(crate) fn read(
    tx: &Transaction<'_>,
    tenant: &str,
    draft: i64,
) -> Result<Option<DraftHeaderState>> {
    let row = tx
        .query_row(
            "SELECT name,order_number,customer,due_date,header_version,updated_at FROM plan_draft_headers WHERE tenant_id=?1 AND draft_id=?2",
            params![tenant, draft],
            |row| {
                Ok(DraftHeaderState {
                    header: BuildHeader {
                        name: row.get(0)?,
                        order_number: row.get(1)?,
                        customer: row.get(2)?,
                        due_date: row.get(3)?,
                    },
                    header_version: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            },
        );
    match row {
        Ok(state) => Ok(Some(state)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// The Build's current header: name and order number from `build_profiles`,
/// customer and due date from `build_profile_details`.
pub(crate) fn build_header(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
) -> Result<BuildHeader> {
    Ok(tx.query_row(
        "SELECT p.name,p.order_number,d.customer,d.due_date FROM build_profiles p LEFT JOIN build_profile_details d ON d.profile_id=p.id AND d.tenant_id=p.tenant_id WHERE p.tenant_id=?1 AND p.id=?2",
        params![tenant, profile],
        |row| {
            Ok(BuildHeader {
                name: row.get(0)?,
                order_number: row.get(1)?,
                customer: row.get(2)?,
                due_date: row.get(3)?,
            })
        },
    )?)
}

/// Copies draft `from`'s header row, if it has one, to the new draft `to`
/// unchanged. Every path that writes a draft from an older one calls this.
pub(crate) fn carry(tx: &Transaction<'_>, tenant: &str, from: i64, to: i64) -> Result<()> {
    tx.execute(
        "INSERT INTO plan_draft_headers (draft_id,tenant_id,name,order_number,customer,due_date,header_version,updated_at) SELECT ?3,tenant_id,name,order_number,customer,due_date,header_version,updated_at FROM plan_draft_headers WHERE tenant_id=?1 AND draft_id=?2",
        params![tenant, from, to],
    )?;
    Ok(())
}

fn write(
    tx: &Transaction<'_>,
    tenant: &str,
    draft: i64,
    header: &BuildHeader,
    version: u32,
) -> Result<()> {
    tx.execute(
        "INSERT INTO plan_draft_headers (draft_id,tenant_id,name,order_number,customer,due_date,header_version,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(draft_id) DO UPDATE SET name=excluded.name,order_number=excluded.order_number,customer=excluded.customer,due_date=excluded.due_date,header_version=excluded.header_version,updated_at=excluded.updated_at",
        params![
            draft,
            tenant,
            header.name,
            header.order_number,
            header.customer,
            header.due_date,
            version,
            auth::catalog_timestamp(),
        ],
    )?;
    Ok(())
}

pub(super) struct Args<'a> {
    pub credential: &'a Credential,
    pub policy: auth::AuthPolicy,
    pub profile: PositiveId,
    pub request: &'a SaveDraftHeaderRequest,
    pub key: &'a str,
    pub observations: &'a Observations,
    pub cancelled: &'a Arc<AtomicBool>,
}

pub(super) fn execute(
    connection: &mut Connection,
    args: Args<'_>,
    admission: writer_admission::Deadline,
) -> Result<HeaderOutcome> {
    ensure!(!args.cancelled.load(Ordering::Acquire), Failure::Cancelled);
    let mut tx = writer_admission::begin_immediate(connection, admission)?;
    let saved = match transact(&mut tx, &args)? {
        Ok(saved) => saved,
        Err(reason) => return Ok(HeaderOutcome::Refused { reason }),
    };
    ensure!(!args.cancelled.load(Ordering::Acquire), Failure::Cancelled);
    tx.commit()?;
    Ok(HeaderOutcome::Saved(Box::new(saved)))
}

fn transact(
    tx: &mut Transaction<'_>,
    c: &Args<'_>,
) -> Result<std::result::Result<DraftHeaderSaved, HeaderRefusal>> {
    let header = match normalized(&c.request.header) {
        Ok(header) => header,
        Err((field, detail)) => return Ok(Err(HeaderRefusal::Invalid { field, detail })),
    };
    let (tenant, actor) = tenant_actor(tx, c.credential, c.policy)?;
    let tenant = tenant.as_str();
    let profile = i64::try_from(c.profile.get())?;
    let Some(p) = one(
        tx,
        "SELECT * FROM build_profiles WHERE tenant_id=? AND id=?",
        &[&tenant, &profile],
    )?
    else {
        return Ok(Err(HeaderRefusal::Draft(Refusal::NotFound)));
    };
    let (key, digest) = receipt_key(c, profile, &actor, &header);
    // The receipt is read before any stale check, so a committed retry
    // replays its first answer even after the header or draft moved on.
    if let Some((draft, prior, state)) = receipt(tx, tenant, profile, &actor, &key)? {
        if prior != digest {
            return Ok(Err(HeaderRefusal::Draft(Refusal::IdempotencyConflict)));
        }
        return Ok(Ok(saved(tx, tenant, profile, draft, state)?));
    }
    let (draft, state) = match write_observed(tx, c, (tenant, &actor, profile), &p, &key, header)? {
        Ok(written) => written,
        Err(reason) => return Ok(Err(reason)),
    };
    tx.execute(
        "INSERT INTO plan_draft_header_receipts (tenant_id,profile_id,actor_id,idempotency_key,payload_digest,draft_id,name,order_number,customer,due_date,header_version,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        params![
            tenant,
            profile,
            actor,
            key,
            digest,
            draft,
            state.header.name,
            state.header.order_number,
            state.header.customer,
            state.header.due_date,
            state.header_version,
            state.updated_at,
        ],
    )?;
    Ok(Ok(saved(tx, tenant, profile, draft, state)?))
}

/// The stored receipt key, the request key's SHA-256, and the digest of
/// everything the save was asked to do, which tells a changed payload apart.
fn receipt_key(c: &Args<'_>, profile: i64, actor: &str, header: &BuildHeader) -> (String, String) {
    use sha2::{Digest as _, Sha256};
    let key = format!(
        "{KEY_PREFIX}{}",
        hex::encode(Sha256::digest(c.key.as_bytes()))
    );
    let base = &c.request.expected_base;
    let draft = c.request.expected_draft.as_ref().map(|d| {
        json!({"id":d.draft_id(),"snapshotDigest":d.snapshot_digest(),"lifecycleVersion":d.lifecycle_version()})
    });
    let payload = json!({"profileId":profile,"actorId":actor,"expectedBase":{"revisionId":base.revision_id(),"planVersion":base.plan_version()},"expectedDraft":draft,"expectedHeaderVersion":c.request.expected_header_version,"header":header});
    (key, required_units::model::digest(&payload))
}

/// The receipt key `key` stored: its draft, payload digest and header.
fn receipt(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    actor: &str,
    key: &str,
) -> Result<Option<(i64, String, DraftHeaderState)>> {
    let row = tx.query_row(
        "SELECT draft_id,payload_digest,name,order_number,customer,due_date,header_version,updated_at FROM plan_draft_header_receipts WHERE tenant_id=?1 AND profile_id=?2 AND actor_id=?3 AND idempotency_key=?4",
        params![tenant, profile, actor, key],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                DraftHeaderState {
                    header: BuildHeader {
                        name: row.get(2)?,
                        order_number: row.get(3)?,
                        customer: row.get(4)?,
                        due_date: row.get(5)?,
                    },
                    header_version: row.get(6)?,
                    updated_at: row.get(7)?,
                },
            ))
        },
    );
    match row {
        Ok(found) => Ok(Some(found)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Checks the observed base, draft and header version, then writes `header`
/// into the open draft, or into a new draft when none is open. Returns the
/// draft and the header it now holds.
fn write_observed(
    tx: &mut Transaction<'_>,
    c: &Args<'_>,
    (tenant, actor, profile): (&str, &str, i64),
    p: &Value,
    key: &str,
    header: BuildHeader,
) -> Result<std::result::Result<(i64, DraftHeaderState), HeaderRefusal>> {
    let base = match accepted_base(tx, tenant, p, &c.request.expected_base)? {
        Ok(base) => base,
        Err(reason) => return Ok(Err(HeaderRefusal::Draft(reason))),
    };
    let mut budget = observation::PreparationBudget::new(c.cancelled, c.observations.limits)?;
    budget.check()?;
    let observed = match observed_draft(
        tx,
        tenant,
        profile,
        &base,
        c.request.expected_draft.as_ref(),
        &mut budget,
    )? {
        Ok(observed) => observed,
        Err(reason) => return Ok(Err(HeaderRefusal::Draft(reason))),
    };
    let draft = match observed {
        Some(d) => {
            let id = num(&d.header, "id")?;
            let current = read(tx, tenant, id)?;
            if let Some(state) = current.as_ref().filter(|state| state.header == header) {
                return Ok(Ok((id, state.clone())));
            }
            let version = current.as_ref().map(|state| state.header_version);
            if version != c.request.expected_header_version {
                return Ok(Err(HeaderRefusal::HeaderChanged {
                    current: current.map(Box::new),
                }));
            }
            let next = version.map_or(Ok(1), |v| {
                v.checked_add(1)
                    .filter(|next| *next <= i32::MAX as u32)
                    .ok_or_else(|| anyhow!("Draft header version overflow"))
            })?;
            write(tx, tenant, id, &header, next)?;
            crate::domain_events::draft_changed(tx, tenant, profile, id)?;
            id
        }
        None => {
            if c.request.expected_header_version.is_some() {
                return Ok(Err(HeaderRefusal::HeaderChanged { current: None }));
            }
            let prepared =
                match prepare_new(tx, tenant, profile, &base, c.observations, &mut budget)? {
                    Ok(prepared) => prepared,
                    Err(reason) => return Ok(Err(HeaderRefusal::Draft(reason))),
                };
            budget.check()?;
            let id = insert_snapshot(tx, tenant, actor, profile, key, &prepared, Origin::New)?;
            write(tx, tenant, id, &header, 1)?;
            id
        }
    };
    let state = read(tx, tenant, draft)?.ok_or_else(|| anyhow!("Saved draft header is missing"))?;
    Ok(Ok((draft, state)))
}

fn saved(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    draft: i64,
    header: DraftHeaderState,
) -> Result<DraftHeaderSaved> {
    Ok(DraftHeaderSaved {
        receipt: DraftHeaderReceipt {
            profile_id: profile,
            draft_id: draft,
            header,
        },
        header: view(tx, tenant, profile, draft)?,
    })
}

/// The header read of draft `draft` of Plan `profile`.
pub(crate) fn view(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    draft: i64,
) -> Result<HeaderView> {
    Ok(HeaderView {
        draft_id: draft,
        build: build_header(tx, tenant, profile)?,
        draft: read(tx, tenant, draft)?,
    })
}

#[cfg(test)]
mod tests {
    use super::{DraftHeader, calendar_date, normalized};

    #[test]
    fn normalized_trims_every_field_and_names_the_first_failure_in_order() {
        let header = |name: &str, due: &str| DraftHeader {
            name: name.into(),
            order_number: None,
            customer: None,
            due_date: Some(due.into()),
        };
        let saved = normalized(&header(" Kit ", " 2026-12-01 ")).unwrap();
        assert_eq!(
            (saved.name.as_str(), saved.due_date.as_deref()),
            ("Kit", Some("2026-12-01"))
        );
        assert_eq!(
            normalized(&header(" ", "2026-02-30")).unwrap_err(),
            ("name", "text_length")
        );
        assert_eq!(
            normalized(&header("Kit", " 2026-02-30 ")).unwrap_err(),
            ("due_date", "calendar_date")
        );
    }

    #[test]
    fn calendar_date_accepts_only_real_days() {
        for good in [
            "2026-11-06",
            "2024-02-29",
            "2000-02-29",
            "0001-01-01",
            "2026-12-31",
        ] {
            assert!(calendar_date(good), "{good}");
        }
        for bad in [
            "2026-02-29",
            "1900-02-29",
            "2026-13-01",
            "2026-00-10",
            "2026-04-31",
            "0000-01-01",
            "2026-1-01",
            "2026/11/06",
            "20261106",
            "2026-11-06T00:00",
            "+026-11-06",
            "",
        ] {
            assert!(!calendar_date(bad), "{bad}");
        }
    }
}
