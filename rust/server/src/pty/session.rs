//! Port of packages/core/src/pty.ts. Process-local PTY registry with buffered
//! output, cursor-addressed replay, attach/detach subscribers, exit retention
//! (bounded), and lifecycle events published through the SSE bus.
//!
//! The upstream TS runtime is single-threaded (Bun event loop), so the PTY
//! service uses ordinary maps guarded by nothing. In Rust the pty spawns a
//! background reader thread per session and multiple Axum handlers can touch
//! the registry concurrently, so per-session state lives inside a mutex.
//!
//! Linux correctness is required by the task; portable-pty backs the
//! implementation with UnixPtySystem on Linux and macOS. Windows uses ConPTY
//! through the same trait, but this port targets Linux and macOS: the
//! login-shell rules and default-shell fallback intentionally omit Windows
//! branches from packages/core/src/shell.ts.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex, Weak};
use std::thread;

use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;

use crate::bus::Bus;

/// Same 2 MiB retained buffer per session as packages/core/src/pty.ts.
const BUFFER_LIMIT: usize = 2 * 1024 * 1024;

/// Cap on exited sessions retained for observation before oldest gets evicted.
const EXITED_LIMIT: usize = 25;

/// PTY info projected in HTTP responses; mirrors packages/schema/src/pty.ts
/// key order (id, title, command, args, cwd, status, pid, exitCode?).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Info {
    pub id: String,
    pub title: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub status: Status,
    pub pid: u32,
    #[serde(rename = "exitCode", skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Running,
    Exited,
}

#[derive(Debug, Default, Deserialize)]
pub struct CreateInput {
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub cwd: Option<String>,
    pub title: Option<String>,
    pub env: Option<HashMap<String, String>>,
}

