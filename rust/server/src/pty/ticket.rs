//! Port of packages/core/src/pty/ticket.ts.
//!
//! Single-use scoped expiring connect tickets. A ticket is minted for a
//! `(ptyID, directory?, workspaceID?)` scope and consumed exactly once by the
//! WebSocket connect route, only if the scope on consume matches the scope on
//! issue. Tickets expire after their TTL (60 seconds by default).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use uuid::Uuid;

/// Bun cache capacity in the TS port: refuses to grow past this.
const CAPACITY: usize = 10_000;
/// Bun default TTL is 60 seconds.
pub const DEFAULT_TTL: Duration = Duration::from_secs(60);

/// Scope over which a ticket is valid. Directory and workspace default to
/// `None`, matching the upstream Option treatment when not supplied.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Scope {
    pub pty_id: String,
    pub directory: Option<String>,
    pub workspace_id: Option<String>,
}

/// Response mirror of packages/schema/src/pty-ticket.ts::ConnectToken:
/// `{ ticket: string, expires_in: PositiveInt }`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConnectToken {
    pub ticket: String,
    pub expires_in: u64,
}

struct Entry {
    scope: Scope,
    expires_at: Instant,
}

/// Process-local ticket registry. Cloning shares state with all clones so it
/// can be handed to Axum via `State`.
#[derive(Clone)]
pub struct Registry {
    inner: std::sync::Arc<Mutex<HashMap<String, Entry>>>,
    ttl: Duration,
}

impl Registry {
    pub fn new(ttl: Duration) -> Self {
        Registry {
            inner: std::sync::Arc::new(Mutex::new(HashMap::with_capacity(64))),
            ttl,
        }
    }

    /// Issue a fresh single-use ticket for the given scope.
    pub fn issue(&self, scope: Scope) -> ConnectToken {
        let ticket = format!("tkt_{}", Uuid::new_v4());
        let expires_at = Instant::now() + self.ttl;
        let mut guard = self.inner.lock().expect("pty ticket registry poisoned");
        prune(&mut guard);
        if guard.len() >= CAPACITY {
            // Evict the oldest entry to keep the cache bounded, matching the
            // Bun Cache eviction behavior at capacity.
            if let Some(oldest) = guard
                .iter()
                .min_by_key(|(_, entry)| entry.expires_at)
                .map(|(key, _)| key.clone())
            {
                guard.remove(&oldest);
            }
        }
        guard.insert(ticket.clone(), Entry { scope, expires_at });
        ConnectToken {
            ticket,
            expires_in: self.ttl.as_secs().max(1),
        }
    }

    /// Consume a ticket exactly once when the presented scope matches the
    /// scope it was issued under and the TTL has not yet elapsed. Returns
    /// `true` when a live ticket was removed.
    pub fn consume(&self, ticket: &str, scope: &Scope) -> bool {
        let mut guard = self.inner.lock().expect("pty ticket registry poisoned");
        prune(&mut guard);
        let Some(entry) = guard.get(ticket) else {
            return false;
        };
        if &entry.scope != scope {
            return false;
        }
        guard.remove(ticket).is_some()
    }

    /// Testing hook: how many live entries the registry currently holds.
    #[cfg(test)]
    fn len(&self) -> usize {
        let mut guard = self.inner.lock().unwrap();
        prune(&mut guard);
        guard.len()
    }
}

impl Default for Registry {
    fn default() -> Self {
        Registry::new(DEFAULT_TTL)
    }
}

fn prune(map: &mut HashMap<String, Entry>) {
    let now = Instant::now();
    map.retain(|_, entry| entry.expires_at > now);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    fn scope(pty: &str) -> Scope {
        Scope {
            pty_id: pty.into(),
            directory: Some("/tmp/a".into()),
            workspace_id: None,
        }
    }

    #[test]
    fn consumes_tickets_once() {
        let registry = Registry::default();
        let ticket = registry.issue(scope("pty_1"));
        assert!(registry.consume(&ticket.ticket, &scope("pty_1")));
        assert!(!registry.consume(&ticket.ticket, &scope("pty_1")));
    }

    #[test]
    fn rejects_tickets_scoped_to_a_different_directory() {
        let registry = Registry::default();
        let ticket = registry.issue(scope("pty_1"));
        let other = Scope {
            directory: Some("/tmp/b".into()),
            ..scope("pty_1")
        };
        assert!(!registry.consume(&ticket.ticket, &other));
        // Original scope still consumes.
        assert!(registry.consume(&ticket.ticket, &scope("pty_1")));
    }

    #[test]
    fn rejects_tickets_scoped_to_a_different_workspace() {
        let registry = Registry::default();
        let issued = registry.issue(Scope {
            pty_id: "pty_1".into(),
            directory: None,
            workspace_id: Some("ws_a".into()),
        });
        assert!(!registry.consume(
            &issued.ticket,
            &Scope {
                pty_id: "pty_1".into(),
                directory: None,
                workspace_id: Some("ws_b".into()),
            },
        ));
        assert!(registry.consume(
            &issued.ticket,
            &Scope {
                pty_id: "pty_1".into(),
                directory: None,
                workspace_id: Some("ws_a".into()),
            },
        ));
    }

    #[test]
    fn rejects_tickets_scoped_to_a_different_pty() {
        let registry = Registry::default();
        let issued = registry.issue(scope("pty_1"));
        assert!(!registry.consume(&issued.ticket, &scope("pty_2")));
    }

    #[test]
    fn rejects_tickets_after_the_ttl_elapses() {
        let registry = Registry::new(Duration::from_millis(5));
        let issued = registry.issue(scope("pty_1"));
        sleep(Duration::from_millis(25));
        assert!(!registry.consume(&issued.ticket, &scope("pty_1")));
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn expires_in_is_reported_in_seconds() {
        let registry = Registry::new(Duration::from_millis(500));
        // Sub-second TTLs round up to 1 like the TS port.
        assert_eq!(registry.issue(scope("pty_1")).expires_in, 1);
        let registry = Registry::new(Duration::from_secs(60));
        assert_eq!(registry.issue(scope("pty_2")).expires_in, 60);
    }

    #[test]
    fn tickets_start_with_the_tkt_prefix() {
        let registry = Registry::default();
        let issued = registry.issue(scope("pty_1"));
        assert!(issued.ticket.starts_with("tkt_"));
    }
}
