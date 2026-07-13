//! File-based server discovery matching `packages/cli/src/services/daemon.ts`.
//!
//! State lives under `$XDG_STATE_HOME/opencode/` (typically
//! `~/.local/state/opencode/`):
//!
//! - `server.json` — `{ id, version, url, pid }` registration
//! - `password` — auto-generated Basic auth secret (mode 0600)

use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const USERNAME: &str = "opencode";
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
const START_POLL: Duration = Duration::from_millis(50);
const START_ATTEMPTS: u32 = 100;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Registration {
    pub id: Option<String>,
    pub version: Option<String>,
    pub url: String,
    pub pid: u32,
}

pub struct Transport {
    pub url: String,
    pub username: String,
    pub password: String,
}

impl Transport {
    pub fn authorization(&self) -> String {
        auth::basic_header(&self.username, &self.password)
    }
}

pub fn state_dir() -> PathBuf {
    if let Ok(home) = std::env::var("OPENCODE_TEST_HOME") {
        return PathBuf::from(home).join(".local/state/opencode");
    }
    let base = std::env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .expect("HOME or XDG_STATE_HOME must be set");
    base.join("opencode")
}

pub fn server_file() -> PathBuf {
    state_dir().join("server.json")
}

pub fn password_file() -> PathBuf {
    state_dir().join("password")
}

pub fn load_or_create_password(value: Option<&str>) -> Result<String, String> {
    auth::load_or_create_password(value)
}

pub fn read_registration() -> Result<Registration, String> {
    let text = fs::read_to_string(server_file()).map_err(|error| error.to_string())?;
    serde_json::from_str(&text).map_err(|error| error.to_string())
}

pub fn healthy() -> Result<Registration, String> {
    let info = read_registration()?;
    let password = load_or_create_password(None)?;
    if !health::is_healthy(&info.url, &password) {
        return Err("registered server is not healthy".into());
    }
    Ok(info)
}

pub fn compatible() -> Result<Registration, String> {
    let info = healthy()?;
    if info.version.as_deref() != Some(VERSION) {
        return Err("registered server version does not match the client".into());
    }
    Ok(info)
}

pub fn transport() -> Result<Transport, String> {
    let url = start()?;
    let password = load_or_create_password(None)?;
    Ok(Transport {
        url,
        username: USERNAME.into(),
        password,
    })
}

pub fn status() -> Result<Option<String>, String> {
    match compatible() {
        Ok(info) => Ok(Some(info.url)),
        Err(_) => {
            let _ = fs::remove_file(server_file());
            Ok(None)
        }
    }
}

pub fn start() -> Result<String, String> {
    if let Ok(found) = healthy() {
        if found.version.as_deref() == Some(VERSION) {
            return Ok(found.url);
        }
        stop_process(&found)?;
    }

    spawn_server(true)?;
    let started = Instant::now();
    for _ in 0..START_ATTEMPTS {
        if let Ok(info) = compatible() {
            return Ok(info.url);
        }
        if started.elapsed() > Duration::from_secs(5) {
            break;
        }
        thread::sleep(START_POLL);
    }
    Err("failed to start server".into())
}

pub fn stop() -> Result<(), String> {
    if let Ok(info) = healthy() {
        stop_process(&info)?;
    }
    let _ = fs::remove_file(server_file());
    Ok(())
}

pub fn register(url: &str, id: &str) -> Result<(), String> {
    let directory = state_dir();
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let registration = Registration {
        id: Some(id.to_string()),
        version: Some(VERSION.into()),
        url: url.to_string(),
        pid: std::process::id(),
    };
    let temp = server_file().with_extension(format!("{id}.tmp"));
    write_atomic(&temp, &server_file(), &registration)?;
    Ok(())
}

pub fn unregister(id: &str) {
    if let Ok(info) = read_registration() {
        if info.id.as_deref() == Some(id) {
            let _ = fs::remove_file(server_file());
        }
    }
}

pub fn registration_owner(id: &str) -> bool {
    read_registration()
        .ok()
        .and_then(|info| info.id)
        .is_some_and(|current| current == id)
}

