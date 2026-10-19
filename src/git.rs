use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::{Command, Output, Stdio};

use anyhow::{Context, Result, bail};

const MAX_CAPTURE_BYTES: usize = 16 * 1024 * 1024;

/// Runs an audited, read-only Git query with prompting, pagers, and optional
/// index locks disabled. Callers still choose from the allowlist below.
pub fn run_read_only<I, S>(working_directory: &Path, arguments: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = run_read_only_status(working_directory, arguments)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git query failed: {}", stderr.trim());
    }
    Ok(output)
}

pub fn run_read_only_status<I, S>(working_directory: &Path, arguments: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut arguments: Vec<OsString> = arguments
        .into_iter()
        .map(|argument| argument.as_ref().to_owned())
        .collect();
    ensure_allowed(&arguments)?;
    apply_safety_arguments(&mut arguments);

    let output = Command::new("git")
        .arg("--no-optional-locks")
        .args(&arguments)
        .current_dir(working_directory)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("failed to start git")?;

    if output.stdout.len() > MAX_CAPTURE_BYTES || output.stderr.len() > MAX_CAPTURE_BYTES {
        bail!(
            "git output exceeded the {} MiB safety limit",
            MAX_CAPTURE_BYTES / 1024 / 1024
        );
    }
    Ok(output)
}

fn apply_safety_arguments(arguments: &mut Vec<OsString>) {
    if arguments.first().is_some_and(|command| command == "diff") {
        arguments.insert(1, OsString::from("--no-ext-diff"));
        arguments.insert(2, OsString::from("--no-textconv"));
    }
}

fn ensure_allowed(arguments: &[OsString]) -> Result<()> {
    let Some(command) = arguments.first().and_then(|argument| argument.to_str()) else {
        bail!("missing or non-UTF-8 Git subcommand");
    };

    match command {
        "rev-parse" | "status" | "cat-file" | "ls-tree" | "diff" => {}
        _ => bail!("Git subcommand {command:?} is not approved for read-only use"),
    }

    for argument in &arguments[1..] {
        let argument = argument.to_string_lossy();
        let unsafe_option = argument == "--ext-diff"
            || argument == "--textconv"
            || argument == "--filters"
            || argument == "--output"
            || argument.starts_with("--output=");
        if unsafe_option {
            bail!("Git option {argument:?} is not approved for read-only use");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_mutating_commands() {
        let error = ensure_allowed(&[OsString::from("add"), OsString::from(".")])
            .expect_err("git add must be rejected");
        assert!(error.to_string().contains("not approved"));
    }

    #[test]
    fn permits_snapshot_queries() {
        for command in ["rev-parse", "status", "cat-file", "ls-tree", "diff"] {
            ensure_allowed(&[OsString::from(command)]).expect("query should be allowed");
        }
    }

    #[test]
    fn diff_always_disables_external_helpers() {
        let mut arguments = vec![OsString::from("diff"), OsString::from("--raw")];
        apply_safety_arguments(&mut arguments);
        assert_eq!(
            arguments,
            ["diff", "--no-ext-diff", "--no-textconv", "--raw"].map(OsString::from)
        );
    }

    #[test]
    fn rejects_git_options_that_can_write_or_run_filters() {
        for arguments in [
            vec![OsString::from("diff"), OsString::from("--output=result")],
            vec![OsString::from("diff"), OsString::from("--ext-diff")],
            vec![OsString::from("cat-file"), OsString::from("--filters")],
        ] {
            assert!(ensure_allowed(&arguments).is_err());
        }
    }
}
