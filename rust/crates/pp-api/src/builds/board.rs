use crate::{
    accepted::{AcceptedHttpConfig, Gate, internal, ok_json},
    loopback::PresentedCredential,
    problem::{self, Problem, ProblemCode, ProblemType, ToProblem},
};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, State},
    response::Response,
    routing::get,
};
use pp_storage::build_graph::board::{Client, Entry, Error};
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

impl ToProblem for Error {
    fn to_problem(&self) -> Problem {
        let (kind, status, code) = match self {
            Self::BuildNotFound => (ProblemType::NotFound, 404, ProblemCode::BoardBuildNotFound),
            Self::InvalidNote => (ProblemType::Validation, 400, ProblemCode::InvalidBoardNote),
            Self::BuildFull => (ProblemType::Conflict, 409, ProblemCode::BoardNoteLimit),
            Self::Storage(error) => return error.to_problem(),
        };
        Problem::new(kind, status, &self.to_string()).with_problem_code(code)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub(crate) struct CreateBoardNoteRequest {
    build_id: i64,
    body: String,
}

#[derive(Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub(crate) struct BoardNote {
    id: String,
    build_id: i64,
    build_name: String,
    body: String,
    created_at: String,
}

impl From<Entry> for BoardNote {
    fn from(entry: Entry) -> Self {
        Self {
            id: entry.note.id,
            build_id: entry.build_id,
            build_name: entry.build_name,
            body: entry.note.body,
            created_at: entry.note.created_at,
        }
    }
}

#[derive(Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub(crate) struct BoardFeed {
    notes: Vec<BoardNote>,
}

#[derive(Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
struct BuildChoice {
    id: i64,
    name: String,
}

#[derive(Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub(crate) struct BoardBuilds {
    builds: Vec<BuildChoice>,
}

#[cfg(feature = "codegen")]
pub(crate) fn fixtures() -> Result<Vec<serde_json::Value>, serde_json::Error> {
    use crate::client_contract::codegen::served;

    let request = CreateBoardNoteRequest {
        build_id: 1,
        body: "Check the enclosure fit.\nKeep the spare clips.".into(),
    };
    let note = || BoardNote {
        id: "d38981f6611d4dcbaa461796432aeddb".into(),
        build_id: request.build_id,
        build_name: "Enclosure".into(),
        body: request.body.clone(),
        created_at: "2026-11-27T12:00:00.000Z".into(),
    };
    Ok(vec![
        served("BoardNote", "note", note())?,
        served(
            "BoardFeed",
            "populated",
            BoardFeed {
                notes: vec![note()],
            },
        )?,
        served("BoardFeed", "empty", BoardFeed { notes: vec![] })?,
        served("CreateBoardNoteRequest", "add-note", request)?,
        served(
            "BoardBuilds",
            "choices",
            BoardBuilds {
                builds: vec![BuildChoice {
                    id: 1,
                    name: "Enclosure".into(),
                }],
            },
        )?,
    ])
}

pub fn board_router(config: AcceptedHttpConfig, client: Client) -> Router {
    let Some(gate) = Gate::open(config) else {
        return Router::new();
    };
    gate.guard(
        Router::new()
            .route("/api/v2/board/notes", get(list).post(create))
            .route("/api/v2/board/builds", get(builds))
            .layer(DefaultBodyLimit::max(16 * 1024)),
    )
    .with_state((gate, client))
}

async fn builds(
    State((gate, client)): State<(Gate, Client)>,
    Extension(credential): Extension<PresentedCredential>,
) -> Response {
    match gate
        .blocking(move |cancelled, wait| {
            client.builds(credential.into_credential(), &cancelled, wait)
        })
        .await
    {
        Ok(Ok(builds)) => ok_json(&BoardBuilds {
            builds: builds
                .into_iter()
                .map(|build| BuildChoice {
                    id: build.id,
                    name: build.name,
                })
                .collect(),
        }),
        Ok(Err(error)) => problem::writer_failure(&error),
        Err(_) => internal(),
    }
}

async fn list(
    State((gate, client)): State<(Gate, Client)>,
    Extension(credential): Extension<PresentedCredential>,
) -> Response {
    match gate
        .blocking(move |cancelled, wait| {
            client.list(credential.into_credential(), &cancelled, wait)
        })
        .await
    {
        Ok(Ok(notes)) => ok_json(&BoardFeed {
            notes: notes.into_iter().map(BoardNote::from).collect(),
        }),
        Ok(Err(error)) => problem::writer_failure(&error),
        Err(_) => internal(),
    }
}

async fn create(
    State((gate, client)): State<(Gate, Client)>,
    Extension(credential): Extension<PresentedCredential>,
    Json(request): Json<CreateBoardNoteRequest>,
) -> Response {
    match gate
        .blocking(move |cancelled, wait| {
            client.create(
                credential.into_credential(),
                request.build_id,
                request.body,
                &cancelled,
                wait,
            )
        })
        .await
    {
        Ok(Ok(note)) => ok_json(&BoardNote::from(note)),
        Ok(Err(error)) => problem::refused(&error),
        Err(_) => internal(),
    }
}
