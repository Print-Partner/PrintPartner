//! Rust-serialized fixtures for the account routes in the client contract.
//! Responses come from the same structs the handlers send; request cases record
//! whether the contract's request type accepts the body.

use super::wire::{
    AuthStatus, CurrentUser, PublicUser, RegisterRequest, SignInRequest, SignedIn, SignedOut,
};
use crate::client_contract::codegen::{request, served};
use pp_storage::auth::Provider;
use serde_json::{Value, json};

fn owner(email: Option<&str>) -> PublicUser {
    PublicUser {
        user_id: "user-1".into(),
        login: "owner".into(),
        display_name: "Ünïcødé 🧵 owner".into(),
        email: email.map(str::to_owned),
        provider: Provider::Email,
        is_admin: true,
        has_password: true,
    }
}

pub(crate) fn fixtures() -> Result<Vec<Value>, serde_json::Error> {
    let mut cases = vec![
        served(
            "AuthStatus",
            "first-run",
            AuthStatus {
                setup_required: true,
            },
        )?,
        served(
            "AuthStatus",
            "owner-exists",
            AuthStatus {
                setup_required: false,
            },
        )?,
        served(
            "CurrentUser",
            "owner",
            CurrentUser {
                user: owner(Some("owner@example.com")),
                multi_user: false,
            },
        )?,
        served(
            "SignedIn",
            "owner",
            SignedIn {
                user: owner(Some("owner@example.com")),
            },
        )?,
        served("SignedIn", "no-email", SignedIn { user: owner(None) })?,
        served("SignedOut", "ok", SignedOut { ok: true })?,
    ];
    for (name, value) in [
        (
            "filled",
            json!({"email": "owner@example.com", "password": "password-1234"}),
        ),
        ("missing-password", json!({"email": "owner@example.com"})),
        (
            "extra-key",
            json!({"email": "a@b.c", "password": "x", "remember": true}),
        ),
    ] {
        cases.push(request::<SignInRequest>("SignInRequest", name, value));
    }
    for (name, value) in [
        (
            "filled",
            json!({"email": "owner@example.com", "password": "password-1234"}),
        ),
        (
            "display-name",
            json!({"email": "owner@example.com", "password": "password-1234", "display_name": "Owner"}),
        ),
        ("missing-email", json!({"password": "password-1234"})),
        (
            "extra-key",
            json!({"email": "a@b.c", "password": "x", "admin": true}),
        ),
    ] {
        cases.push(request::<RegisterRequest>("RegisterRequest", name, value));
    }
    Ok(cases)
}
