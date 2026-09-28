use std::{env, ffi::OsString, process::ExitCode};

use clap::{CommandFactory, Parser, Subcommand};

const AFTER_HELP: &str = "\
Arguments after a command are passed to Terraform or OpenTofu unchanged.
Run with no command in an interactive terminal to open the change overview.

Examples:
  terraleph                           Open the change overview
  terraleph plan -var-file=prod.tfvars
  terraleph apply
  terraleph tofu plan
  alias terraform='terraleph terraform'";

#[derive(Parser)]
#[command(version, about, after_help = AFTER_HELP)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    #[command(
        about = "Run a Terraform command, reviewing supported interactive plan and apply runs"
    )]
    Terraform,
    #[command(
        about = "Run an OpenTofu command, reviewing supported interactive plan and apply runs"
    )]
    Tofu,
    #[command(about = "Review a Terraform plan (same as `terraleph terraform plan`)")]
    Plan,
    #[command(
        about = "Review a Terraform plan, then apply it (same as `terraleph terraform apply`)"
    )]
    Apply,
}

fn main() -> ExitCode {
    let arguments: Vec<OsString> = env::args_os().collect();
    if arguments.len() == 1
        && let Some(exit) = terraleph::run_default()
    {
        return exit;
    }
    match arguments.get(1).and_then(|arg| arg.to_str()) {
        Some("terraform") => terraleph::run_terraform(&arguments[2..]),
        Some("tofu") => terraleph::run_tofu(&arguments[2..]),
        Some("plan" | "apply") => terraleph::run_terraform(&arguments[1..]),
        _ => {
            let _ = Cli::parse_from(arguments);
            if Cli::command().print_help().is_err() {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
    }
}
