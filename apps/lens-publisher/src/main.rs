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
    /// Register using a setup code read from standard input.
    Register {
        #[arg(long, default_value = "https://app-api.dataplicity.com")]
        server_url: String,
        #[arg(long)]
        name: String,
        /// Explicitly refresh credentials or pair the existing host; keeps host identity.
        #[arg(long)]
        refresh: bool,
    },
    /// Show registration status without exposing credentials.
    Status,
    /// Change local syslog forwarding while the publisher is stopped.
    Configure {
        #[arg(long)]
        no_syslog_upload: bool,
        #[arg(long, default_value = "local setup")]
        reason: String,
    },
    /// Print private rsyslog configuration to standard output for local installation.
    SyslogConfig {
        #[arg(long)]
        ca_file: String,
        /// DNS hostname of this deployment's TLS syslog collector.
        #[arg(long)]
        host: Option<String>,
        /// TLS port, default 6514.
        #[arg(long, requires = "host")]
        port: Option<u16>,
    },
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
        Action::Configure {
            no_syslog_upload,
            reason,
        } => {
            lens_reporting::publisher::configure_syslog(&args.state_dir, !no_syslog_upload, &reason)
        }
        Action::SyslogConfig {
            ca_file,
            host,
            port,
        } => {
            print!(
                "{}",
                lens_reporting::publisher::syslog_config(
                    &args.state_dir,
                    &ca_file,
                    host.as_deref(),
                    port,
                )?
            );
            Ok(())
        }
        Action::Status => lens_reporting::publisher::status(&args.state_dir),
        Action::Run { pipe } => lens_reporting::publisher::run(args.state_dir, pipe),
    }
}
