use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() || args.iter().any(|item| item == "--help" || item == "-h") {
        usage();
        return;
    }
    let base = std::env::var("OPENCODE_URL").unwrap_or_else(|_| "http://127.0.0.1:4097".into());
    let request = match request_for(&args) {
        Some(request) => request,
        None => {
            usage();
            std::process::exit(2);
        }
    };
    match send(&base, &request) {
        Ok(body) => print_body(&body),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

fn usage() {
    eprintln!(
        "usage: opencode-client <command>\n\
         commands:\n\
           health | config | providers | path | vcs | vcs-status | vcs-diff [git|branch]\n\
           sessions | session <id> | messages <session-id> | message <session-id> <message-id>\n\
           projects | project-current | file-list <path> | file-content <path> | find <pattern> | find-file <query>\n\
         v2 commands (/api):\n\
           v2-health | active | v2-sessions | v2-session <id> | history <id>\n\
           create [agent]                durably create a session\n\
           prompt <id> <text...>         durably admit a prompt (admit-only; no model execution)\n\
           queue <id> <text...>          admit with queue delivery"
    );
}

struct Request {
    method: &'static str,
    path: String,
    body: Option<Value>,
}

impl Request {
    fn get(path: String) -> Request {
        Request {
            method: "GET",
            path,
            body: None,
        }
    }

    fn post(path: String, body: Value) -> Request {
        Request {
            method: "POST",
            path,
            body: Some(body),
        }
    }
}

fn request_for(args: &[String]) -> Option<Request> {
    Some(match args.first()?.as_str() {
        "health" => Request::get("/global/health".into()),
        "config" => Request::get("/config".into()),
        "providers" => Request::get("/provider".into()),
        "path" => Request::get("/path".into()),
        "vcs" => Request::get("/vcs".into()),
        "vcs-status" => Request::get("/vcs/status".into()),
        "vcs-diff" => Request::get(format!(
            "/vcs/diff?mode={}",
            encode(args.get(1).map(String::as_str).unwrap_or("git"))
        )),
        "sessions" => Request::get("/session".into()),
        "session" => Request::get(format!("/session/{}", encode(args.get(1)?))),
        "messages" => Request::get(format!("/session/{}/message", encode(args.get(1)?))),
        "message" => Request::get(format!(
            "/session/{}/message/{}",
            encode(args.get(1)?),
            encode(args.get(2)?)
        )),
        "projects" => Request::get("/project".into()),
        "project-current" => Request::get("/project/current".into()),
        "file-list" => Request::get(format!("/file?path={}", encode(args.get(1)?))),
        "file-content" => Request::get(format!("/file/content?path={}", encode(args.get(1)?))),
        "find" => Request::get(format!("/find?pattern={}", encode(args.get(1)?))),
        "find-file" => Request::get(format!("/find/file?query={}", encode(args.get(1)?))),
        "v2-health" => Request::get("/api/health".into()),
        "active" => Request::get("/api/session/active".into()),
        "v2-sessions" => Request::get("/api/session".into()),
        "v2-session" => Request::get(format!("/api/session/{}", encode(args.get(1)?))),
        "history" => Request::get(format!("/api/session/{}/history", encode(args.get(1)?))),
        "create" => Request::post(
            "/api/session".into(),
            match args.get(1) {
                Some(agent) => json!({ "agent": agent }),
                None => json!({}),
            },
        ),
        // Admission is durable but admit-only from this client: the message
        // becomes visible to whichever process owns the Session drain.
        "prompt" => Request::post(
            format!("/api/session/{}/prompt", encode(args.get(1)?)),
            json!({ "prompt": { "text": args.get(2..)?.join(" ") }, "resume": false }),
        ),
        "queue" => Request::post(
            format!("/api/session/{}/prompt", encode(args.get(1)?)),
            json!({ "prompt": { "text": args.get(2..)?.join(" ") }, "delivery": "queue", "resume": false }),
        ),
        _ => return None,
    })
}

fn send(base: &str, request: &Request) -> std::io::Result<String> {
    let (host, port) = parse_base(base)?;
    let mut stream = TcpStream::connect((host.as_str(), port))?;
    let body = request
        .body
        .as_ref()
        .map(Value::to_string)
        .unwrap_or_default();
    write!(
        stream,
        "{} {} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nAccept: application/json\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        request.method,
        request.path,
        body.len(),
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let (head, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| std::io::Error::other("bad HTTP response"))?;
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);
    if !(200..300).contains(&status) {
        return Err(std::io::Error::other(format!(
            "{}: {body}",
            head.lines().next().unwrap_or("request failed")
        )));
    }
    Ok(body.to_string())
}

fn parse_base(base: &str) -> std::io::Result<(String, u16)> {
    let trimmed = base.strip_prefix("http://").unwrap_or(base);
    let (host, port) = trimmed
        .split_once(':')
        .ok_or_else(|| std::io::Error::other("OPENCODE_URL must look like http://host:port"))?;
    Ok((host.to_string(), port.parse().unwrap_or(80)))
}

fn print_body(body: &str) {
    match serde_json::from_str::<Value>(body) {
        Ok(value) => println!("{}", serde_json::to_string_pretty(&value).expect("json")),
        Err(_) => print!("{body}"),
    }
}

fn encode(input: &str) -> String {
    input
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                vec![byte as char]
            }
            _ => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}
