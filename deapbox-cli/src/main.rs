use std::process::ExitCode;

use deapbox_cli::{run_service, CliOptions};

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts = match CliOptions::parse(args) {
        Ok(opts) => opts,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(1);
        }
    };

    let config = match deapbox_store::load_config(&opts.config_path) {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!(
                "error: failed to load config from {}: {err}",
                opts.config_path.display()
            );
            return ExitCode::from(2);
        }
    };

    match run_service(config).await {
        Ok(()) => {
            tracing::info!("deapbox exited cleanly");
            ExitCode::SUCCESS
        }
        Err(err) => {
            tracing::error!(error = %err, "deapbox exited with error");
            eprintln!("error: {err}");
            ExitCode::from(3)
        }
    }
}
