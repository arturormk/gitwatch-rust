mod app;
mod cli;
mod highlight;
mod markdown;
mod terminal;
mod ui;

use std::io::{self, IsTerminal};

use anyhow::{Context, Result, bail};
use clap::Parser;

use crate::{cli::Cli, terminal::TerminalSession};

fn main() {
    if let Err(error) = run() {
        eprintln!("gitwatch: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    cli.validate()?;

    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("an interactive terminal is required");
    }

    let mut terminal = TerminalSession::enter().context("failed to initialize terminal")?;
    app::run(&mut terminal, &cli)
}
