use opencode_server::ServeOptions;

#[tokio::main]
async fn main() {
    let options = ServeOptions::from_args(std::env::args().collect());
    if let Err(error) = opencode_server::run(options).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
