#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    if let Err(err) = deapbox_cli::run_from_args(std::env::args().skip(1)).await {
        match err {
            deapbox_cli::CliError::Help(message) => {
                println!("{message}");
            }
            other => {
                eprintln!("{other}");
                std::process::exit(1);
            }
        }
    }
}
