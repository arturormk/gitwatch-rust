use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub struct TestRepository {
    root: PathBuf,
}

impl TestRepository {
    pub fn new() -> Self {
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "gitwatch-repository-test-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("temporary repository directory should be created");
        let repository = Self { root };
        repository.git(["init", "--quiet"]);
        repository.git(["config", "user.name", "Gitwatch Tests"]);
        repository.git(["config", "user.email", "gitwatch@example.invalid"]);
        repository
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    pub fn join(&self, path: impl AsRef<Path>) -> PathBuf {
        self.root.join(path)
    }

    pub fn write(&self, path: impl AsRef<Path>, content: impl AsRef<[u8]>) {
        let path = self.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent directories should be created");
        }
        fs::write(path, content).expect("fixture file should be written");
    }

    pub fn git<I, S>(&self, arguments: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(&self.root)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("git should start");
        assert!(
            output.status.success(),
            "fixture git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    pub fn commit_all(&self, message: &str) {
        self.git(["add", "--all"]);
        self.git(["commit", "--quiet", "-m", message]);
    }
}

impl Drop for TestRepository {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
