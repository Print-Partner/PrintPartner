//! Leak canary: every error class each router maps, with a secret-looking
//! string in the underlying SQL, path or URL, must answer a problem whose body
//! and headers never contain that string.
use super::*;
use axum::{body::to_bytes, response::IntoResponse};
use pp_storage::{
    auth::{AuthFailure, AuthInputFailure},
    build_graph::{BudgetResource, Failure as BuildFailure},
    catalog::CatalogFailure,
    jobs::JobAccessFailure,
    working_drafts::Failure as DraftFailure,
};

const MARK: &str = "canary-7f3a";
const SQL: &str = "SELECT secret FROM canary-7f3a_users WHERE token = 'canary-7f3a'";
const PATH: &str = "/home/someone/canary-7f3a/print-partner.db";
const URL: &str = "http://canary-7f3a:canary-7f3a@127.0.0.1:1/oauth/token?code=canary-7f3a";

/// A real driver error, filesystem error and HTTP client error, each naming
/// the canary.
async fn underlying() -> Vec<anyhow::Error> {
    let sql = rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
        Some(format!("near \"{SQL}\": syntax error")),
    );
    let path = std::io::Error::new(std::io::ErrorKind::PermissionDenied, format!("open {PATH}"));
    let url = network_error().await;
    assert!(
        format!("{url:#}").contains(MARK),
        "reqwest no longer names the URL"
    );
    vec![
        anyhow::Error::new(sql).context(format!("query {SQL}")),
        anyhow::Error::new(path).context(format!("read {PATH}")),
        anyhow::Error::new(url).context(format!("fetch {URL}")),
    ]
}

async fn network_error() -> reqwest::Error {
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    client.get(URL).send().await.unwrap_err()
}

/// Every typed failure the mappers branch on, under a canary context, plus
/// the untyped messages they match and the raw underlying errors.
async fn classes() -> Vec<anyhow::Error> {
    use AuthFailure as A;
    use AuthInputFailure as I;
    use BuildFailure as B;
    use CatalogFailure as C;
    use DraftFailure as D;
    use JobAccessFailure as J;
    use pp_storage::required_units::AdmissionFailure as Q;
    macro_rules! errors {
        ($($error:expr),* $(,)?) => { vec![$(anyhow::Error::from($error)),*] };
    }
    #[rustfmt::skip]
    let mut typed: Vec<anyhow::Error> = errors![
        A::InvalidInput(I::TooLong), A::InvalidInput(I::PasswordTooShort), A::InvalidCredentials,
        A::SessionRequired, A::CurrentPasswordIncorrect, A::OAuthOnlyAccount, A::DuplicateEmail,
        A::RegistrationClosed, A::SingleAccountExists, A::OwnerMappingRequired,
        A::CredentialChanged, A::QueueFull, A::Stopped, A::CommitUnknown, A::Storage,
        B::Cancelled, B::TooManyBuilds, B::TooManyLayers, B::QueueFull, B::Stopped,
        B::OutcomeUnknown, B::BudgetExhausted { resource: BudgetResource::GraphRows },
        C::Input("Source name is required".into()), C::DuplicateName("Base".into()), C::NotFound,
        C::InvalidStoredNaming, C::Referenced, C::TooLarge, C::Conflict,
        D::Cancelled, D::QueueFull, D::Stopped, D::OutcomeUnknown, D::CorruptOrigin,
        Q::Cancelled, Q::QueueFull, Q::Stopped, Q::OutcomeUnknown,
        J::NotFound, J::InternalOnly, J::Capacity, J::Stopped,
    ];
    for message in [
        "Authentication required",
        "Writer queue full",
        "database is locked",
    ] {
        typed.push(anyhow::anyhow!(message));
    }
    let mut errors: Vec<anyhow::Error> = typed
        .into_iter()
        .map(|error| {
            error
                .context(format!("read {PATH}"))
                .context(format!("query {SQL}"))
        })
        .collect();
    errors.extend(underlying().await);
    errors
}

