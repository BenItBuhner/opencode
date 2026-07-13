//! End-to-end test for the PTY WebSocket route. Boots the standalone
//! `pty::router` on a random loopback port, opens a real WebSocket, and
//! verifies replay + cursor control frame + live input path against the
//! wire protocol from packages/core/src/pty/protocol.ts.

#![cfg(unix)]

use futures::{SinkExt, StreamExt};
use opencode_server::bus::Bus;
use opencode_server::pty::{self, CreateInput};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message;

async fn bind_router(
    registry: pty::Registry,
    tickets: pty::TicketRegistry,
) -> (u16, tokio::task::JoinHandle<()>) {
    let router = pty::router::router(pty::router::State_ { registry, tickets });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind");
    let port = listener.local_addr().unwrap().port();
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (port, handle)
}

fn create_cat(registry: &pty::Registry) -> pty::session::Info {
    registry
        .create(CreateInput {
            command: Some("/usr/bin/env".into()),
            args: Some(vec!["cat".into()]),
            cwd: Some("/tmp".into()),
            title: None,
            env: None,
        })
        .expect("create")
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_replay_meta_and_live_delivery() {
    let registry = pty::Registry::new(Bus::new());
    let tickets = pty::TicketRegistry::default();
    let (port, _server) = bind_router(registry.clone(), tickets.clone()).await;

    let info = create_cat(&registry);
    registry.write(&info.id, "AAA\n").expect("write");
    // Give the reader thread a moment to buffer the echo.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let ticket = tickets.issue(pty::Scope {
        pty_id: info.id.clone(),
        directory: None,
        workspace_id: None,
    });
    let url = format!(
        "ws://127.0.0.1:{port}/api/pty/{id}/connect?ticket={ticket}",
        id = info.id,
        ticket = ticket.ticket,
    );
    let request = url.into_client_request().unwrap();
    let (mut socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("ws connect");

    let mut received = String::new();
    let mut cursor = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("timeout")
            .expect("stream ended")
            .expect("frame");
        match message {
            Message::Text(text) => received.push_str(&text),
            Message::Binary(bytes) => {
                assert_eq!(bytes[0], 0, "control frame is 0x00-prefixed");
                let payload: serde_json::Value =
                    serde_json::from_slice(&bytes[1..]).expect("cursor JSON");
                cursor = payload.get("cursor").and_then(|value| value.as_u64());
                break;
            }
            Message::Close(_) => panic!("closed before cursor frame"),
            _ => {}
        }
    }
    assert!(
        received.contains("AAA"),
        "replay contained AAA (got {received:?})"
    );
    assert!(cursor.unwrap() > 0, "cursor > 0 after replay");

    // Live delivery: send input, expect the echo back through the socket.
    socket
        .send(Message::Text("BBB\n".into()))
        .await
        .expect("send");
    let mut live = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline && !live.contains("BBB") {
        let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("timeout")
            .expect("stream ended")
            .expect("frame");
        if let Message::Text(text) = message {
            live.push_str(&text);
        }
    }
    assert!(live.contains("BBB"), "live echo delivered (got {live:?})");

    let _ = socket.close(None).await;
    registry.remove(&info.id).ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_rejects_bad_ticket() {
    let registry = pty::Registry::new(Bus::new());
    let tickets = pty::TicketRegistry::default();
    let (port, _server) = bind_router(registry.clone(), tickets.clone()).await;
    let info = create_cat(&registry);

    let url = format!(
        "ws://127.0.0.1:{port}/api/pty/{id}/connect?ticket=nope",
        id = info.id,
    );
    let request = url.into_client_request().unwrap();
    let result = tokio_tungstenite::connect_async(request).await;
    assert!(result.is_err(), "invalid ticket must fail the handshake");

    registry.remove(&info.id).ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_tail_cursor_negative_one_skips_replay() {
    let registry = pty::Registry::new(Bus::new());
    let tickets = pty::TicketRegistry::default();
    let (port, _server) = bind_router(registry.clone(), tickets.clone()).await;
    let info = create_cat(&registry);
    registry.write(&info.id, "OLD\n").expect("write");
    tokio::time::sleep(Duration::from_millis(200)).await;

    let ticket = tickets.issue(pty::Scope {
        pty_id: info.id.clone(),
        directory: None,
        workspace_id: None,
    });
    let url = format!(
        "ws://127.0.0.1:{port}/api/pty/{id}/connect?ticket={ticket}&cursor=-1",
        id = info.id,
        ticket = ticket.ticket,
    );
    let request = url.into_client_request().unwrap();
    let (mut socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("ws connect");
    // First frame must be the cursor control frame with no prior text.
    let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .expect("timeout")
        .expect("stream ended")
        .expect("frame");
    match message {
        Message::Binary(bytes) => assert_eq!(bytes[0], 0),
        other => panic!("expected binary cursor frame first, got {other:?}"),
    }
    let _ = socket.close(None).await;
    registry.remove(&info.id).ok();
}
