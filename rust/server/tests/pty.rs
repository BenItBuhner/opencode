//! Integration tests for the PTY slice: mirror the upstream Bun tests in
//! packages/core/test/pty/ (ticket + pty-session) against the public Rust
//! `pty::Registry` and `pty::TicketRegistry`.

use opencode_server::bus::Bus;
use opencode_server::pty::{self, CreateInput, PtyEvent, UpdateInput};
use std::time::Duration;

fn spawn_cat() -> pty::Registry {
    pty::Registry::new(Bus::new())
}

fn scope(pty: &str) -> pty::Scope {
    pty::Scope {
        pty_id: pty.into(),
        directory: Some("/tmp/a".into()),
        workspace_id: None,
    }
}

#[test]
fn ticket_lifecycle_matches_upstream_semantics() {
    let tickets = pty::TicketRegistry::default();
    let issued = tickets.issue(scope("pty_1"));
    assert!(issued.expires_in >= 1);
    assert!(tickets.consume(&issued.ticket, &scope("pty_1")));
    assert!(!tickets.consume(&issued.ticket, &scope("pty_1")));

    let issued = tickets.issue(scope("pty_1"));
    assert!(!tickets.consume(
        &issued.ticket,
        &pty::Scope {
            directory: Some("/tmp/b".into()),
            ..scope("pty_1")
        },
    ));
    assert!(tickets.consume(&issued.ticket, &scope("pty_1")));
}

#[test]
fn ticket_expires_after_ttl() {
    let tickets = pty::TicketRegistry::new(Duration::from_millis(5));
    let issued = tickets.issue(scope("pty_1"));
    std::thread::sleep(Duration::from_millis(25));
    assert!(!tickets.consume(&issued.ticket, &scope("pty_1")));
}

#[cfg(unix)]
#[test]
fn pty_lifecycle_end_to_end() {
    let registry = spawn_cat();
    let info = registry
        .create(CreateInput {
            command: Some("/usr/bin/env".into()),
            args: Some(vec!["cat".into()]),
            cwd: Some("/tmp".into()),
            title: Some("integ".into()),
            env: None,
        })
        .expect("create");
    assert_eq!(info.title, "integ");
    assert!(info.pid > 0);
    assert_eq!(registry.list().len(), 1);

    // Update title lands and echoes back.
    let updated = registry
        .update(
            &info.id,
            UpdateInput {
                title: Some("renamed".into()),
                size: None,
            },
        )
        .expect("update");
    assert_eq!(updated.title, "renamed");

    // Write then attach: replay must include the echo.
    registry.write(&info.id, "AAA\n").expect("write");
    let mut attach = registry.attach(&info.id, None).expect("attach");
    attach.activate();
    let received = drain_until(&mut attach.events, "AAA");
    assert!(received.contains("AAA"));

    // Detach then verify next attach still sees replay.
    drop(attach);
    let replay = registry.attach(&info.id, None).expect("attach2");
    assert!(replay.replay.contains("AAA"));

    registry.remove(&info.id).expect("remove");
    assert!(registry.list().is_empty());
}

#[cfg(unix)]
#[test]
fn attach_after_exit_is_rejected() {
    let registry = spawn_cat();
    let info = registry
        .create(CreateInput {
            command: Some("/usr/bin/env".into()),
            args: Some(vec!["sh".into(), "-c".into(), "exit 7".into()]),
            cwd: Some("/tmp".into()),
            title: None,
            env: None,
        })
        .expect("create");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if registry.get(&info.id).expect("get").exit_code == Some(7) {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let attach = registry.attach(&info.id, None);
    assert!(matches!(attach, Err(pty::Error::Exited(_))));
}

fn drain_until(rx: &mut tokio::sync::mpsc::UnboundedReceiver<PtyEvent>, needle: &str) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut received = String::new();
    while std::time::Instant::now() < deadline {
        match rx.try_recv() {
            Ok(PtyEvent::Data(chunk)) => {
                received.push_str(&chunk);
                if received.contains(needle) {
                    return received;
                }
            }
            Ok(PtyEvent::End(_)) => return received,
            Err(_) => std::thread::sleep(Duration::from_millis(25)),
        }
    }
    panic!("timeout waiting for {needle:?} (got {received:?})");
}