pub fn spawn_server(register: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut command = Command::new(exe);
    command.arg("serve");
    if register {
        command.arg("--register");
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach(&mut command)?;
    command.spawn().map_err(|error| error.to_string())?;
    Ok(())
}

fn stop_process(info: &Registration) -> Result<(), String> {
    let current = healthy().ok();
    if current.as_ref() != Some(info) {
        return Ok(());
    }
    signal(info.pid, signal_term());
    if wait_stopped(info.pid, 100) {
        return Ok(());
    }
    let latest = healthy().ok();
    if latest.as_ref() == Some(info) {
        signal(info.pid, signal_kill());
        wait_stopped(info.pid, 100);
    }
    Ok(())
}

#[cfg(unix)]
fn signal_term() -> i32 {
    libc::SIGTERM
}

#[cfg(unix)]
fn signal_kill() -> i32 {
    libc::SIGKILL
}

#[cfg(not(unix))]
fn signal_term() -> i32 {
    15
}

#[cfg(not(unix))]
fn signal_kill() -> i32 {
    9
}

fn signal(pid: u32, sig: i32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, sig);
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, sig);
    }
}

fn wait_stopped(pid: u32, attempts: u32) -> bool {
    for _ in 0..attempts {
        if !process_running(pid) {
            return true;
        }
        thread::sleep(START_POLL);
    }
    !process_running(pid)
}

fn process_running(pid: u32) -> bool {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, 0) == 0
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn write_atomic(temp: &Path, target: &Path, registration: &Registration) -> Result<(), String> {
    let body = serde_json::to_string(registration).map_err(|error| error.to_string())?;
    fs::write(temp, body).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(temp, fs::Permissions::from_mode(0o600))
            .map_err(|error| error.to_string())?;
    }
    fs::rename(temp, target).map_err(|error| error.to_string())
}

#[cfg(unix)]
fn detach(command: &mut Command) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn detach(_command: &mut Command) -> Result<(), String> {
    Ok(())
}

mod auth {
    use super::*;
    use base64::Engine;

    pub fn basic_header(username: &str, password: &str) -> String {
        let token =
            base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
        format!("Basic {token}")
    }

    pub fn load_or_create_password(value: Option<&str>) -> Result<String, String> {
        let file = password_file();
        if value.is_none() {
            if let Ok(existing) = fs::read_to_string(&file) {
                let trimmed = existing.trim();
                if !trimmed.is_empty() {
                    return Ok(trimmed.to_string());
                }
            }
        }
        let generated = value.map(str::to_string).unwrap_or_else(generate_password);
        fs::create_dir_all(state_dir()).map_err(|error| error.to_string())?;
        let temp = file.with_extension("tmp");
        {
            let mut handle = fs::File::create(&temp).map_err(|error| error.to_string())?;
            handle
                .write_all(generated.as_bytes())
                .map_err(|error| error.to_string())?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))
                .map_err(|error| error.to_string())?;
        }
        fs::rename(&temp, &file).map_err(|error| error.to_string())?;
        Ok(generated)
    }

    fn generate_password() -> String {
        let mut bytes = [0_u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }
}

mod health {
    use super::*;

    pub fn is_healthy(url: &str, password: &str) -> bool {
        let response = ureq::get(&format!("{url}/api/health"))
            .timeout(HEALTH_TIMEOUT)
            .set(
                "Authorization",
                &auth::basic_header(super::USERNAME, password),
            )
            .call();
        match response {
            Ok(response) => {
                let body = response.into_string().unwrap_or_default();
                serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|value| value.get("healthy").and_then(|healthy| healthy.as_bool()))
                    .unwrap_or(false)
            }
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_dir_honors_test_home() {
        std::env::set_var("OPENCODE_TEST_HOME", "/tmp/opengoal-test-home");
        assert_eq!(
            state_dir(),
            PathBuf::from("/tmp/opengoal-test-home/.local/state/opencode")
        );
        std::env::remove_var("OPENCODE_TEST_HOME");
    }
}
