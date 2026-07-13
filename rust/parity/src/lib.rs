#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use serde_json::{json, Value};
    use std::collections::BTreeSet;
    use std::fs;
    use std::io::{BufRead, BufReader};
    use std::process::Command;
    use std::sync::mpsc;
    use std::sync::Mutex;
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    const DEFAULT_BUN: &str = "http://127.0.0.1:4096";
    const DEFAULT_RUST: &str = "http://127.0.0.1:4097";
    const DEFAULT_WORKSPACE: &str = "/workspace";
    const DEFAULT_DB: &str = "/home/ubuntu/.local/share/opencode/opencode-local.db";

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct HttpResponse {
        status: u16,
        body: String,
        next_cursor: Option<String>,
    }

    impl HttpResponse {
        fn json(&self) -> Value {
            serde_json::from_str(&self.body).unwrap_or_else(|error| {
                panic!("failed to parse response JSON: {error}\n{}", self.body)
            })
        }
    }

    #[derive(Clone)]
    struct Client {
        label: &'static str,
        base: String,
    }

    impl Client {
        fn get(&self, path: &str) -> Result<HttpResponse, String> {
            self.request("GET", path, None, Duration::from_secs(30))
        }

        fn post(&self, path: &str, payload: Option<&Value>) -> Result<HttpResponse, String> {
            self.request("POST", path, payload, Duration::from_secs(30))
        }

        fn post_slow(&self, path: &str, payload: Option<&Value>) -> Result<HttpResponse, String> {
            self.request("POST", path, payload, Duration::from_secs(180))
        }

        fn patch(&self, path: &str, payload: &Value) -> Result<HttpResponse, String> {
            self.request("PATCH", path, Some(payload), Duration::from_secs(30))
        }

        fn delete(&self, path: &str) -> Result<HttpResponse, String> {
            self.request("DELETE", path, None, Duration::from_secs(30))
        }

        fn request(
            &self,
            method: &str,
            path: &str,
            payload: Option<&Value>,
            timeout: Duration,
        ) -> Result<HttpResponse, String> {
            let url = format!("{}{}", self.base, path);
            let request = ureq::request(method, &url)
                .set("Content-Type", "application/json")
                .timeout(timeout);
            let result = match payload {
                Some(value) => request.send_string(&value.to_string()),
                None => request.call(),
            };
            match result {
                Ok(response) => Ok(read_response(response)),
                Err(ureq::Error::Status(_, response)) => Ok(read_response(response)),
                Err(ureq::Error::Transport(error)) => {
                    Err(format!("{} {method} {url}: {error}", self.label))
                }
            }
        }
    }

    fn read_response(response: ureq::Response) -> HttpResponse {
        let status = response.status();
        let next_cursor = response.header("X-Next-Cursor").map(str::to_owned);
        let body = response
            .into_string()
            .unwrap_or_else(|error| panic!("failed to read response body: {error}"));
        HttpResponse {
            status,
            body,
            next_cursor,
        }
    }

    struct Servers {
        bun: Client,
        rust: Client,
    }

    impl Servers {
        fn from_env() -> Self {
            Self {
                bun: Client {
                    label: "bun",
                    base: std::env::var("OPENCODE_PARITY_BUN_URL")
                        .unwrap_or_else(|_| DEFAULT_BUN.to_string()),
                },
                rust: Client {
                    label: "rust",
                    base: std::env::var("OPENCODE_PARITY_RUST_URL")
                        .unwrap_or_else(|_| DEFAULT_RUST.to_string()),
                },
            }
        }

        fn available() -> Option<Self> {
            let servers = Self::from_env();
            let bun = servers.bun.get("/api/health");
            let rust = servers.rust.get("/api/health");
            if bun.is_ok() && rust.is_ok() {
                return Some(servers);
            }
            eprintln!(
                "SKIP parity tests: Bun/Rust servers are unavailable; set OPENCODE_PARITY_BUN_URL and OPENCODE_PARITY_RUST_URL or start servers on 127.0.0.1:4096/4097. bun={bun:?} rust={rust:?}"
            );
            None
        }
    }

    #[test]
    fn deterministic_parity() {
        let _guard = TEST_LOCK.lock().expect("test lock poisoned");
        let Some(servers) = Servers::available() else {
            return;
        };
        let mut failures = Vec::new();

        v2_parity(&servers, &mut failures);
        legacy_session_project_parity(&servers, &mut failures);
        file_and_metadata_parity(&servers, &mut failures);
        event_and_sqlite_parity(&servers, &mut failures);

        assert!(
            failures.is_empty(),
            "{} deterministic parity failures: {failures:?}",
            failures.len()
        );
    }

    #[test]
    fn live_runner_parity() {
        let _guard = TEST_LOCK.lock().expect("test lock poisoned");
        if std::env::var("OPENCODE_PARITY_LIVE_RUNNER").ok().as_deref() != Some("1") {
            eprintln!("SKIP live runner parity: set OPENCODE_PARITY_LIVE_RUNNER=1 to run provider-backed checks");
            return;
        }
        let Some(servers) = Servers::available() else {
            return;
        };
        let mut failures = Vec::new();

        runner_live_parity(&servers, &mut failures);

        assert!(
            failures.is_empty(),
            "{} live runner parity failures: {failures:?}",
            failures.len()
        );
    }

    #[test]
    fn live_feature_parity() {
        let _guard = TEST_LOCK.lock().expect("test lock poisoned");
        if std::env::var("OPENCODE_PARITY_LIVE_FEATURES")
            .ok()
            .as_deref()
            != Some("1")
        {
            eprintln!("SKIP live feature parity: set OPENCODE_PARITY_LIVE_FEATURES=1 to run provider-backed checks");
            return;
        }
        let Some(servers) = Servers::available() else {
            return;
        };
        let mut failures = Vec::new();

        feature_live_parity(&servers, &mut failures);

        assert!(
            failures.is_empty(),
            "{} live feature parity failures: {failures:?}",
            failures.len()
        );
    }

    fn v2_parity(servers: &Servers, failures: &mut Vec<String>) {
        let fixture = ensure_v2_fixture(servers);
        for path in [
            "/api/health".to_string(),
            "/api/session/active".to_string(),
            format!("/api/session/{fixture}"),
            "/api/session?limit=3".to_string(),
            "/api/session?directory=/workspace/packages/opencode&limit=5".to_string(),
            "/api/session?search=Goal".to_string(),
            "/api/session?search=zzz-no-match".to_string(),
            "/api/session?order=asc&limit=4".to_string(),
            format!("/api/session/{fixture}/history"),
            format!("/api/session/{fixture}/history?limit=1"),
            format!("/api/session/{fixture}/history?after=0"),
            format!("/api/session/{fixture}/message"),
            format!("/api/session/{fixture}/message?limit=5&order=asc"),
            "/api/session/ses_doesnotexist0000000000000".to_string(),
            format!("/api/session/{fixture}/message/msg_doesnotexist00000000000000"),
        ] {
            check_response(
                failures,
                &format!("GET {path}"),
                servers.bun.get(&path),
                servers.rust.get(&path),
            );
        }

        for path in [
            "/api/location",
            "/api/agent",
            "/api/command",
            "/api/skill",
            "/api/reference",
            "/api/model",
            "/api/provider",
            "/api/provider/opencode",
            "/api/fs/list?path=src",
        ] {
            check_response(
                failures,
                &format!("GET {path}"),
                servers.bun.get(path),
                servers.rust.get(path),
            );
        }

        let bun_page = get_json(&servers.bun, "/api/session?limit=2");
        let rust_page = get_json(&servers.rust, "/api/session?limit=2");
        check(
            failures,
            "cursors byte-identical",
            bun_page.get("cursor") == rust_page.get("cursor"),
            || {
                format!(
                    "bun={:?} rust={:?}",
                    bun_page.get("cursor"),
                    rust_page.get("cursor")
                )
            },
        );
        if let Some(cursor) = bun_page.pointer("/cursor/next").and_then(Value::as_str) {
            check_response(
                failures,
                "bun cursor readable by rust",
                servers
                    .bun
                    .get(&format!("/api/session?cursor={cursor}&limit=2")),
                servers
                    .rust
                    .get(&format!("/api/session?cursor={cursor}&limit=2")),
            );
            let prev = get_json(
                &servers.bun,
                &format!("/api/session?cursor={cursor}&limit=2"),
            )
            .pointer("/cursor/previous")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
            check_response(
                failures,
                "previous cursor parity",
                servers
                    .bun
                    .get(&format!("/api/session?cursor={prev}&limit=2")),
                servers
                    .rust
                    .get(&format!("/api/session?cursor={prev}&limit=2")),
            );
        } else {
            record(
                failures,
                "bun cursor readable by rust",
                "missing next cursor after self-seeding",
            );
        }
        check_response(
            failures,
            "invalid cursor parity",
            servers.bun.get("/api/session?cursor=%21%21not-base64"),
            servers.rust.get("/api/session?cursor=%21%21not-base64"),
        );

        let prompt = json!({"text": "cross-server admission (rust first)"});
        let rust_admit = servers
            .rust
            .post(
                &format!("/api/session/{fixture}/prompt"),
                Some(&json!({"prompt": prompt, "resume": false})),
            )
            .expect("rust admit request failed");
        check(
            failures,
            "rust admit succeeds",
            rust_admit.status == 200,
            || rust_admit.body.clone(),
        );
        let admitted = rust_admit.json()["data"].clone();
        let bun_retry = servers
            .bun
            .post(
                &format!("/api/session/{fixture}/prompt"),
                Some(&json!({"id": admitted["id"], "prompt": prompt, "resume": false})),
            )
            .expect("bun retry request failed");
        check(
            failures,
            "bun reconciles rust admission",
            bun_retry.status == 200 && bun_retry.body == rust_admit.body,
            || bun_retry.body.clone(),
        );
        check_response(
            failures,
            "conflict parity",
            servers.bun.post(
                &format!("/api/session/{fixture}/prompt"),
                Some(&json!({"id": admitted["id"], "prompt": {"text": "different"}, "resume": false})),
            ),
            servers.rust.post(
                &format!("/api/session/{fixture}/prompt"),
                Some(&json!({"id": admitted["id"], "prompt": {"text": "different"}, "resume": false})),
            ),
        );

        let prompt = json!({"text": "cross-server admission (bun first)", "files": [{"uri": "file:///workspace/README.md", "name": "README.md"}]});
        let bun_admit = servers
            .bun
            .post(
                &format!("/api/session/{fixture}/prompt"),
                Some(&json!({"prompt": prompt, "delivery": "queue", "resume": false})),
            )
            .expect("bun admit request failed");
        check(
            failures,
            "bun admit succeeds",
            bun_admit.status == 200,
            || bun_admit.body.clone(),
        );
        let admitted = bun_admit.json()["data"].clone();
        let rust_retry = servers
            .rust
            .post(
                &format!("/api/session/{fixture}/prompt"),
                Some(&json!({"id": admitted["id"], "prompt": prompt, "delivery": "queue", "resume": false})),
            )
            .expect("rust retry request failed");
        check(
            failures,
            "rust reconciles bun admission",
            rust_retry.status == 200 && rust_retry.body == bun_admit.body,
            || rust_retry.body.clone(),
        );

        check_response(
            failures,
            "history parity after interleaved admissions",
            servers
                .bun
                .get(&format!("/api/session/{fixture}/history?limit=100")),
            servers
                .rust
                .get(&format!("/api/session/{fixture}/history?limit=100")),
        );
        let history = get_json(
            &servers.bun,
            &format!("/api/session/{fixture}/history?limit=100"),
        );
        check(
            failures,
            "sequences strictly increasing",
            seqs(&history) == sorted_unique(seqs(&history)),
            || format!("{:?}", seqs(&history)),
        );

        let created = servers
            .rust
            .post(
                "/api/session",
                Some(&json!({"location": {"directory": "/workspace/packages/opencode"}, "agent": "plan"})),
            )
            .expect("rust create request failed");
        check(
            failures,
            "rust v2 create succeeds",
            created.status == 200,
            || created.body.clone(),
        );
        let session_id = created.json()["data"]["id"]
            .as_str()
            .expect("created session id")
            .to_string();
        let bun_view = servers.bun.get(&format!("/api/session/{session_id}"));
        let rust_view = servers.rust.get(&format!("/api/session/{session_id}"));
        check_response(
            failures,
            "created session parity across servers",
            bun_view.clone(),
            rust_view,
        );
        check(
            failures,
            "create response matches later reads",
            created.json() == bun_view.expect("bun view failed").json(),
            || "created response differed from read response".to_string(),
        );
        let adopted = servers
            .bun
            .post("/api/session", Some(&json!({"id": session_id})))
            .expect("bun adopt request failed");
        check(
            failures,
            "bun adopts rust-created session",
            adopted.status == 200 && adopted.json()["data"]["id"] == session_id,
            || adopted.body.clone(),
        );

        for (index, client) in [&servers.bun, &servers.rust, &servers.bun, &servers.rust]
            .into_iter()
            .enumerate()
        {
            let body = client
                .post(
                    &format!("/api/session/{session_id}/prompt"),
                    Some(&json!({"prompt": {"text": format!("turn {index}")}, "resume": false})),
                )
                .expect("alternating admission request failed");
            check(
                failures,
                &format!("alternating admission {index} via {}", client.label),
                body.status == 200,
                || body.body.clone(),
            );
        }
        check_response(
            failures,
            "alternating history parity",
            servers
                .bun
                .get(&format!("/api/session/{session_id}/history")),
            servers
                .rust
                .get(&format!("/api/session/{session_id}/history")),
        );
        let data = get_json(&servers.bun, &format!("/api/session/{session_id}/history"))["data"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        check(
            failures,
            "all four admissions durable",
            data.len() == 4,
            || data.len().to_string(),
        );
        let admission_seqs = data
            .iter()
            .filter_map(|event| event.pointer("/durable/seq").and_then(Value::as_i64))
            .collect::<Vec<_>>();
        let contiguous = admission_seqs
            .first()
            .map(|first| {
                admission_seqs == (*first..*first + admission_seqs.len() as i64).collect::<Vec<_>>()
            })
            .unwrap_or(false);
        check(failures, "admission seqs contiguous", contiguous, || {
            format!("{admission_seqs:?}")
        });

        projected_message_parity(servers, failures);
        check_response(
            failures,
            "interrupt parity",
            servers
                .bun
                .post(&format!("/api/session/{session_id}/interrupt"), None),
            servers
                .rust
                .post(&format!("/api/session/{session_id}/interrupt"), None),
        );
    }

    fn ensure_v2_fixture(servers: &Servers) -> String {
        let page = get_json(&servers.bun, "/api/session?limit=3");
        let count = page["data"].as_array().map(Vec::len).unwrap_or_default();
        for index in count..3 {
            servers
                .rust
                .post(
                    "/api/session",
                    Some(&json!({
                        "location": {"directory": "/workspace"},
                        "model": {"id": "big-pickle", "providerID": "opencode"},
                        "title": format!("Rust parity seed {index}")
                    })),
                )
                .expect("failed to seed v2 session");
        }
        let page = get_json(&servers.bun, "/api/session?limit=1");
        page["data"][0]["id"]
            .as_str()
            .expect("fixture session id")
            .to_string()
    }

    fn projected_message_parity(servers: &Servers, failures: &mut Vec<String>) {
        let sessions = get_json(&servers.bun, "/api/session?limit=50")["data"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut covered = 0;
        for session in sessions {
            let sid = session["id"].as_str().unwrap_or_default();
            let bun_msgs = servers
                .bun
                .get(&format!("/api/session/{sid}/message?order=asc"))
                .expect("bun projected message request failed");
            if bun_msgs.status != 200
                || bun_msgs.json()["data"].as_array().is_none_or(Vec::is_empty)
            {
                continue;
            }
            covered += 1;
            check_response(
                failures,
                &format!("projected messages parity {sid}"),
                Ok(bun_msgs.clone()),
                servers
                    .rust
                    .get(&format!("/api/session/{sid}/message?order=asc")),
            );
            let first = bun_msgs.json()["data"][0]["id"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            check_response(
                failures,
                &format!("projected single message parity {sid}"),
                servers
                    .bun
                    .get(&format!("/api/session/{sid}/message/{first}")),
                servers
                    .rust
                    .get(&format!("/api/session/{sid}/message/{first}")),
            );
            let page = get_json(&servers.bun, &format!("/api/session/{sid}/message?limit=1"));
            if let Some(next_cursor) = page.pointer("/cursor/next").and_then(Value::as_str) {
                check_response(
                    failures,
                    &format!("message cursor parity {sid}"),
                    servers.bun.get(&format!(
                        "/api/session/{sid}/message?cursor={next_cursor}&limit=2"
                    )),
                    servers.rust.get(&format!(
                        "/api/session/{sid}/message?cursor={next_cursor}&limit=2"
                    )),
                );
            }
            if covered >= 3 {
                break;
            }
        }
        if covered == 0 {
            eprintln!("INFO projected v2 message parity had no existing runner-output fixture; OPENCODE_PARITY_LIVE_RUNNER covers this path");
            return;
        }
        println!("PASS at least one session with projected messages covered covered={covered}");
    }

    fn legacy_session_project_parity(servers: &Servers, failures: &mut Vec<String>) {
        for path in [
            "/session",
            "/session?directory=/workspace/packages/opencode",
            "/session?directory=/workspace",
            "/session?roots=true",
            "/session?limit=3",
            "/session?search=Goal",
        ] {
            check_json(
                failures,
                &format!("GET {path}"),
                &servers.bun,
                &servers.rust,
                path,
            );
        }

        let sessions = get_json(&servers.bun, "/session");
        for item in sessions.as_array().cloned().unwrap_or_default() {
            let sid = item["id"].as_str().unwrap_or_default();
            check_json(
                failures,
                &format!("GET /session/{sid}"),
                &servers.bun,
                &servers.rust,
                &format!("/session/{sid}"),
            );
        }

        let created = servers
            .rust
            .post(
                "/session",
                Some(&json!({"title": "Created by Rust server"})),
            )
            .expect("legacy create request failed");
        let sid = created.json()["id"]
            .as_str()
            .expect("legacy session id")
            .to_string();
        check(
            failures,
            "cross-server create (rust write, bun read)",
            get_json(&servers.bun, &format!("/session/{sid}")) == created.json(),
            || created.body.clone(),
        );

        let goal = json!({
            "goal": {
                "text": "Rust strangler port",
                "status": "active",
                "created": 1783197000000_i64,
                "updated": 1783197000000_i64,
                "progress": 5,
                "revision": 1
            }
        });
        let updated = servers
            .rust
            .patch(&format!("/session/{sid}"), &json!({"metadata": goal}))
            .expect("legacy metadata patch failed");
        let bun_view = get_json(&servers.bun, &format!("/session/{sid}"));
        check(
            failures,
            "cross-server patch metadata (rust write, bun read)",
            bun_view == updated.json(),
            || updated.body.clone(),
        );
        check(
            failures,
            "legacy metadata goal text persisted",
            bun_view
                .pointer("/metadata/goal/text")
                .and_then(Value::as_str)
                == Some("Rust strangler port"),
            || bun_view.to_string(),
        );

        servers
            .bun
            .patch(
                &format!("/session/{sid}"),
                &json!({"title": "Renamed by Bun server"}),
            )
            .expect("bun title patch failed");
        check_json(
            failures,
            "cross-server patch title (bun write, rust read)",
            &servers.bun,
            &servers.rust,
            &format!("/session/{sid}"),
        );

        let sessions = get_json(&servers.bun, "/session");
        for item in sessions.as_array().cloned().unwrap_or_default() {
            let sid = item["id"].as_str().unwrap_or_default();
            check_json(
                failures,
                &format!("GET /session/{sid}/message"),
                &servers.bun,
                &servers.rust,
                &format!("/session/{sid}/message"),
            );
        }

        let fixture = sessions
            .as_array()
            .and_then(|items| {
                items.iter().find_map(|item| {
                    let sid = item["id"].as_str()?;
                    let messages = get_json(&servers.bun, &format!("/session/{sid}/message"));
                    messages
                        .as_array()
                        .filter(|items| !items.is_empty())
                        .map(|_| sid.to_string())
                })
            })
            .unwrap_or_else(|| {
                servers
                    .rust
                    .post("/session", Some(&json!({"title": "Parity fixture"})))
                    .expect("legacy fixture create failed")
                    .json()["id"]
                    .as_str()
                    .expect("legacy fixture id")
                    .to_string()
            });
        let bun_pages = legacy_message_pages(&servers.bun, &fixture);
        let rust_pages = legacy_message_pages(&servers.rust, &fixture);
        check(
            failures,
            "paginated walk page count",
            bun_pages.len() == rust_pages.len(),
            || format!("bun={} rust={}", bun_pages.len(), rust_pages.len()),
        );
        for (index, (bun_page, rust_page)) in bun_pages.iter().zip(rust_pages.iter()).enumerate() {
            check(
                failures,
                &format!("paginated walk page {index}"),
                bun_page == rust_page,
                || format!("bun={bun_page:?} rust={rust_page:?}"),
            );
        }

        let full = get_json(&servers.bun, &format!("/session/{fixture}/message"))
            .as_array()
            .cloned()
            .unwrap_or_default();
        for entry in full.iter().take(3).chain(full.iter().rev().take(3)) {
            let mid = entry
                .pointer("/info/id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            check_json(
                failures,
                &format!("GET /session/{fixture}/message/{mid}"),
                &servers.bun,
                &servers.rust,
                &format!("/session/{fixture}/message/{mid}"),
            );
        }

        check_json(
            failures,
            &format!("GET /session/{fixture}/todo"),
            &servers.bun,
            &servers.rust,
            &format!("/session/{fixture}/todo"),
        );
        let child = servers
            .rust
            .post(
                "/session",
                Some(&json!({"title": "Child of fixture", "parentID": fixture})),
            )
            .expect("legacy child create failed")
            .json();
        let children = get_json(&servers.bun, &format!("/session/{fixture}/children"));
        check_json(
            failures,
            &format!("GET /session/{fixture}/children"),
            &servers.bun,
            &servers.rust,
            &format!("/session/{fixture}/children"),
        );
        check(
            failures,
            "legacy child visible through bun",
            children
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["id"] == child["id"])),
            || children.to_string(),
        );

        check_json(
            failures,
            "GET /project",
            &servers.bun,
            &servers.rust,
            "/project",
        );
        let mut bun_current = get_json(&servers.bun, "/project/current");
        let mut rust_current = get_json(&servers.rust, "/project/current");
        remove_time_updated(&mut bun_current);
        remove_time_updated(&mut rust_current);
        check(
            failures,
            "GET /project/current (modulo cached time.updated)",
            bun_current == rust_current,
            || format!("bun={bun_current} rust={rust_current}"),
        );
    }

    fn legacy_message_pages(client: &Client, fixture: &str) -> Vec<Value> {
        let mut pages = Vec::new();
        let mut cursor = None;
        loop {
            let path = match &cursor {
                Some(cursor) => format!("/session/{fixture}/message?limit=25&before={cursor}"),
                None => format!("/session/{fixture}/message?limit=25"),
            };
            let response = client
                .get(&path)
                .expect("legacy message page request failed");
            cursor = response.next_cursor.clone();
            pages.push(response.json());
            if cursor.is_none() {
                return pages;
            }
        }
    }

    fn file_and_metadata_parity(servers: &Servers, failures: &mut Vec<String>) {
        for path in [".", "src", "src/tool", "test", "src/session"] {
            let encoded = percent_encode(path);
            check_json(
                failures,
                &format!("GET /file?path={path}"),
                &servers.bun,
                &servers.rust,
                &format!("/file?path={encoded}"),
            );
        }
        for path in [
            "package.json",
            "src/tool/goal.ts",
            "script/schema.ts",
            "no/such/file.txt",
        ] {
            let encoded = percent_encode(path);
            check_json(
                failures,
                &format!("GET /file/content?path={path}"),
                &servers.bun,
                &servers.rust,
                &format!("/file/content?path={encoded}"),
            );
        }

        for pattern in [
            "GoalSetTool",
            "SessionPrompt.Service",
            "does_not_exist_anywhere_123",
        ] {
            let oracle = rg_oracle(pattern);
            let expected_count = 10.min(oracle.len());
            let encoded = percent_encode(pattern);
            for client in [&servers.bun, &servers.rust] {
                let items = get_json(client, &format!("/find?pattern={encoded}"));
                let keys = items
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|item| {
                        Some((
                            item.pointer("/path/text")?.as_str()?.to_string(),
                            item["line_number"].as_i64()?,
                            item.pointer("/lines/text")?.as_str()?.to_string(),
                        ))
                    })
                    .collect::<BTreeSet<_>>();
                check(
                    failures,
                    &format!("GET /find?pattern={pattern} [{}]", client.label),
                    keys.is_subset(&oracle) && keys.len() == expected_count,
                    || {
                        format!(
                            "{}/{} all-in-oracle={}",
                            keys.len(),
                            expected_count,
                            keys.is_subset(&oracle)
                        )
                    },
                );
            }
        }

        for (query, expected_top) in [
            ("registry", "registry"),
            ("goal.ts", "goal"),
            ("prompt.test", "prompt.test"),
        ] {
            let bun = get_json(&servers.bun, &format!("/find/file?query={query}&limit=10"));
            let rust = get_json(&servers.rust, &format!("/find/file?query={query}&limit=10"));
            let bun_top = bun
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .take(3)
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect::<BTreeSet<_>>();
            let rust_items = rust
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect::<Vec<_>>();
            let rust_top = rust_items.iter().take(3).cloned().collect::<BTreeSet<_>>();
            check(
                failures,
                &format!("GET /find/file?query={query} top-hit agreement"),
                rust_top.iter().any(|path| path.contains(expected_top))
                    && bun_top.iter().any(|path| rust_items.contains(path)),
                || format!("bun_top={bun_top:?} rust_top={rust_top:?}"),
            );
        }

        check_json(
            failures,
            "GET /find/symbol",
            &servers.bun,
            &servers.rust,
            "/find/symbol?query=x",
        );
        check_json(
            failures,
            "GET /file/status",
            &servers.bun,
            &servers.rust,
            "/file/status",
        );

        for path in [
            "/global/health",
            "/global/config",
            "/permission",
            "/question",
            "/path",
            "/vcs",
            "/vcs/status",
            "/lsp",
            "/formatter",
        ] {
            check_json(
                failures,
                &format!("GET {path}"),
                &servers.bun,
                &servers.rust,
                path,
            );
        }

        let bun_cfg = get_json(&servers.bun, "/config");
        let rust_cfg = get_json(&servers.rust, "/config");
        check(
            failures,
            "config preserves schema",
            rust_cfg.get("$schema") == bun_cfg.get("$schema")
                && rust_cfg.get("$schema").and_then(Value::as_str)
                    == Some("https://opencode.ai/config.json"),
            || format!("bun={bun_cfg} rust={rust_cfg}"),
        );
        check(
            failures,
            "config has username",
            rust_cfg
                .get("username")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty()),
            || rust_cfg.to_string(),
        );

        let bun_config_providers = get_json(&servers.bun, "/config/providers");
        let rust_config_providers = get_json(&servers.rust, "/config/providers");
        let bun_opencode = find_by_id(&bun_config_providers, "providers", "opencode");
        let rust_opencode = find_by_id(&rust_config_providers, "providers", "opencode");
        check(
            failures,
            "config/providers exposes opencode",
            bun_opencode.is_some() && rust_opencode.is_some(),
            || format!("bun={bun_config_providers} rust={rust_config_providers}"),
        );
        if let (Some(bun_opencode), Some(rust_opencode)) = (bun_opencode, rust_opencode) {
            check(
                failures,
                "config/providers opencode default",
                bun_config_providers.pointer("/default/opencode")
                    == rust_config_providers.pointer("/default/opencode"),
                || format!("bun={bun_config_providers} rust={rust_config_providers}"),
            );
            check(
                failures,
                "config/providers opencode model set",
                string_set(bun_opencode, "models") == string_set(rust_opencode, "models"),
                || {
                    format!(
                        "bun={} rust={}",
                        string_set(bun_opencode, "models").len(),
                        string_set(rust_opencode, "models").len()
                    )
                },
            );
        }

        let bun_provider = get_json(&servers.bun, "/provider");
        let rust_provider = get_json(&servers.rust, "/provider");
        check(
            failures,
            "provider catalog non-empty",
            rust_provider["all"]
                .as_array()
                .is_some_and(|items| !items.is_empty()),
            || rust_provider.to_string(),
        );
        let bun_provider_ids = id_set(&bun_provider, "all");
        let rust_provider_ids = id_set(&rust_provider, "all");
        check(
            failures,
            "provider catalog contains bun providers",
            bun_provider_ids.is_subset(&rust_provider_ids),
            || {
                format!(
                    "missing={:?}",
                    bun_provider_ids
                        .difference(&rust_provider_ids)
                        .collect::<Vec<_>>()
                )
            },
        );
        check(
            failures,
            "provider connected includes opencode",
            rust_provider["connected"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item == "opencode")),
            || rust_provider.to_string(),
        );

        let auth = get_json(&servers.rust, "/provider/auth");
        check(
            failures,
            "provider/auth exposes opencode method",
            auth["opencode"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
                || auth["opencode"]
                    .as_object()
                    .is_some_and(|items| !items.is_empty()),
            || auth.to_string(),
        );

        let bun_commands = name_set(&get_json(&servers.bun, "/command"));
        let rust_commands = name_set(&get_json(&servers.rust, "/command"));
        check(
            failures,
            "command metadata includes builtin commands",
            ["init", "review", "goal"]
                .into_iter()
                .all(|name| rust_commands.contains(name)),
            || format!("rust={rust_commands:?}"),
        );
        check(
            failures,
            "command metadata includes local commands",
            ["commit", "rmslop", "issues", "learn"]
                .into_iter()
                .all(|name| rust_commands.contains(name)),
            || format!("rust={rust_commands:?}"),
        );
        check(
            failures,
            "command metadata covers bun names",
            bun_commands.is_subset(&rust_commands),
            || {
                format!(
                    "missing={:?}",
                    bun_commands.difference(&rust_commands).collect::<Vec<_>>()
                )
            },
        );

        let bun_agents = name_set(&get_json(&servers.bun, "/agent"));
        let rust_agents = name_set(&get_json(&servers.rust, "/agent"));
        check(
            failures,
            "agent metadata includes builtins",
            ["build", "plan", "goal", "general", "explore"]
                .into_iter()
                .all(|name| rust_agents.contains(name)),
            || format!("rust={rust_agents:?}"),
        );
        check(
            failures,
            "agent metadata includes local agents",
            ["duplicate-pr", "triage"]
                .into_iter()
                .all(|name| rust_agents.contains(name)),
            || format!("rust={rust_agents:?}"),
        );
        check(
            failures,
            "agent metadata covers bun names",
            bun_agents.is_subset(&rust_agents),
            || {
                format!(
                    "missing={:?}",
                    bun_agents.difference(&rust_agents).collect::<Vec<_>>()
                )
            },
        );

        let rust_skills = name_set(&get_json(&servers.rust, "/skill"));
        check(
            failures,
            "skill metadata includes effect skill",
            rust_skills.contains("effect"),
            || format!("{rust_skills:?}"),
        );
    }

    fn event_and_sqlite_parity(servers: &Servers, failures: &mut Vec<String>) {
        let bun_session = servers
            .bun
            .post("/session", Some(&json!({"title": "SSE parity bun"})))
            .expect("bun SSE session create failed")
            .json();
        let rust_session = servers
            .rust
            .post("/session", Some(&json!({"title": "SSE parity rust"})))
            .expect("rust SSE session create failed")
            .json();

        let bun_id = bun_session["id"].as_str().expect("bun session id");
        let rust_id = rust_session["id"].as_str().expect("rust session id");
        let bun_events = sse_capture(&servers.bun, 2, || {
            servers
                .bun
                .patch(
                    &format!("/session/{bun_id}"),
                    &json!({"title": "SSE parity bun"}),
                )
                .expect("bun SSE trigger failed");
        });
        let rust_events = sse_capture(&servers.rust, 2, || {
            servers
                .rust
                .patch(
                    &format!("/session/{rust_id}"),
                    &json!({"title": "SSE parity rust"}),
                )
                .expect("rust SSE trigger failed");
        });

        check(
            failures,
            "SSE first frame is server.connected",
            bun_events.first().map(normalize_event) == rust_events.first().map(normalize_event),
            || format!("bun={bun_events:?} rust={rust_events:?}"),
        );
        check(
            failures,
            "SSE session.updated shape",
            bun_events.get(1).map(normalize_event) == rust_events.get(1).map(normalize_event),
            || format!("bun={bun_events:?} rust={rust_events:?}"),
        );
        check(
            failures,
            "SSE event ids and types",
            bun_events.get(1).and_then(|event| event["type"].as_str()) == Some("session.updated")
                && rust_events.get(1).and_then(|event| event["type"].as_str())
                    == Some("session.updated")
                && rust_events
                    .get(1)
                    .and_then(|event| event["id"].as_str())
                    .is_some_and(|id| id.starts_with("evt_")),
            || format!("bun={bun_events:?} rust={rust_events:?}"),
        );

        let db = Connection::open(
            std::env::var("OPENCODE_PARITY_DB").unwrap_or_else(|_| DEFAULT_DB.to_string()),
        )
        .expect("failed to open parity SQLite database");
        check(
            failures,
            "durable event row structure",
            durable_row(&db, bun_id) == durable_row(&db, rust_id),
            || {
                format!(
                    "bun={:?} rust={:?}",
                    durable_row(&db, bun_id),
                    durable_row(&db, rust_id)
                )
            },
        );

        servers
            .rust
            .patch(
                &format!("/session/{rust_id}"),
                &json!({"title": "SSE parity rust 2"}),
            )
            .expect("rust second patch failed");
        let seqs = db
            .prepare("SELECT seq FROM event WHERE aggregate_id = ? ORDER BY seq")
            .expect("failed to prepare seq query")
            .query_map([rust_id], |row| row.get::<_, i64>(0))
            .expect("failed to query seq rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("failed to collect seq rows");
        check(
            failures,
            "rust durable seq progression",
            seqs == (0..seqs.len() as i64).collect::<Vec<_>>(),
            || format!("{seqs:?}"),
        );

        let parent = servers
            .rust
            .post("/session", Some(&json!({"title": "Delete tree parent"})))
            .expect("delete parent create failed")
            .json();
        let parent_id = parent["id"].as_str().expect("parent id");
        let child = servers
            .rust
            .post(
                "/session",
                Some(&json!({"title": "Delete tree child", "parentID": parent_id})),
            )
            .expect("delete child create failed")
            .json();
        let child_id = child["id"].as_str().expect("child id");
        servers
            .rust
            .delete(&format!("/session/{parent_id}"))
            .expect("delete parent failed");
        for sid in [parent_id, child_id] {
            let bun_get = servers
                .bun
                .get(&format!("/session/{sid}"))
                .expect("bun delete read failed");
            check(
                failures,
                &format!(
                    "bun 404 after rust delete ({})",
                    sid.chars().take(20).collect::<String>()
                ),
                bun_get.status == 404,
                || bun_get.body.clone(),
            );
            let rows = db
                .query_row(
                    "SELECT count(*) FROM event WHERE aggregate_id = ?",
                    [sid],
                    |row| row.get::<_, i64>(0),
                )
                .expect("failed to query deleted durable rows");
            check(
                failures,
                &format!(
                    "durable history removed ({})",
                    sid.chars().take(20).collect::<String>()
                ),
                rows == 0,
                || rows.to_string(),
            );
        }

        check_json(
            failures,
            "GET /session/status",
            &servers.bun,
            &servers.rust,
            "/session/status",
        );
        for sid in [bun_id, rust_id] {
            let _ = servers.rust.delete(&format!("/session/{sid}"));
        }
    }

    fn runner_live_parity(servers: &Servers, failures: &mut Vec<String>) {
        let created = servers
            .rust
            .post_slow(
                "/api/session",
                Some(&json!({
                    "location": {"directory": "/workspace/rust"},
                    "model": {"id": "big-pickle", "providerID": "opencode"}
                })),
            )
            .expect("runner create failed");
        check(failures, "create session", created.status == 200, || {
            created.body.clone()
        });
        let sid = created.json()["data"]["id"]
            .as_str()
            .expect("runner session id")
            .to_string();

        let admitted = servers
            .rust
            .post_slow(
                &format!("/api/session/{sid}/prompt"),
                Some(&json!({"prompt": {"text": "What is 12+5? Reply with only the number."}})),
            )
            .expect("runner prompt failed");
        check(
            failures,
            "prompt admitted with wake",
            admitted.status == 200,
            || admitted.body.clone(),
        );
        let active = get_json(&servers.rust, "/api/session/active");
        check(
            failures,
            "session active during drain",
            active
                .pointer(&format!("/data/{sid}/type"))
                .and_then(Value::as_str)
                == Some("running"),
            || active.to_string(),
        );
        let wait = servers
            .rust
            .post_slow(&format!("/api/session/{sid}/wait"), None)
            .expect("runner wait failed");
        check(failures, "wait settles", wait.status == 204, || {
            wait.body.clone()
        });

        let messages = get_json(
            &servers.rust,
            &format!("/api/session/{sid}/message?order=asc"),
        );
        let data = messages["data"].as_array().cloned().unwrap_or_default();
        check(
            failures,
            "user message projected",
            data.iter().any(|message| message["type"] == "user"),
            || messages.to_string(),
        );
        let assistant = data
            .iter()
            .filter(|message| message["type"] == "assistant")
            .cloned()
            .collect::<Vec<_>>();
        check(
            failures,
            "assistant message projected",
            assistant.len() == 1,
            || messages.to_string(),
        );
        let texts = text_parts(&assistant);
        check(
            failures,
            "assistant answered",
            texts.iter().any(|text| text.contains("17")),
            || format!("{texts:?}"),
        );
        check(
            failures,
            "step settled",
            assistant
                .last()
                .and_then(|message| message["finish"].as_str())
                == Some("stop"),
            || {
                format!(
                    "{:?}",
                    assistant.last().and_then(|message| message.get("finish"))
                )
            },
        );
        check(
            failures,
            "tokens accounted",
            assistant
                .last()
                .and_then(|message| message.pointer("/tokens/input"))
                .and_then(Value::as_i64)
                .is_some_and(|tokens| tokens > 0),
            || {
                format!(
                    "{:?}",
                    assistant.last().and_then(|message| message.get("tokens"))
                )
            },
        );

        let history = get_json(
            &servers.rust,
            &format!("/api/session/{sid}/history?limit=100"),
        );
        let kinds = event_kinds(&history);
        check(
            failures,
            "trace starts with admission",
            kinds
                .first()
                .is_some_and(|kind| kind == "session.next.prompt.admitted"),
            || format!("{kinds:?}"),
        );
        check(
            failures,
            "prompted precedes step",
            index_of(&kinds, "session.next.prompted")
                < index_of(&kinds, "session.next.step.started"),
            || format!("{kinds:?}"),
        );
        check(
            failures,
            "step ended settles trace",
            kinds
                .last()
                .is_some_and(|kind| kind == "session.next.step.ended"),
            || format!("{kinds:?}"),
        );
        let seqs = seqs(&history);
        let contiguous = seqs
            .first()
            .map(|first| seqs == (*first..*first + seqs.len() as i64).collect::<Vec<_>>())
            .unwrap_or(false);
        check(failures, "sequences contiguous", contiguous, || {
            format!("{seqs:?}")
        });
        for path in [
            format!("/api/session/{sid}/message?order=asc"),
            format!("/api/session/{sid}/history?limit=100"),
            format!("/api/session/{sid}"),
        ] {
            check_response(
                failures,
                &format!("byte parity {path}"),
                servers.bun.get(&path),
                servers.rust.get(&path),
            );
        }

        let tool_admit = servers
            .rust
            .post_slow(
                &format!("/api/session/{sid}/prompt"),
                Some(&json!({"prompt": {"text": "Use the read tool to read Cargo.toml and report the workspace members. Be brief."}})),
            )
            .expect("tool prompt failed");
        check(
            failures,
            "tool prompt admitted",
            tool_admit.status == 200,
            || tool_admit.body.clone(),
        );
        let _ = servers
            .rust
            .post_slow(&format!("/api/session/{sid}/wait"), None);
        wait_until_idle(&servers.rust, &sid, Duration::from_secs(120));
        let history = get_json(
            &servers.rust,
            &format!("/api/session/{sid}/history?limit=100"),
        );
        let kinds = event_kinds(&history);
        let called = count_kind(&kinds, "session.next.tool.called");
        let succeeded = count_kind(&kinds, "session.next.tool.success");
        let failed = count_kind(&kinds, "session.next.tool.failed");
        check(failures, "tool call recorded durably", called >= 1, || {
            format!("{kinds:?}")
        });
        check(
            failures,
            "all tool calls settled",
            called == succeeded + failed,
            || format!("called={called} ok={succeeded} failed={failed}"),
        );
        check(
            failures,
            "continuation turn ran",
            count_kind(&kinds, "session.next.step.started") >= 3,
            || count_kind(&kinds, "session.next.step.started").to_string(),
        );
        check(
            failures,
            "trace settles after tools",
            kinds
                .last()
                .is_some_and(|kind| kind == "session.next.step.ended"),
            || format!("{kinds:?}"),
        );
        let final_texts = text_parts(
            get_json(
                &servers.rust,
                &format!("/api/session/{sid}/message?order=asc"),
            )["data"]
                .as_array()
                .unwrap_or(&Vec::new()),
        );
        check(
            failures,
            "tool answer mentions members",
            final_texts
                .iter()
                .any(|text| text.contains("server") && text.contains("client")),
            || format!("{:?}", final_texts.last()),
        );
        check_response(
            failures,
            "byte parity after tool loop",
            servers
                .bun
                .get(&format!("/api/session/{sid}/message?order=asc")),
            servers
                .rust
                .get(&format!("/api/session/{sid}/message?order=asc")),
        );

        let bun_admit = servers
            .bun
            .post_slow(
                &format!("/api/session/{sid}/prompt"),
                Some(&json!({"prompt": {"text": "Now reply with exactly: HANDOFF-OK"}})),
            )
            .expect("bun continuation failed");
        check(
            failures,
            "bun continuation admitted",
            bun_admit.status == 200,
            || bun_admit.body.clone(),
        );
        let handed_off = wait_for_text(&servers.bun, &sid, "HANDOFF-OK", Duration::from_secs(120));
        check(
            failures,
            "bun runner continued rust session",
            handed_off,
            || "HANDOFF-OK not observed".to_string(),
        );
        check_response(
            failures,
            "byte parity after cross-runner handoff",
            servers
                .bun
                .get(&format!("/api/session/{sid}/message?order=asc")),
            servers
                .rust
                .get(&format!("/api/session/{sid}/message?order=asc")),
        );
    }

    fn feature_live_parity(servers: &Servers, failures: &mut Vec<String>) {
        let workdir =
            std::env::temp_dir().join(format!("opencode-feature-demo-{}", unique_suffix()));
        let _ = fs::remove_dir_all(&workdir);
        fs::create_dir_all(&workdir).expect("failed to create feature workdir");
        Command::new("git")
            .arg("init")
            .arg("-q")
            .current_dir(&workdir)
            .status()
            .expect("failed to init feature git repo");
        fs::write(workdir.join("notes.txt"), "alpha\nbeta\ngamma\n")
            .expect("failed to write notes fixture");

        let goal_sid = create_live_session(servers, &workdir, Some("goal"));
        turn(
            servers,
            &goal_sid,
            r#"Call the goal_set tool with text "Ship the Rust port". Then stop and confirm in one short line."#,
        );
        let goal = goal_metadata(&servers.rust, &goal_sid);
        check(
            failures,
            "goal_set stores durable goal",
            goal.pointer("/text").and_then(Value::as_str) == Some("Ship the Rust port"),
            || goal.to_string(),
        );
        check(
            failures,
            "goal_set marks active, revision 1",
            goal.pointer("/status").and_then(Value::as_str) == Some("active")
                && goal.pointer("/revision").and_then(Value::as_i64) == Some(1),
            || goal.to_string(),
        );
        let states = tool_states(&servers.rust, &goal_sid, "goal_set");
        check(
            failures,
            "goal_set tool settled",
            states.last().and_then(|state| state["status"].as_str()) == Some("completed"),
            || format!("{states:?}"),
        );
        check(
            failures,
            "goal_set output format",
            states.last().is_some_and(|state| {
                state
                    .pointer("/content/0/text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| {
                        text.contains("Goal: Ship the Rust port") && text.contains("Status: active")
                    })
            }),
            || format!("{states:?}"),
        );

        turn(
            servers,
            &goal_sid,
            "Call the goal_status tool, then stop and report the status in one line.",
        );
        let states = tool_states(&servers.rust, &goal_sid, "goal_status");
        check(
            failures,
            "goal_status reads goal",
            states.last().is_some_and(|state| {
                state
                    .pointer("/content/0/text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.contains("Ship the Rust port"))
            }),
            || format!("{states:?}"),
        );

        turn(
            servers,
            &goal_sid,
            "Call goal_summarize_state with progress 40 and this exact summary:\n## Progress\n- Runner ported\n## Current State\n- All suites green\n## Blockers\n- None\n## Next Steps\n- Polish TUI\nThen stop.",
        );
        let goal = goal_metadata(&servers.rust, &goal_sid);
        check(
            failures,
            "goal_summarize_state persists progress",
            goal.pointer("/progress").and_then(Value::as_i64) == Some(40),
            || goal.to_string(),
        );
        check(
            failures,
            "summary recorded with revision",
            goal["summaries"]
                .as_array()
                .and_then(|items| items.last())
                .is_some_and(|summary| {
                    summary["progress"].as_i64() == Some(40)
                        && summary["summary"]
                            .as_str()
                            .is_some_and(|text| text.contains("Runner ported"))
                }),
            || goal.to_string(),
        );

        turn(
            servers,
            &goal_sid,
            r#"Call goal_summarize_state with progress 41 and summary exactly "just plain text, no headers". Report the exact error you get in one line."#,
        );
        let errors = tool_states(&servers.rust, &goal_sid, "goal_summarize_state")
            .into_iter()
            .filter(|state| state["status"] == "error")
            .collect::<Vec<_>>();
        check(
            failures,
            "invalid summary rejected with validator message",
            errors.last().is_some_and(|state| {
                state
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .is_some_and(|message| message.contains("size 2 markdown headers"))
            }),
            || format!("{errors:?}"),
        );
        check(
            failures,
            "failed summary does not bump progress",
            goal_metadata(&servers.rust, &goal_sid)
                .pointer("/progress")
                .and_then(Value::as_i64)
                == Some(40),
            || goal_metadata(&servers.rust, &goal_sid).to_string(),
        );

        for (prompt, name, expected) in [
            (
                "Call the goal_pause tool, then stop.",
                "goal_pause pauses durable goal",
                "paused",
            ),
            (
                "Call the goal_resume tool, then stop.",
                "goal_resume reactivates goal",
                "active",
            ),
        ] {
            turn(servers, &goal_sid, prompt);
            check(
                failures,
                name,
                goal_metadata(&servers.rust, &goal_sid)
                    .pointer("/status")
                    .and_then(Value::as_str)
                    == Some(expected),
                || goal_metadata(&servers.rust, &goal_sid).to_string(),
            );
        }
        turn(
            servers,
            &goal_sid,
            "Call the goal_complete tool, then stop.",
        );
        check(
            failures,
            "goal_complete clears goal from session",
            goal_metadata(&servers.rust, &goal_sid).is_null(),
            || goal_metadata(&servers.rust, &goal_sid).to_string(),
        );
        let states = tool_states(&servers.rust, &goal_sid, "goal_complete");
        check(
            failures,
            "goal_complete reports completed",
            states.last().is_some_and(|state| {
                state
                    .pointer("/content/0/text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.contains("Status: completed"))
            }),
            || format!("{states:?}"),
        );
        turn(servers, &goal_sid, "Call the goal_pause tool, then stop.");
        let states = tool_states(&servers.rust, &goal_sid, "goal_pause");
        check(
            failures,
            "pause without goal reports none",
            states.last().is_some_and(|state| {
                state
                    .pointer("/content/0/text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.contains("No session goal is currently set."))
            }),
            || format!("{states:?}"),
        );
        live_cross_server_byte_parity(servers, failures, "goal mode", &goal_sid);

        let plan_sid = create_live_session(servers, &workdir, Some("plan"));
        turn(
            servers,
            &plan_sid,
            "Use the edit tool to replace beta with delta in notes.txt. If the tool errors, report the exact error text. Then use the read tool to read notes.txt and report its second line.",
        );
        let edit_states = tool_states(&servers.rust, &plan_sid, "edit");
        check(
            failures,
            "plan mode denies edit with Bun's message",
            edit_states.last().is_some_and(|state| {
                state["status"] == "error"
                    && state
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .is_some_and(|message| {
                            message.starts_with("Unable to edit ") && message.ends_with("notes.txt")
                        })
            }),
            || format!("{edit_states:?}"),
        );
        check(
            failures,
            "plan mode left the file untouched",
            fs::read_to_string(workdir.join("notes.txt")).expect("failed to read notes fixture")
                == "alpha\nbeta\ngamma\n",
            || fs::read_to_string(workdir.join("notes.txt")).unwrap_or_default(),
        );
        check(
            failures,
            "plan mode allows read",
            tool_states(&servers.rust, &plan_sid, "read")
                .last()
                .and_then(|state| state["status"].as_str())
                == Some("completed"),
            || format!("{:?}", tool_states(&servers.rust, &plan_sid, "read")),
        );
        check(
            failures,
            "plan agent advertises no edit/write tools",
            servers
                .rust
                .get(&format!("/api/session/{plan_sid}/history?limit=100"))
                .expect("plan history failed")
                .body
                .contains("session.next.tool"),
            || "missing session.next.tool marker".to_string(),
        );
        live_cross_server_byte_parity(servers, failures, "plan mode", &plan_sid);

        let build_sid = create_live_session(servers, &workdir, Some("build"));
        turn(
            servers,
            &build_sid,
            r#"Use the edit tool to replace beta with delta in notes.txt, then use the write tool to create out/result.txt whose content is the single word "done" with no punctuation. Then stop."#,
        );
        check(
            failures,
            "build mode edit applied",
            fs::read_to_string(workdir.join("notes.txt")).expect("failed to read notes fixture")
                == "alpha\ndelta\ngamma\n",
            || fs::read_to_string(workdir.join("notes.txt")).unwrap_or_default(),
        );
        check(
            failures,
            "build mode write created nested file",
            fs::read_to_string(workdir.join("out/result.txt"))
                .unwrap_or_default()
                .trim()
                .trim_end_matches('.')
                == "done",
            || fs::read_to_string(workdir.join("out/result.txt")).unwrap_or_default(),
        );
        let edit_states = tool_states(&servers.rust, &build_sid, "edit");
        check(
            failures,
            "edit records unified patch + counts",
            edit_states.last().is_some_and(|state| {
                state
                    .pointer("/structured/files/0/additions")
                    .and_then(Value::as_i64)
                    == Some(1)
                    && state
                        .pointer("/structured/files/0/deletions")
                        .and_then(Value::as_i64)
                        == Some(1)
                    && state
                        .pointer("/structured/files/0/patch")
                        .and_then(Value::as_str)
                        .is_some_and(|patch| patch.contains("@@"))
            }),
            || format!("{edit_states:?}"),
        );
        live_cross_server_byte_parity(servers, failures, "build edits", &build_sid);

        turn(
            servers,
            &build_sid,
            r#"Use the edit tool on notes.txt with oldString "does-not-exist-anywhere" and newString "x". Report the exact error in one line."#,
        );
        let edit_errors = tool_states(&servers.rust, &build_sid, "edit")
            .into_iter()
            .filter(|state| state["status"] == "error")
            .collect::<Vec<_>>();
        check(
            failures,
            "edit miss reports exact-match guidance",
            edit_errors.last().is_some_and(|state| {
                state
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .is_some_and(|message| message.contains("Could not find oldString"))
            }),
            || format!("{edit_errors:?}"),
        );

        let agent = servers
            .rust
            .post_slow(
                &format!("/api/session/{build_sid}/agent"),
                Some(&json!({"agent": "goal"})),
            )
            .expect("agent switch failed");
        check(
            failures,
            "agent switch endpoint",
            agent.status == 204,
            || agent.body.clone(),
        );
        check(
            failures,
            "agent-switched message projected",
            get_json(
                &servers.rust,
                &format!("/api/session/{build_sid}/message?order=asc"),
            )["data"]
                .as_array()
                .is_some_and(|items| {
                    items
                        .iter()
                        .any(|message| message["type"] == "agent-switched")
                }),
            || {
                get_json(
                    &servers.rust,
                    &format!("/api/session/{build_sid}/message?order=asc"),
                )
                .to_string()
            },
        );
        turn(
            servers,
            &build_sid,
            r#"Call the goal_set tool with text "Verify goal harness". Then stop."#,
        );
        check(
            failures,
            "goal tools usable after switch",
            !goal_metadata(&servers.rust, &build_sid).is_null(),
            || goal_metadata(&servers.rust, &build_sid).to_string(),
        );
        live_cross_server_byte_parity(servers, failures, "agent switching", &build_sid);
    }

    fn create_live_session(
        servers: &Servers,
        workdir: &std::path::Path,
        agent: Option<&str>,
    ) -> String {
        let mut payload = json!({
            "location": {"directory": workdir.display().to_string()},
            "model": {"id": "big-pickle", "providerID": "opencode"}
        });
        if let Some(agent) = agent {
            payload["agent"] = json!(agent);
        }
        servers
            .rust
            .post_slow("/api/session", Some(&payload))
            .expect("live session create failed")
            .json()["data"]["id"]
            .as_str()
            .expect("live session id")
            .to_string()
    }

    fn turn(servers: &Servers, sid: &str, text: &str) {
        let response = servers
            .rust
            .post_slow(
                &format!("/api/session/{sid}/prompt"),
                Some(&json!({"prompt": {"text": text}})),
            )
            .expect("live turn prompt failed");
        assert_eq!(response.status, 200, "{}", response.body);
        let wait = servers
            .rust
            .post_slow(&format!("/api/session/{sid}/wait"), None)
            .expect("live turn wait failed");
        assert_eq!(wait.status, 204, "{}", wait.body);
        assert!(wait_until_idle(
            &servers.rust,
            sid,
            Duration::from_secs(120)
        ));
    }

    fn goal_metadata(client: &Client, sid: &str) -> Value {
        get_json(client, &format!("/session/{sid}"))
            .pointer("/metadata/goal")
            .cloned()
            .unwrap_or(Value::Null)
    }

    fn tool_states(client: &Client, sid: &str, name: &str) -> Vec<Value> {
        get_json(client, &format!("/api/session/{sid}/message?order=asc"))["data"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|message| message["type"] == "assistant")
            .flat_map(|message| message["content"].as_array().cloned().unwrap_or_default())
            .filter(|part| part["type"] == "tool" && part["name"] == name)
            .filter_map(|part| part.get("state").cloned())
            .collect()
    }

    fn live_cross_server_byte_parity(
        servers: &Servers,
        failures: &mut Vec<String>,
        name: &str,
        sid: &str,
    ) {
        let same = [
            format!("/api/session/{sid}/message?order=asc"),
            format!("/api/session/{sid}/history?limit=100"),
            format!("/api/session/{sid}"),
            format!("/session/{sid}"),
        ]
        .into_iter()
        .all(|path| servers.bun.get(&path).ok() == servers.rust.get(&path).ok());
        check(
            failures,
            &format!("{name}: cross-server byte parity"),
            same,
            || sid.to_string(),
        );
    }

    fn wait_until_idle(client: &Client, sid: &str, timeout: Duration) -> bool {
        let deadline = SystemTime::now() + timeout;
        while SystemTime::now() < deadline {
            let active = get_json(client, "/api/session/active");
            if active.pointer(&format!("/data/{sid}")).is_none() {
                return true;
            }
            thread::sleep(Duration::from_millis(500));
        }
        false
    }

    fn wait_for_text(client: &Client, sid: &str, needle: &str, timeout: Duration) -> bool {
        let deadline = SystemTime::now() + timeout;
        while SystemTime::now() < deadline {
            let messages = get_json(client, &format!("/api/session/{sid}/message?order=asc"));
            if text_parts(messages["data"].as_array().unwrap_or(&Vec::new()))
                .iter()
                .any(|text| text.contains(needle))
            {
                return true;
            }
            thread::sleep(Duration::from_secs(1));
        }
        false
    }

    fn text_parts(messages: &[Value]) -> Vec<String> {
        messages
            .iter()
            .filter(|message| message["type"] == "assistant")
            .flat_map(|message| message["content"].as_array().cloned().unwrap_or_default())
            .filter(|content| content["type"] == "text")
            .filter_map(|content| content["text"].as_str().map(str::to_string))
            .collect()
    }

    fn sse_capture(client: &Client, count: usize, trigger: impl FnOnce()) -> Vec<Value> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let base = client.base.clone();
        thread::spawn(move || {
            let result = ureq::get(&format!("{base}/event"))
                .timeout(Duration::from_secs(10))
                .call();
            let _ = ready_tx.send(());
            let Ok(response) = result else {
                return;
            };
            for line in BufReader::new(response.into_reader()).lines() {
                let Ok(line) = line else {
                    return;
                };
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let Ok(event) = serde_json::from_str::<Value>(data.trim()) else {
                    continue;
                };
                let _ = event_tx.send(event);
            }
        });
        let _ = ready_rx.recv_timeout(Duration::from_secs(5));
        thread::sleep(Duration::from_millis(500));
        trigger();
        let mut events = Vec::new();
        while events.len() < count {
            match event_rx.recv_timeout(Duration::from_secs(8)) {
                Ok(event) => events.push(event),
                Err(_) => return events,
            }
        }
        events
    }

    fn normalize_event(event: &Value) -> Value {
        let mut event = event.clone();
        if let Some(object) = event.as_object_mut() {
            object.remove("id");
            if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
                properties.remove("sessionID");
                if let Some(info) = properties.get_mut("info").and_then(Value::as_object_mut) {
                    for key in ["id", "slug", "time", "title"] {
                        info.remove(key);
                    }
                }
            }
        }
        event
    }

    fn durable_row(db: &Connection, session_id: &str) -> Option<Value> {
        db.query_row(
            "SELECT type, data FROM event WHERE aggregate_id = ? ORDER BY seq DESC LIMIT 1",
            [session_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .ok()
        .map(|(kind, data)| {
            let parsed = serde_json::from_str::<Value>(&data).expect("durable event JSON");
            json!({
                "type": kind,
                "keys": parsed.as_object().map(|object| object.keys().cloned().collect::<Vec<_>>()).unwrap_or_default(),
                "info_keys_subset": parsed["info"].as_object().map(|info| {
                    info.keys()
                        .filter(|key| ["id", "slug", "projectID", "directory", "title", "version", "cost", "tokens", "time"].contains(&key.as_str()))
                        .cloned()
                        .collect::<Vec<_>>()
                }).unwrap_or_default()
            })
        })
    }

    fn get_json(client: &Client, path: &str) -> Value {
        client
            .get(path)
            .unwrap_or_else(|error| panic!("{} GET {path} failed: {error}", client.label))
            .json()
    }

    fn check_json(failures: &mut Vec<String>, name: &str, bun: &Client, rust: &Client, path: &str) {
        let bun = get_json(bun, path);
        let rust = get_json(rust, path);
        check(failures, name, bun == rust, || {
            format!("bun={bun} rust={rust}")
        });
    }

    fn check_response(
        failures: &mut Vec<String>,
        name: &str,
        bun: Result<HttpResponse, String>,
        rust: Result<HttpResponse, String>,
    ) {
        match (bun, rust) {
            (Ok(bun), Ok(rust)) => check(failures, name, bun == rust, || {
                format!("bun={bun:?} rust={rust:?}")
            }),
            (bun, rust) => record(failures, name, &format!("bun={bun:?} rust={rust:?}")),
        }
    }

    fn check(failures: &mut Vec<String>, name: &str, ok: bool, detail: impl FnOnce() -> String) {
        if ok {
            println!("PASS {name}");
            return;
        }
        record(failures, name, &detail());
    }

    fn record(failures: &mut Vec<String>, name: &str, detail: &str) {
        println!("FAIL {name} {detail}");
        failures.push(name.to_string());
    }

    fn rg_oracle(pattern: &str) -> BTreeSet<(String, i64, String)> {
        let workspace = std::env::var("OPENCODE_PARITY_WORKSPACE")
            .unwrap_or_else(|_| DEFAULT_WORKSPACE.to_string());
        let output = Command::new("rg")
            .args([
                "--no-config",
                "--json",
                "--hidden",
                "--no-messages",
                "--glob=!**/.git/**",
                "--",
                pattern,
                ".",
            ])
            .current_dir(format!("{workspace}/packages/opencode"))
            .output()
            .expect("failed to run rg oracle");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|record| record["type"] == "match")
            .filter_map(|record| {
                let data = &record["data"];
                Some((
                    data.pointer("/path/text")?
                        .as_str()?
                        .trim_start_matches("./")
                        .to_string(),
                    data["line_number"].as_i64()?,
                    data.pointer("/lines/text")?.as_str()?.to_string(),
                ))
            })
            .collect()
    }

    fn percent_encode(value: &str) -> String {
        value
            .bytes()
            .flat_map(|byte| match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    vec![byte as char]
                }
                _ => format!("%{byte:02X}").chars().collect(),
            })
            .collect()
    }

    fn remove_time_updated(value: &mut Value) {
        if let Some(time) = value.get_mut("time").and_then(Value::as_object_mut) {
            time.remove("updated");
        }
    }

    fn find_by_id<'a>(value: &'a Value, key: &str, id: &str) -> Option<&'a Value> {
        value[key]
            .as_array()?
            .iter()
            .find(|item| item["id"].as_str() == Some(id))
    }

    fn string_set(value: &Value, key: &str) -> BTreeSet<String> {
        value[key]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect()
    }

    fn id_set(value: &Value, key: &str) -> BTreeSet<String> {
        value[key]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|value| value["id"].as_str().map(str::to_string))
            .collect()
    }

    fn name_set(value: &Value) -> BTreeSet<String> {
        value
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|value| value["name"].as_str().map(str::to_string))
            .collect()
    }

    fn seqs(history: &Value) -> Vec<i64> {
        history["data"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|event| event.pointer("/durable/seq").and_then(Value::as_i64))
            .collect()
    }

    fn sorted_unique(values: Vec<i64>) -> Vec<i64> {
        values
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn event_kinds(history: &Value) -> Vec<String> {
        history["data"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|event| event["type"].as_str().map(str::to_string))
            .collect()
    }

    fn index_of(values: &[String], needle: &str) -> usize {
        values
            .iter()
            .position(|value| value == needle)
            .unwrap_or(usize::MAX)
    }

    fn count_kind(values: &[String], needle: &str) -> usize {
        values
            .iter()
            .filter(|value| value.as_str() == needle)
            .count()
    }

    fn unique_suffix() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before epoch")
            .as_millis()
    }
}
