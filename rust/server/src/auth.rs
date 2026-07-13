//! Basic auth middleware matching `packages/server/src/middleware/authorization.ts`.

use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::Engine;
use serde_json::json;

#[derive(Clone)]
pub struct ServerAuth {
    pub username: String,
    pub password: Option<String>,
}

impl ServerAuth {
    pub fn enabled(&self) -> bool {
        self.password
            .as_ref()
            .is_some_and(|password| !password.is_empty())
    }
}

pub async fn middleware(State(auth): State<ServerAuth>, request: Request, next: Next) -> Response {
    if !auth.enabled() {
        return next.run(request).await;
    }
    let Some(password) = auth.password.as_deref() else {
        return next.run(request).await;
    };
    let authorized = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(decode_basic)
        .is_some_and(|(username, secret)| username == auth.username && secret == password);
    if authorized {
        return next.run(request).await;
    }
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, r#"Basic realm="Secure Area""#)],
        axum::Json(json!({ "message": "Authentication required" })),
    )
        .into_response()
}

fn decode_basic(value: &str) -> Option<(String, String)> {
    let encoded = value.strip_prefix("Basic ")?.trim();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (username, password) = text.split_once(':')?;
    Some((username.to_string(), password.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_basic_credentials() {
        let header = "Basic b3BlbmNvZGU6c2VjcmV0";
        let (username, password) = decode_basic(header).expect("decode");
        assert_eq!(username, "opencode");
        assert_eq!(password, "secret");
    }
}
