use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, FromArgMatches, Parser};

use crate::{CommitOptions, create_commit};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Create a Git commit whose abbreviated ID is stored in the commit itself"
)]
struct Cli {
    /// Commit message. May be supplied multiple times to create paragraphs.
    #[arg(short = 'm', long = "message", action = clap::ArgAction::Append)]
    messages: Vec<String>,

    /// Read the commit message from a file. Use '-' for standard input.
    #[arg(
        short = 'F',
        long = "file",
        value_name = "PATH",
        conflicts_with = "messages"
    )]
    message_file: Option<PathBuf>,

    /// Repository-relative file that receives the mined prefix.
    #[arg(long = "sha-file", default_value = ".shasha", value_name = "PATH")]
    sha_file: PathBuf,

    /// Number of hexadecimal prefix characters to mine.
    #[arg(long, default_value_t = 5, value_name = "N")]
    length: u8,

    /// Number of mining threads. Defaults to the available parallelism.
    #[arg(long, value_name = "N")]
    threads: Option<usize>,
}

pub fn main(command_name: &'static str) -> ExitCode {
    match run(command_name) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(command_name: &'static str) -> Result<()> {
    let matches = Cli::command()
        .name(command_name)
        .bin_name(command_name)
        .get_matches();
    let cli = Cli::from_arg_matches(&matches).expect("clap should validate command-line arguments");
    let message = read_message(&cli)?;
    let threads = cli.threads.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
    });
    let current_dir = std::env::current_dir().context("could not determine current directory")?;

    let outcome = create_commit(
        &current_dir,
        &CommitOptions {
            message: message.clone(),
            version_file: cli.sha_file,
            prefix_len: cli.length,
            threads,
        },
    )?;

    let reference = outcome
        .reference
        .strip_prefix("refs/heads/")
        .unwrap_or(&outcome.reference);
    let subject = message.lines().next().unwrap_or("");
    let rate = if outcome.elapsed.is_zero() {
        0.0
    } else {
        outcome.attempts as f64 / outcome.elapsed.as_secs_f64()
    };

    println!("[{reference} {}] {subject}", outcome.prefix);
    println!(
        "mined {} candidates in {} ({:.1} MH/s)",
        outcome.attempts,
        format_duration(outcome.elapsed),
        rate / 1_000_000.0
    );
    println!(
        "{} contains {}; full commit is {}",
        outcome.version_file.display(),
        outcome.prefix,
        outcome.oid
    );
    Ok(())
}

fn read_message(cli: &Cli) -> Result<String> {
    if !cli.messages.is_empty() {
        return Ok(cli.messages.join("\n\n"));
    }

    match cli.message_file.as_deref() {
        Some(path) if path == std::path::Path::new("-") => {
            let mut message = String::new();
            io::stdin()
                .read_to_string(&mut message)
                .context("could not read the commit message from standard input")?;
            Ok(message)
        }
        Some(path) => fs::read_to_string(path)
            .with_context(|| format!("could not read commit message from {}", path.display())),
        None => bail!("provide a commit message with -m or -F"),
    }
}

fn format_duration(duration: std::time::Duration) -> String {
    let seconds = duration.as_secs_f64();
    if seconds < 0.001 {
        format!("{:.3}ms", seconds * 1_000.0)
    } else if seconds < 1.0 {
        format!("{:.1}ms", seconds * 1_000.0)
    } else {
        format!("{seconds:.3}s")
    }
}

#[cfg(test)]
mod tests {
    use super::format_duration;
    use std::time::Duration;

    #[test]
    fn formats_short_mining_durations_readably() {
        assert_eq!(format_duration(Duration::from_micros(464)), "0.464ms");
        assert_eq!(format_duration(Duration::from_millis(12)), "12.0ms");
        assert_eq!(format_duration(Duration::from_millis(1234)), "1.234s");
    }
}