async fn assert_clean(label: &str, response: Response) {
    assert!(is_problem(&response), "{label}: not a problem");
    assert!(
        response.status().is_client_error() || response.status().is_server_error(),
        "{label}"
    );
    for (name, value) in response.headers() {
        assert!(
            !String::from_utf8_lossy(value.as_bytes()).contains(MARK),
            "{label}: header {name} leaks"
        );
    }
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let text = String::from_utf8_lossy(&bytes);
    for leak in [MARK, "SELECT", "/home/", "http://", "Caused by", "near \""] {
        assert!(!text.contains(leak), "{label}: body leaks {leak:?}: {text}");
    }
    let problem: Problem = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(problem.title, problem.problem_type.title(), "{label}");
}

type Mapper = fn(anyhow::Error) -> Response;

async fn problem_of(response: Response) -> Problem {
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Every router's storage-error mapper.
const MAPPERS: [(&str, Mapper); 7] = [
    ("auth", |error| {
        crate::auth::Failure::from(error).into_response()
    }),
    ("accepted", |error| crate::accepted::read_error(&error)),
    ("builds", crate::builds::domain_error),
    ("catalog", crate::catalog::catalog_error),
    ("catalog-naming", crate::catalog::naming_error),
    ("drafts", |error| {
        crate::drafts::storage_failure_with(&error, "Plan draft update failed")
    }),
    ("jobs", crate::jobs::domain_error),
];

#[tokio::test]
async fn problem_response_canary_never_leaks_sql_paths_or_urls() {
    let mappers = MAPPERS;
    let mut checked = 0;
    for (name, mapper) in mappers {
        for (index, error) in classes().await.into_iter().enumerate() {
            assert_clean(&format!("{name}#{index}"), mapper(error)).await;
            checked += 1;
        }
    }
    assert_clean(
        "oauth-network",
        crate::auth::network(network_error().await).into_response(),
    )
    .await;
    assert!(checked >= 7 * 48, "{checked}");
}

/// A full writer queue is retryable on every router; an unknown commit is
/// never a plain 500 or 401 that a client might blindly retry.
#[tokio::test]
async fn problem_response_typed_storage_failures_keep_their_type_on_every_router() {
    let cases: [(fn() -> anyhow::Error, ProblemType); 2] = [
        (|| AuthFailure::QueueFull.into(), ProblemType::Overloaded),
        (
            || AuthFailure::CommitUnknown.into(),
            ProblemType::OutcomeUnknown,
        ),
    ];
    for (name, mapper) in MAPPERS {
        for (error, kind) in cases {
            let response = mapper(error().context(format!("query {SQL}")));
            assert_eq!(
                response.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{name} {kind:?}"
            );
            let problem = problem_of(response).await;
            assert_eq!(problem.problem_type, kind, "{name}");
        }
    }
    let queue = crate::accepted::read_error(&anyhow::anyhow!("Writer queue full"));
    let problem = problem_of(queue).await;
    assert_eq!(problem.problem_type, ProblemType::Overloaded);
}

/// Manifest observation failures send only a closed `ReadFailure` name as
/// `reason`; a message carrying a path is dropped.
#[tokio::test]
async fn problem_response_canary_covers_manifest_failures() {
    use pp_storage::build_graph::ManifestOptionsOutcome as Outcome;
    let present = |outcome| crate::builds::present_manifest(&axum::http::Method::GET, outcome);
    let leaked = Outcome::ObservationFailed {
        detail: format!("Io(NotFound) {PATH}"),
    };
    assert_clean("manifest-observation", present(leaked)).await;
    let named = present(Outcome::ObservationFailed {
        detail: "Io(NotFound)".into(),
    });
    let bytes = to_bytes(named.into_body(), 1 << 20).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["reason"], "Io(NotFound)");

    // An invalid-input detail is pp-storage text plus the request's own keys,
    // so it carries no server secret; it is still bounded and decodable.
    let long = pp_storage::manifest_options::ManifestInputDetail::from("k".repeat(4096));
    let response = present(Outcome::InvalidInput { detail: long });
    assert!(is_problem(&response));
    let problem = problem_of(response).await;
    assert_eq!(problem.problem_type, ProblemType::Validation);
    assert!(problem.detail.unwrap().len() <= MAX_DETAIL_BYTES);
}