#[derive(Debug, Default, Deserialize)]
pub struct UpdateInput {
    pub title: Option<String>,
    pub size: Option<Size>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("PTY session not found: {0}")]
    NotFound(String),
    #[error("PTY session already exited: {0}")]
    Exited(String),
    #[error("failed to spawn pty: {0}")]
    Spawn(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// One attached subscriber. Follows the TS activation protocol:
/// - inactive: chunks accumulate in `pending`; end is stashed in `end`.
/// - active:   chunks and end fire immediately through the tx channel.
/// - detached: subscriber has been dropped by the client; any further offer
///   is a no-op.
struct Subscriber {
    tx: mpsc::UnboundedSender<Event>,
    active: bool,
    detached: bool,
    pending: Vec<String>,
    end: Option<EndEvent>,
}

#[derive(Debug, Clone)]
pub enum Event {
    Data(String),
    End(#[allow(dead_code)] EndEvent),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct EndEvent {
    #[allow(dead_code)]
    pub exit_code: Option<u32>,
}

struct State {
    info: Info,
    buffer: String,
    buffer_cursor: u64,
    cursor: u64,
    subscribers: HashMap<u64, Subscriber>,
    next_subscriber: u64,
}

impl State {
    fn on_data(&mut self, chunk: &str) {
        self.cursor += chunk.len() as u64;
        // Fan out to subscribers, buffering for inactive ones and dropping any
        // whose channel has closed.
        self.subscribers.retain(|_, subscriber| {
            if subscriber.detached {
                return false;
            }
            if !subscriber.active {
                subscriber.pending.push(chunk.to_string());
                return true;
            }
            subscriber.tx.send(Event::Data(chunk.to_string())).is_ok()
        });
        self.buffer.push_str(chunk);
        if self.buffer.len() <= BUFFER_LIMIT {
            return;
        }
        let excess = self.buffer.len() - BUFFER_LIMIT;
        // Advance to a UTF-8 char boundary so the retained buffer stays valid.
        let mut cut = excess;
        while cut < self.buffer.len() && !self.buffer.is_char_boundary(cut) {
            cut += 1;
        }
        self.buffer.drain(..cut);
        self.buffer_cursor += cut as u64;
    }

    fn notify_end(&mut self, event: EndEvent) {
        for subscriber in self.subscribers.values_mut() {
            if !subscriber.active {
                subscriber.end = Some(event);
                continue;
            }
            let _ = subscriber.tx.send(Event::End(event));
        }
        self.subscribers.clear();
    }
}

/// One live PTY session in the registry.
pub struct Session {
    state: Mutex<State>,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    killer: Mutex<Option<Box<dyn ChildKiller + Send + Sync>>>,
}

impl Session {
    fn info(&self) -> Info {
        self.state.lock().unwrap().info.clone()
    }

    fn write(&self, data: &str) {
        let mut guard = self.writer.lock().unwrap();
        if let Some(writer) = guard.as_mut() {
            let _ = writer.write_all(data.as_bytes());
            let _ = writer.flush();
        }
    }

    fn resize(&self, size: Size) {
        let guard = self.master.lock().unwrap();
        let Some(master) = guard.as_ref() else {
            return;
        };
        let _ = master.resize(PtySize {
            cols: size.cols,
            rows: size.rows,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    fn kill(&self) {
        if let Some(mut killer) = self.killer.lock().unwrap().take() {
            let _ = killer.kill();
        }
    }

    fn teardown(&self) {
        // Drop writer + master so background threads unblock; kill the child
        // if it's still running.
        self.writer.lock().unwrap().take();
        self.master.lock().unwrap().take();
        self.kill();
        self.state.lock().unwrap().notify_end(EndEvent::default());
    }
}

/// Handle returned by `Registry::attach`. The client applies `replay`, then
/// calls `activate` to begin receiving live chunks and an `end` event.
pub struct Attachment {
    pub replay: String,
    pub cursor: u64,
    session: Arc<Session>,
    token: u64,
    pub events: mpsc::UnboundedReceiver<Event>,
    activated: bool,
    detached: bool,
}

impl Attachment {
    pub fn write(&self, data: &str) {
        let running = self.session.state.lock().unwrap().info.status == Status::Running;
        if running {
            self.session.write(data);
        }
    }

    pub fn activate(&mut self) {
        if self.activated || self.detached {
            return;
        }
        self.activated = true;
        let mut state = self.session.state.lock().unwrap();
        let Some(subscriber) = state.subscribers.get_mut(&self.token) else {
            return;
        };
        subscriber.active = true;
        let pending = std::mem::take(&mut subscriber.pending);
        let end = subscriber.end.take();
        let tx = subscriber.tx.clone();
        drop(state);
        for chunk in pending {
            if tx.send(Event::Data(chunk)).is_err() {
                self.detached = true;
                return;
            }
        }
        if let Some(end) = end {
            let _ = tx.send(Event::End(end));
        }
    }

    pub fn detach(&mut self) {
        if self.detached {
            return;
        }
        self.detached = true;
        let mut state = self.session.state.lock().unwrap();
        if let Some(subscriber) = state.subscribers.get_mut(&self.token) {
            subscriber.detached = true;
            subscriber.pending.clear();
            subscriber.end = None;
        }
        state.subscribers.remove(&self.token);
    }
}

impl Drop for Attachment {
    fn drop(&mut self) {
        self.detach();
    }
}

/// Process-local PTY registry. `Clone` shares the inner state.
#[derive(Clone)]
pub struct Registry {
    inner: Arc<Inner>,
}

struct Inner {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    exit_order: Mutex<VecDeque<String>>,
    bus: Bus,
}

impl Registry {
    pub fn new(bus: Bus) -> Self {
        Registry {
            inner: Arc::new(Inner {
                sessions: Mutex::new(HashMap::new()),
                exit_order: Mutex::new(VecDeque::new()),
                bus,
            }),
        }
    }

    pub fn list(&self) -> Vec<Info> {
        self.inner
            .sessions
            .lock()
            .unwrap()
            .values()
            .map(|session| session.info())
            .collect()
    }

    pub fn get(&self, id: &str) -> Result<Info, Error> {
        self.require(id).map(|session| session.info())
    }

    pub fn create(&self, input: CreateInput) -> Result<Info, Error> {
        let id = format!("pty_{}", crate::identifier::ascending());
        let command = input.command.clone().unwrap_or_else(preferred_shell);
        let mut args = input.args.clone().unwrap_or_default();
        if is_login_shell(&command) && !args.iter().any(|arg| arg == "-l") {
            args.push("-l".into());
        }
        let cwd = input.cwd.clone().unwrap_or_else(|| {
            std::env::current_dir()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "/".into())
        });
        let title = input
            .title
            .clone()
            .unwrap_or_else(|| format!("Terminal {}", &id[id.len().saturating_sub(4)..]));

        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| Error::Spawn(error.to_string()))?;
        let mut cmd = CommandBuilder::new(&command);
        cmd.args(args.iter());
        cmd.cwd(&cwd);
        // The spawned shell inherits the server's environment plus TERM and
        // OPENCODE_TERMINAL, then any explicitly supplied overrides. This
        // matches the TS create flow: `{ ...process.env, ...input.env, TERM,
        // OPENCODE_TERMINAL }`.
        cmd.env_clear();
        for (key, value) in std::env::vars() {
            cmd.env(key, value);
        }
        if let Some(env) = input.env.as_ref() {
            for (key, value) in env {
                cmd.env(key, value);
            }
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("OPENCODE_TERMINAL", "1");

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|error| Error::Spawn(error.to_string()))?;
        let pid = child.process_id().unwrap_or(0);
        let killer = child.clone_killer();
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| Error::Spawn(error.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| Error::Spawn(error.to_string()))?;
        // Drop the slave now so EOF propagates when the child exits.
        drop(pair.slave);

        let info = Info {
            id: id.clone(),
            title,
            command,
            args,
            cwd,
            status: Status::Running,
            pid,
            exit_code: None,
        };
        let session = Arc::new(Session {
            state: Mutex::new(State {
                info: info.clone(),
                buffer: String::new(),
                buffer_cursor: 0,
                cursor: 0,
                subscribers: HashMap::new(),
                next_subscriber: 0,
            }),
            writer: Mutex::new(Some(writer)),
            master: Mutex::new(Some(pair.master)),
            killer: Mutex::new(Some(killer)),
        });

        self.inner
            .sessions
            .lock()
            .unwrap()
            .insert(id.clone(), Arc::clone(&session));

        // Reader thread streams master output into the session buffer. When
        // the pipe closes the loop drops out and the join call in the waiter
        // thread will observe the child exit.
        let weak = Arc::downgrade(&session);
        thread::Builder::new()
            .name(format!("pty-reader-{id}"))
            .spawn(move || reader_loop(weak, reader))
            .map_err(|error| Error::Spawn(error.to_string()))?;

        // Waiter thread turns the blocking child.wait into an exit
        // notification. The registry publishes pty.exited and evicts oldest
        // exited sessions if the retention cap is exceeded.
        let registry = self.clone();
        let waiter_id = id.clone();
        thread::Builder::new()
            .name(format!("pty-waiter-{id}"))
            .spawn(move || {
                let exit_code = child.wait().map(|status| status.exit_code()).unwrap_or(0);
                registry.on_exit(&waiter_id, exit_code);
            })
            .map_err(|error| Error::Spawn(error.to_string()))?;

        self.inner
            .bus
            .publish("pty.created", json!({ "info": info.clone() }));
        Ok(info)
    }

    pub fn update(&self, id: &str, input: UpdateInput) -> Result<Info, Error> {
        let session = self.require(id)?;
        {
            let mut state = session.state.lock().unwrap();
            if let Some(title) = input.title {
                state.info.title = title;
            }
            if let Some(size) = input.size {
                if state.info.status == Status::Running {
                    drop(state);
                    session.resize(size);
                    state = session.state.lock().unwrap();
                }
                let _ = size;
            }
            let info = state.info.clone();
            drop(state);
            self.inner
                .bus
                .publish("pty.updated", json!({ "info": info.clone() }));
            Ok(info)
        }
    }

    pub fn remove(&self, id: &str) -> Result<(), Error> {
        let session = self.require(id)?;
        self.evict(id, &session);
        Ok(())
    }

    #[allow(dead_code)]
    pub fn write(&self, id: &str, data: &str) -> Result<(), Error> {
        let session = self.require(id)?;
        if session.state.lock().unwrap().info.status == Status::Running {
            session.write(data);
        }
        Ok(())
    }

    /// Attach a subscriber. The returned attachment carries the replay slice
    /// captured atomically with the subscriber insertion, so no chunks are
    /// lost or duplicated between replay and live delivery.
    pub fn attach(&self, id: &str, cursor: Option<i64>) -> Result<Attachment, Error> {
        let session = self.require(id)?;
        let mut state = session.state.lock().unwrap();
        if state.info.status != Status::Running {
            return Err(Error::Exited(id.into()));
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let token = state.next_subscriber;
        state.next_subscriber += 1;
        state.subscribers.insert(
            token,
            Subscriber {
                tx,
                active: false,
                detached: false,
                pending: Vec::new(),
                end: None,
            },
        );
        let start = state.buffer_cursor;
        let end = state.cursor;
        // cursor == -1 tails from the current end; None replays the full
        // retained buffer; other integers clamp to [start, end).
        let from = match cursor {
            Some(-1) => end,
            Some(value) if value >= 0 => value as u64,
            _ => 0,
        };
        let replay = if state.buffer.is_empty() || from >= end {
            String::new()
        } else {
            let offset = (from.saturating_sub(start)) as usize;
            if offset >= state.buffer.len() {
                String::new()
            } else {
                let mut cut = offset;
                while cut < state.buffer.len() && !state.buffer.is_char_boundary(cut) {
                    cut += 1;
                }
                state.buffer[cut..].to_string()
            }
        };
        drop(state);
        Ok(Attachment {
            replay,
            cursor: end,
            session,
            token,
            events: rx,
            activated: false,
            detached: false,
        })
    }

    fn require(&self, id: &str) -> Result<Arc<Session>, Error> {
        self.inner
            .sessions
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| Error::NotFound(id.into()))
    }

    fn on_exit(&self, id: &str, exit_code: u32) {
        let Some(session) = self.inner.sessions.lock().unwrap().get(id).cloned() else {
            return;
        };
        {
            let mut state = session.state.lock().unwrap();
            if state.info.status == Status::Exited {
                return;
            }
            state.info.status = Status::Exited;
            state.info.exit_code = Some(exit_code);
            state.notify_end(EndEvent {
                exit_code: Some(exit_code),
            });
        }
        self.inner.exit_order.lock().unwrap().push_back(id.into());
        self.inner
            .bus
            .publish("pty.exited", json!({ "id": id, "exitCode": exit_code }));
        // Enforce the retention cap. Once the cap is exceeded, evict oldest
        // exited sessions one at a time until it fits.
        loop {
            let oldest = {
                let mut order = self.inner.exit_order.lock().unwrap();
                if order.len() <= EXITED_LIMIT {
                    break;
                }
                order.pop_front()
            };
            let Some(oldest) = oldest else { break };
            if let Some(session) = self.inner.sessions.lock().unwrap().get(&oldest).cloned() {
                self.evict(&oldest, &session);
            }
        }
    }

    fn evict(&self, id: &str, session: &Arc<Session>) {
        self.inner.sessions.lock().unwrap().remove(id);
        {
            let mut order = self.inner.exit_order.lock().unwrap();
            if let Some(index) = order.iter().position(|entry| entry == id) {
                order.remove(index);
            }
        }
        session.teardown();
        self.inner.bus.publish("pty.deleted", json!({ "id": id }));
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let sessions = std::mem::take(&mut *self.sessions.lock().unwrap());
        for session in sessions.into_values() {
            session.teardown();
        }
    }
}

fn reader_loop(session: Weak<Session>, mut reader: Box<dyn Read + Send>) {
    let mut buf = [0u8; 8 * 1024];
    // Some terminal writes are byte-precise but not code-point aligned, so
    // hold pending bytes that end mid-UTF-8 sequence and flush them once
    // completion arrives.
    let mut carry: Vec<u8> = Vec::new();
    loop {
        let read = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        carry.extend_from_slice(&buf[..read]);
        let (text, remainder) = split_valid_utf8(&carry);
        let text = text.to_string();
        carry = remainder.to_vec();
        if text.is_empty() {
            continue;
        }
        let Some(session) = session.upgrade() else {
            break;
        };
        session.state.lock().unwrap().on_data(&text);
    }
}

fn split_valid_utf8(bytes: &[u8]) -> (&str, &[u8]) {
    match std::str::from_utf8(bytes) {
        Ok(text) => (text, &[]),
        Err(error) => {
            let valid_up_to = error.valid_up_to();
            let text = unsafe { std::str::from_utf8_unchecked(&bytes[..valid_up_to]) };
            match error.error_len() {
                Some(_) => {
                    // Invalid sequence detected; drop it to avoid stalling
                    // downstream text delivery, and continue from the byte
                    // after the invalid sequence.
                    let skip = valid_up_to + error.error_len().unwrap();
                    (text, &bytes[skip..])
                }
                None => (text, &bytes[valid_up_to..]),
            }
        }
    }
}

fn preferred_shell() -> String {
    if let Ok(shell) = std::env::var("SHELL") {
        if !shell.trim().is_empty() {
            return shell;
        }
    }
    if cfg!(target_os = "macos") {
        return "/bin/zsh".into();
    }
    if std::path::Path::new("/bin/bash").exists() {
        return "/bin/bash".into();
    }
    "/bin/sh".into()
}

fn is_login_shell(file: &str) -> bool {
    let name = std::path::Path::new(file)
        .file_name()
        .map(|value| value.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    matches!(
        name.as_str(),
        "bash" | "dash" | "fish" | "ksh" | "sh" | "zsh"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> Registry {
        Registry::new(Bus::new())
    }

    #[test]
    fn not_found_errors_for_missing_sessions() {
        let registry = registry();
        assert!(matches!(
            registry.get("pty_missing"),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            registry.update("pty_missing", UpdateInput::default()),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            registry.remove("pty_missing"),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            registry.write("pty_missing", "x"),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            registry.attach("pty_missing", None),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    #[cfg(unix)]
    fn creates_and_lists_running_session() {
        let registry = registry();
        let info = registry
            .create(CreateInput {
                command: Some("/usr/bin/env".into()),
                args: Some(vec!["cat".into()]),
                cwd: Some("/tmp".into()),
                title: Some("t".into()),
                env: None,
            })
            .unwrap();
        assert_eq!(info.status, Status::Running);
        assert!(info.pid > 0);
        assert_eq!(info.title, "t");
        assert!(info.id.starts_with("pty_"));
        assert_eq!(registry.list().len(), 1);
        assert_eq!(registry.get(&info.id).unwrap().id, info.id);
        registry.remove(&info.id).unwrap();
        assert!(registry.list().is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn retains_exited_sessions_until_removed() {
        let registry = registry();
        let info = registry
            .create(CreateInput {
                command: Some("/usr/bin/env".into()),
                args: Some(vec!["sh".into(), "-c".into(), "exit 3".into()]),
                cwd: Some("/tmp".into()),
                title: None,
                env: None,
            })
            .unwrap();
        let exited = wait_for_exit(&registry, &info.id, 5);
        assert_eq!(exited.status, Status::Exited);
        assert_eq!(exited.exit_code, Some(3));
        registry.remove(&info.id).unwrap();
        assert!(matches!(registry.get(&info.id), Err(Error::NotFound(_))));
    }

    #[test]
    #[cfg(unix)]
    fn replays_buffered_output_and_streams_live_output() {
        let registry = registry();
        let info = registry
            .create(CreateInput {
                command: Some("/usr/bin/env".into()),
                args: Some(vec!["cat".into()]),
                cwd: Some("/tmp".into()),
                ..Default::default()
            })
            .unwrap();
        registry.write(&info.id, "AAA\n").unwrap();
        // Give the reader thread a beat to buffer the echo before attaching so
        // the replay path is exercised deterministically instead of racing the
        // pty read loop.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let first = read_until(&registry, &info.id, None, "AAA");
        assert!(first.contains("AAA"));

        // Write via the attachment.
        let mut second = registry.attach(&info.id, None).unwrap();
        second.activate();
        registry.write(&info.id, "BBB\n").unwrap();
        let combined = drain_until(&mut second.events, "BBB");
        assert!(combined.contains("BBB"));
        // A late attachment replays everything already buffered.
        let replay = registry.attach(&info.id, None).unwrap();
        assert!(replay.replay.contains("AAA"));
        assert!(replay.replay.contains("BBB"));
        assert!(replay.cursor > 0);
        // Tail attachments skip the buffer and only see subsequent output.
        let tail = registry.attach(&info.id, Some(-1)).unwrap();
        assert_eq!(tail.replay, "");
        assert_eq!(tail.cursor, replay.cursor);
        drop(second);
        registry.remove(&info.id).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn stops_delivering_output_after_detach() {
        let registry = registry();
        let info = registry
            .create(CreateInput {
                command: Some("/usr/bin/env".into()),
                args: Some(vec!["cat".into()]),
                cwd: Some("/tmp".into()),
                ..Default::default()
            })
            .unwrap();
        let mut attached = registry.attach(&info.id, Some(-1)).unwrap();
        attached.activate();
        attached.detach();
        registry.write(&info.id, "AAA\n").unwrap();
        // Give the reader thread a beat to drain the write; then confirm the
        // detached attachment received nothing.
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(attached.events.try_recv().is_err());
        registry.remove(&info.id).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn isolates_output_between_sessions() {
        let registry = registry();
        let a = registry
            .create(CreateInput {
                command: Some("/usr/bin/env".into()),
                args: Some(vec!["cat".into()]),
                cwd: Some("/tmp".into()),
                ..Default::default()
            })
            .unwrap();
        let b = registry
            .create(CreateInput {
                command: Some("/usr/bin/env".into()),
                args: Some(vec!["cat".into()]),
                cwd: Some("/tmp".into()),
                ..Default::default()
            })
            .unwrap();
        let mut attached_a = registry.attach(&a.id, None).unwrap();
        attached_a.activate();
        let mut attached_b = registry.attach(&b.id, None).unwrap();
        attached_b.activate();
        registry.write(&a.id, "AAA\n").unwrap();
        let got_a = drain_until(&mut attached_a.events, "AAA");
        assert!(got_a.contains("AAA"));
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(attached_b.events.try_recv().is_err());
        registry.remove(&a.id).unwrap();
        registry.remove(&b.id).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn notifies_attachments_with_exit_code_and_rejects_reattach() {
        let registry = registry();
        let info = registry
            .create(CreateInput {
                command: Some("/usr/bin/env".into()),
                args: Some(vec!["cat".into()]),
                cwd: Some("/tmp".into()),
                ..Default::default()
            })
            .unwrap();
        let mut attached = registry.attach(&info.id, None).unwrap();
        attached.activate();
        registry.write(&info.id, "\u{4}").unwrap();
        let event = wait_for_end(&mut attached.events, 5);
        assert_eq!(event.exit_code, Some(0));
        assert!(matches!(
            registry.attach(&info.id, None),
            Err(Error::Exited(_))
        ));
    }

    fn wait_for_exit(registry: &Registry, id: &str, seconds: u64) -> Info {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
        loop {
            let info = registry.get(id).unwrap();
            if info.status == Status::Exited {
                return info;
            }
            if std::time::Instant::now() >= deadline {
                panic!("timeout waiting for pty exit");
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    fn read_until(registry: &Registry, id: &str, cursor: Option<i64>, needle: &str) -> String {
        let mut attachment = registry.attach(id, cursor).unwrap();
        let mut received = attachment.replay.clone();
        attachment.activate();
        if received.contains(needle) {
            return received;
        }
        received.push_str(&drain_until(&mut attachment.events, needle));
        received
    }

    fn drain_until(rx: &mut mpsc::UnboundedReceiver<Event>, needle: &str) -> String {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut received = String::new();
        while std::time::Instant::now() < deadline {
            match rx.try_recv() {
                Ok(Event::Data(chunk)) => {
                    received.push_str(&chunk);
                    if received.contains(needle) {
                        return received;
                    }
                }
                Ok(Event::End(_)) => return received,
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(25)),
            }
        }
        panic!("timeout waiting for output containing {needle:?} (got {received:?})");
    }

    fn wait_for_end(rx: &mut mpsc::UnboundedReceiver<Event>, seconds: u64) -> EndEvent {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
        while std::time::Instant::now() < deadline {
            match rx.try_recv() {
                Ok(Event::End(event)) => return event,
                Ok(Event::Data(_)) => continue,
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(25)),
            }
        }
        panic!("timeout waiting for end event");
    }
}
