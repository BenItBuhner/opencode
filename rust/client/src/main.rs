use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpStream;

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() || args.iter().any(|item| item == "--help" || item == "-h") {
        usage();
        return;
    }
    let base = std::env::var("OPENCODE_URL").unwrap_or_else(|_| "http://127.0.0.1:4097".into());
    let path = match path_for(&args) {
        Some(path) => path,
        None => {
            usage();
            std::process::exit(2);
        }
    };
    match get(&base, &path) {
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
           projects | project-current | file-list <path> | file-content <path> | find <pattern> | find-file <query>"
    );
}

fn path_for(args: &[String]) -> Option<String> {
    Some(match args.first()?.as_str() {
        "health" => "/global/health".into(),
        "config" => "/config".into(),
        "providers" => "/provider".into(),
        "path" => "/path".into(),
        "vcs" => "/vcs".into(),
        "vcs-status" => "/vcs/status".into(),
        "vcs-diff" => format!(
            "/vcs/diff?mode={}",
            encode(args.get(1).map(String::as_str).unwrap_or("git"))
        ),
        "sessions" => "/session".into(),
        "session" => format!("/session/{}", encode(args.get(1)?)),
        "messages" => format!("/session/{}/message", encode(args.get(1)?)),
        "message" => format!(
            "/session/{}/message/{}",
            encode(args.get(1)?),
            encode(args.get(2)?)
        ),
        "projects" => "/project".into(),
        "project-current" => "/project/current".into(),
        "file-list" => format!("/file?path={}", encode(args.get(1)?)),
        "file-content" => format!("/file/content?path={}", encode(args.get(1)?)),
        "find" => format!("/find?pattern={}", encode(args.get(1)?)),
        "find-file" => format!("/find/file?query={}", encode(args.get(1)?)),
        _ => return None,
    })
}

fn get(base: &str, path: &str) -> std::io::Result<String> {
    let (host, port) = parse_base(base)?;
    let mut stream = TcpStream::connect((host.as_str(), port))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nAccept: application/json\r\n\r\n"
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let (head, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| std::io::Error::other("bad HTTP response"))?;
    if !head.starts_with("HTTP/1.1 200") && !head.starts_with("HTTP/1.0 200") {
        return Err(std::io::Error::other(
            head.lines().next().unwrap_or("request failed").to_string(),
        ));
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
