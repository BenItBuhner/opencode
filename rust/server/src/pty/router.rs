//! Location-agnostic Axum router for the PTY slice. The production build
//! composes these routes into the larger `App` router with the location and
//! authentication middleware wrapped around them, but this smaller router is
//! kept public so integration tests can exercise the wire protocol without
//! having to construct the full `App` state.

use axum::extract::{Path, Query, State, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{protocol, session, ticket};

const TICKET_HEADER: &str = "x-opencode-ticket";
const TICKET_HEADER_VALUE: &str = "1";

/// Cloneable state handed to the PTY router.
#[derive(Clone)]
pub struct State_ {
    pub registry: session::Registry,
    pub tickets: ticket::Registry,
}

pub fn router(state: State_) -> Router {
    Router::new()
        .route("/api/pty", get(list).post(create))
        .route("/api/pty/{id}", get(fetch).put(update).delete(remove))
        .route("/api/pty/{id}/connect-token", post(connect_token))
        .route("/api/pty/{id}/connect", get(connect))
        .with_state(state)
}

fn not_found(id: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "_tag": "PtyNotFoundError",
            "ptyID": id,
            "message": format!("PTY session not found: {id}"),
        })),
    )
        .into_response()
}

fn forbidden(message: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "_tag": "ForbiddenError", "message": message })),
    )
        .into_response()
}

#[derive(Deserialize, Default)]
struct LocationQuery {
    #[serde(rename = "location[directory]")]
    directory: Option<String>,
    #[serde(rename = "location[workspace]")]
    workspace: Option<String>,
}

fn scope_from(query: &LocationQuery, id: &str) -> ticket::Scope {
    ticket::Scope {
        pty_id: id.into(),
        directory: query.directory.clone(),
        workspace_id: query.workspace.clone(),
    }
}

async fn list(State(state): State<State_>) -> Response {
    Json(json!({
        "data": state
            .registry
            .list()
            .into_iter()
            .map(|info| serde_json::to_value(info).expect("serializable"))
            .collect::<Value>(),
    }))
    .into_response()
}

async fn create(
    State(state): State<State_>,
    Json(payload): Json<session::CreateInput>,
) -> Response {
    match state.registry.create(payload) {
        Ok(info) => Json(json!({ "data": info })).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "message": error.to_string() })),
        )
            .into_response(),
    }
}

async fn fetch(State(state): State<State_>, Path(id): Path<String>) -> Response {
    match state.registry.get(&id) {
        Ok(info) => Json(json!({ "data": info })).into_response(),
        Err(_) => not_found(&id),
    }
}

async fn update(
    State(state): State<State_>,
    Path(id): Path<String>,
    Json(payload): Json<session::UpdateInput>,
) -> Response {
    match state.registry.update(&id, payload) {
        Ok(info) => Json(json!({ "data": info })).into_response(),
        Err(session::Error::NotFound(_)) => not_found(&id),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "message": error.to_string() })),
        )
            .into_response(),
    }
}

async fn remove(State(state): State<State_>, Path(id): Path<String>) -> Response {
    match state.registry.remove(&id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(session::Error::NotFound(_)) => not_found(&id),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "message": error.to_string() })),
        )
            .into_response(),
    }
}

async fn connect_token(
    State(state): State<State_>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<LocationQuery>,
) -> Response {
    let header_ok = headers
        .get(TICKET_HEADER)
        .and_then(|value| value.to_str().ok())
        == Some(TICKET_HEADER_VALUE);
    if !header_ok {
        return forbidden("Invalid PTY connect token request");
    }
    if state.registry.get(&id).is_err() {
        return not_found(&id);
    }
    let token = state.tickets.issue(scope_from(&query, &id));
    Json(json!({ "data": token })).into_response()
}

#[derive(Deserialize)]
struct ConnectQuery {
    #[serde(flatten)]
    location: LocationQuery,
    #[serde(default)]
    ticket: Option<String>,
    #[serde(default)]
    cursor: Option<String>,
}

async fn connect(
    State(state): State<State_>,
    Path(id): Path<String>,
    Query(query): Query<ConnectQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    if state.registry.get(&id).is_err() {
        return not_found(&id);
    }
    if let Some(ticket) = query.ticket.as_deref() {
        if !state
            .tickets
            .consume(ticket, &scope_from(&query.location, &id))
        {
            return forbidden("Invalid or expired PTY ticket");
        }
    }
    let cursor = query
        .cursor
        .as_deref()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value >= -1);
    ws.on_upgrade(move |socket| handle_socket(state, id, cursor, socket))
}

pub async fn handle_socket(
    state: State_,
    id: String,
    cursor: Option<i64>,
    socket: axum::extract::ws::WebSocket,
) {
    use axum::extract::ws::{CloseFrame, Message, Utf8Bytes};

    let mut attachment = match state.registry.attach(&id, cursor) {
        Ok(attachment) => attachment,
        Err(error) => {
            let mut socket = socket;
            let (code, reason) = match error {
                session::Error::NotFound(_) => (4404u16, "session not found"),
                session::Error::Exited(_) => (4404u16, "session exited"),
                _ => (1011u16, "attach failed"),
            };
            let _ = socket
                .send(Message::Close(Some(CloseFrame {
                    code,
                    reason: Utf8Bytes::from(reason),
                })))
                .await;
            return;
        }
    };

    let (mut sink, mut stream) = socket.split();
    for chunk in protocol::chunks(&attachment.replay) {
        if sink
            .send(Message::Text(Utf8Bytes::from(chunk.to_string())))
            .await
            .is_err()
        {
            return;
        }
    }
    let meta = protocol::meta_frame(attachment.cursor);
    if sink.send(Message::Binary(meta.into())).await.is_err() {
        return;
    }
    attachment.activate();

    let mut events = std::mem::replace(
        &mut attachment.events,
        tokio::sync::mpsc::unbounded_channel().1,
    );
    let write_task = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                session::Event::Data(chunk) => {
                    if sink
                        .send(Message::Text(Utf8Bytes::from(chunk)))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                session::Event::End(_) => {
                    let _ = sink
                        .send(Message::Close(Some(CloseFrame {
                            code: 1000,
                            reason: Utf8Bytes::from(""),
                        })))
                        .await;
                    break;
                }
            }
        }
    });

    while let Some(Ok(message)) = stream.next().await {
        match message {
            Message::Text(text) => attachment.write(text.as_str()),
            Message::Binary(bytes) => {
                if let Some(decoded) = protocol::decode_input(&bytes) {
                    attachment.write(&decoded);
                }
            }
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) => {}
        }
    }
    attachment.detach();
    let _ = write_task.await;
}
