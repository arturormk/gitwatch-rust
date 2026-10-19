use std::{
    ffi::OsString,
    io::{self, Write},
    path::Path,
    process::Command,
};

use anyhow::{Context, Result};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

pub struct TerminalSession {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl TerminalSession {
    pub fn enter() -> Result<Self> {
        enable_raw_mode()?;

        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, EnableMouseCapture) {
            let _ = execute!(stdout, DisableMouseCapture, LeaveAlternateScreen);
            let _ = disable_raw_mode();
            return Err(error.into());
        }

        match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(mut terminal) => {
                if let Err(error) = terminal.clear() {
                    let mut stdout = io::stdout();
                    let _ = execute!(stdout, DisableMouseCapture, LeaveAlternateScreen);
                    let _ = disable_raw_mode();
                    return Err(error.into());
                }
                Ok(Self { terminal })
            }
            Err(error) => {
                let mut stdout = io::stdout();
                let _ = execute!(stdout, DisableMouseCapture, LeaveAlternateScreen);
                let _ = disable_raw_mode();
                Err(error.into())
            }
        }
    }

    pub fn terminal_mut(&mut self) -> &mut Terminal<CrosstermBackend<io::Stdout>> {
        &mut self.terminal
    }

    pub fn run_shell(&mut self, working_directory: &Path) -> Result<()> {
        self.restore()
            .context("failed to suspend the terminal interface")?;

        let shell = std::env::var_os("SHELL")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| OsString::from("/bin/sh"));
        let shell_result = Command::new(&shell)
            .current_dir(working_directory)
            .status()
            .with_context(|| format!("failed to start shell {}", shell.to_string_lossy()));
        let resume_result = self.reenter();

        shell_result?;
        resume_result.context("failed to restore the terminal interface")
    }

    fn reenter(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        if let Err(error) = execute!(
            self.terminal.backend_mut(),
            EnterAlternateScreen,
            EnableMouseCapture
        ) {
            let _ = execute!(
                self.terminal.backend_mut(),
                DisableMouseCapture,
                LeaveAlternateScreen
            );
            let _ = disable_raw_mode();
            return Err(error);
        }
        self.terminal.clear()
    }

    fn restore(&mut self) -> io::Result<()> {
        // Attempt every restoration step even if an earlier one fails. Leaving
        // the alternate screen and restoring the cursor are more important than
        // returning the first cleanup error early.
        let raw_result = disable_raw_mode();
        let mouse_result = execute!(self.terminal.backend_mut(), DisableMouseCapture);
        let screen_result = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let cursor_result = self.terminal.show_cursor();
        let flush_result = self.terminal.backend_mut().flush();

        raw_result?;
        mouse_result?;
        screen_result?;
        cursor_result?;
        flush_result
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}
