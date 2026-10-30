use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use std::{
    io::{self, Read},
    path::PathBuf,
};

#[derive(Debug, Parser)]
#[command(
    about = "Opt-in Lens report transport. Never collects host data or executes remote commands."
)]
struct Args {
    #[arg(long, default_value = "/var/lib/lens-publisher")]
    state_dir: PathBuf,
    #[command(subcommand)]
    command: Action,
}
#[derive(Debug, Subcommand)]
enum Action {
    /// Register using the existing organisation code, read from standard input (never argv).
    Register {
        #[arg(long, default_value = "https://app-api.dataplicity.com")]
        server_url: String,
        #[arg(long)]
        name: String,
        /// Explicitly repair existing credentials using the org code; keeps host identity.
        #[arg(long)]
        refresh: bool,
    },
    /// Show registration status without exposing credentials.
    Status,
    /// Start the publisher in the foreground. Installation does not invoke this command.
    Run {
        #[arg(long, default_value = "/run/lens-report/observations")]
        pipe: PathBuf,
    },
}
fn main() -> Result<()> {
    let args = Args::parse();
    lens_reporting::require_unprivileged_linux()?;
    match args.command {
        Action::Register {
            server_url,
            name,
            refresh,
        } => {
            // Pipe the code or redirect a private file. Bounded input, no credentials in process args.
            let mut code = String::new();
            io::stdin().take(4097).read_to_string(&mut code)?;
            if code.len() > 4096 {
                bail!("organisation code input too large");
            }
            lens_reporting::publisher::register(&args.state_dir, &server_url, &name, &code, refresh)
        }
        Action::Status => lens_reporting::publisher::status(&args.state_dir),
        Action::Run { pipe } => lens_reporting::publisher::run(args.state_dir, pipe),
    }
}
