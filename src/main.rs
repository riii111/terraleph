use std::{env, ffi::OsString, path::PathBuf, process::ExitCode};

use clap::{CommandFactory, Parser, Subcommand, error::ErrorKind};
use terraleph::EnvironmentTargets;

const AFTER_HELP: &str = "\
Arguments after a command are passed to Terraform or OpenTofu unchanged.
Run with no command in an interactive terminal to open the change overview.
Terraleph options go before the command.

Examples:
  terraleph                           Open the change overview
  terraleph plan -var-file=prod.tfvars
  terraleph --env-dir envs/prod --env-dir envs/stg
  terraleph --max-depth 6
  terraleph apply
  terraleph tofu plan
  alias terraform='terraleph terraform'";

#[derive(Parser)]
#[command(version, about, after_help = AFTER_HELP)]
struct Cli {
    #[arg(
        long = "env-dir",
        value_name = "DIR",
        help = "Use this directory as an environment (repeatable)"
    )]
    env_dirs: Vec<PathBuf>,
    #[arg(
        long,
        value_name = "LEVELS",
        help = "Directory levels to search for environments [default: 4]"
    )]
    max_depth: Option<usize>,
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
    let (targets, consumed) = match EnvironmentTargets::parse_leading(&arguments[1..]) {
        Ok(parsed) => parsed,
        Err(message) => {
            let _ = Cli::command()
                .error(ErrorKind::ValueValidation, message)
                .print();
            return ExitCode::from(2);
        }
    };
    let command = &arguments[1 + consumed..];
    if command.is_empty()
        && let Some(exit) = terraleph::run_default(&targets)
    {
        return exit;
    }
    match command.first().and_then(|arg| arg.to_str()) {
        Some("terraform") => terraleph::run_terraform(&targets, &command[1..]),
        Some("tofu") => terraleph::run_tofu(&targets, &command[1..]),
        Some("plan" | "apply") => terraleph::run_terraform(&targets, command),
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
