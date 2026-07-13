use opencode_tui::Config;

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|item| item == name)
        .and_then(|index| args.get(index + 1).cloned())
}

fn main() -> std::io::Result<()> {
    let config = Config {
        base: arg("--url")
            .or_else(|| std::env::var("OPENCODE_URL").ok())
            .unwrap_or_else(|| "http://127.0.0.1:4097".into()),
        authorization: std::env::var("OPENCODE_SERVER_PASSWORD")
            .ok()
            .map(|password| {
                let username =
                    std::env::var("OPENCODE_SERVER_USERNAME").unwrap_or_else(|_| "opencode".into());
                format!(
                    "Basic {}",
                    base64::Engine::encode(
                        &base64::engine::general_purpose::STANDARD,
                        format!("{username}:{password}")
                    )
                )
            }),
        directory: arg("--directory").unwrap_or_else(|| {
            std::env::current_dir()
                .expect("cwd")
                .to_string_lossy()
                .into_owned()
        }),
        session: arg("--session"),
    };
    opencode_tui::run(config)
}
