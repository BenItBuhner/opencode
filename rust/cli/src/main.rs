//! OpenGoal Rust — one binary to install and run.
//!
//! Default (no subcommand): ensure the background server is alive, then open
//! the TUI. Matches OpenCode v2's daemon model (`packages/cli`):
//!
//!   opengoal-rust                 # TUI (auto-starts server)
//!   opengoal-rust serve --register
//!   opengoal-rust service start|stop|status|restart

use opencode_server::ServeOptions;
use opencode_tui::Config;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let command = args.get(1).map(String::as_str);

    let result = match command {
        None => run_tui(&args),
        Some("tui") => run_tui(&args),
        Some("serve") => run_serve(args),
        Some("service") => run_service(&args),
        Some("help") | Some("--help") | Some("-h") => {
            print_usage();
            Ok(())
        }
        Some(value) if value.starts_with('-') => run_tui(&args),
        Some(other) => {
            eprintln!("unknown command: {other}");
            print_usage();
            std::process::exit(2);
        }
    };

    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run_tui(args: &[String]) -> Result<(), String> {
    let explicit_url = arg_value(args, "--url").or_else(|| std::env::var("OPENCODE_URL").ok());
    let (base, authorization) = if let Some(url) = explicit_url {
        (
            url,
            std::env::var("OPENCODE_SERVER_PASSWORD")
                .ok()
                .map(|password| opengoal_daemon::Transport {
                    url: String::new(),
                    username: std::env::var("OPENCODE_SERVER_USERNAME")
                        .unwrap_or_else(|_| "opencode".into()),
                    password,
                })
                .map(|transport| transport.authorization()),
        )
    } else {
        let transport = opengoal_daemon::transport()?;
        (transport.url.clone(), Some(transport.authorization()))
    };

    let directory = arg_value(args, "--directory").unwrap_or_else(|| {
        std::env::current_dir()
            .expect("cwd")
            .to_string_lossy()
            .into_owned()
    });

    opencode_tui::run(Config {
        base,
        authorization,
        directory,
        session: arg_value(args, "--session"),
    })
    .map_err(|error| error.to_string())
}

fn run_serve(args: Vec<String>) -> Result<(), String> {
    let runtime = tokio::runtime::Runtime::new().map_err(|error| error.to_string())?;
    runtime.block_on(opencode_server::run(ServeOptions::from_args(args)))
}

fn run_service(args: &[String]) -> Result<(), String> {
    match args.get(2).map(String::as_str) {
        Some("start") => {
            println!("{}", opengoal_daemon::start()?);
            Ok(())
        }
        Some("status") => {
            match opengoal_daemon::status()? {
                Some(url) => println!("{url}"),
                None => {
                    eprintln!("server is not running");
                    std::process::exit(1);
                }
            }
            Ok(())
        }
        Some("stop") => opengoal_daemon::stop(),
        Some("restart") => {
            opengoal_daemon::stop()?;
            println!("{}", opengoal_daemon::start()?);
            Ok(())
        }
        Some("password") => {
            println!("{}", opengoal_daemon::load_or_create_password(None)?);
            Ok(())
        }
        _ => {
            eprintln!("usage: opengoal-rust service <start|stop|status|restart|password>");
            std::process::exit(2);
        }
    }
}

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|item| item == name)
        .and_then(|index| args.get(index + 1).cloned())
}

fn print_usage() {
    eprintln!(
        "OpenGoal Rust\n\
         \n\
         Usage:\n\
           opengoal-rust [flags]              Launch the TUI (auto-starts server)\n\
           opengoal-rust serve [--register]   Run the HTTP server\n\
           opengoal-rust service start        Start the background server\n\
           opengoal-rust service stop         Stop the background server\n\
           opengoal-rust service status       Print the registered server URL\n\
           opengoal-rust service restart      Restart the background server\n\
           opengoal-rust service password     Print the server auth password\n\
         \n\
         Flags:\n\
           --url <url>         Connect to an explicit server (skip auto-start)\n\
           --directory <path>  Working directory for new sessions\n\
           --session <id>      Open an existing session\n"
    );
}
