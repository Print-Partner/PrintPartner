use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};

pub(super) async fn network_admission(mut request: Request, next: Next) -> Response {
    let Ok((origin, host)) = pp_net::inbound::origin(&request) else {
        return StatusCode::FORBIDDEN.into_response();
    };
    request
        .extensions_mut()
        .insert(pp_api::NetworkAdmission { origin, host });
    next.run(request).await
}
