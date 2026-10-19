use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Parser;

pub const DEFAULT_QUIET_MS: u64 = 2_000;
pub const DEFAULT_MAX_DELAY_MS: u64 = 10_000;
pub const DEFAULT_POLL_MS: u64 = 2_000;

/// Live, read-only visualization of Git working-tree changes.
#[derive(Debug, Parser)]
#[command(name = "gitwatch", version, about)]
pub struct Cli {
    /// A repository or a directory inside one
    #[arg(value_name = "PATH", default_value = ".")]
    pub path: PathBuf,

    /// Filesystem quiet period before refreshing
    #[arg(long, value_name = "MILLISECONDS", default_value_t = DEFAULT_QUIET_MS)]
    pub quiet_ms: u64,

    /// Maximum refresh delay while changes continue
    #[arg(long, value_name = "MILLISECONDS", default_value_t = DEFAULT_MAX_DELAY_MS)]
    pub max_delay_ms: u64,

    /// Git reconciliation polling interval
    #[arg(long, value_name = "MILLISECONDS", default_value_t = DEFAULT_POLL_MS)]
    pub poll_ms: u64,
}

impl Cli {
    pub fn validate(&self) -> Result<()> {
        if self.quiet_ms == 0 {
            bail!("--quiet-ms must be greater than zero");
        }
        if self.max_delay_ms == 0 {
            bail!("--max-delay-ms must be greater than zero");
        }
        if self.poll_ms == 0 {
            bail!("--poll-ms must be greater than zero");
        }
        if self.max_delay_ms < self.quiet_ms {
            bail!("--max-delay-ms must be greater than or equal to --quiet-ms");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_blueprint() {
        let cli = Cli::try_parse_from(["gitwatch"]).expect("defaults should parse");
        assert_eq!(cli.path, PathBuf::from("."));
        assert_eq!(cli.quiet_ms, 2_000);
        assert_eq!(cli.max_delay_ms, 10_000);
        assert_eq!(cli.poll_ms, 2_000);
    }

    #[test]
    fn maximum_delay_cannot_be_shorter_than_quiet_period() {
        let cli = Cli::try_parse_from(["gitwatch", "--quiet-ms", "2000", "--max-delay-ms", "1000"])
            .expect("arguments should parse");
        assert!(cli.validate().is_err());
    }
}
