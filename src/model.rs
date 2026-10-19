use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositorySnapshot {
    pub generation: u64,
    pub repository_root: PathBuf,
    pub git_dir: PathBuf,
    pub common_git_dir: PathBuf,
    pub head_identity: Option<String>,
    pub collected_at: SystemTime,
    pub files: Vec<FileChange>,
    pub warnings: Vec<SnapshotWarning>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotWarning {
    pub path: Option<PathBuf>,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileChange {
    pub key: PathBuf,
    pub old_path: Option<PathBuf>,
    pub new_path: Option<PathBuf>,
    pub status: ChangeStatus,
    pub staged: bool,
    pub unstaged: bool,
    pub kind: FileKind,
    pub old_content: Option<TextContent>,
    pub new_content: Option<TextContent>,
    pub old_mode: Option<u32>,
    pub new_mode: Option<u32>,
    pub mode_changed: bool,
    pub diff: FileDiff,
}

impl FileChange {
    pub fn path(&self) -> &Path {
        self.new_path
            .as_ref()
            .or(self.old_path.as_ref())
            .unwrap_or(&self.key)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChangeStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    TypeChanged,
    Untracked,
    Conflicted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileKind {
    Text,
    Binary {
        old_size: usize,
        new_size: usize,
    },
    TooLarge {
        limit: usize,
        old_size: usize,
        new_size: usize,
    },
    Submodule,
    Symlink,
    Unavailable {
        message: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextContent {
    pub text: String,
    pub has_final_newline: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FileDiff {
    pub hunks: Vec<DiffHunk>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffHunk {
    pub old_start: usize,
    pub old_len: usize,
    pub new_start: usize,
    pub new_len: usize,
    pub rows: Vec<DiffRow>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiffRow {
    Equal {
        old_line: usize,
        new_line: usize,
        content: String,
    },
    Delete {
        old_line: usize,
        content: String,
    },
    Insert {
        new_line: usize,
        content: String,
    },
    Replace {
        old_line: usize,
        old_content: String,
        new_line: usize,
        new_content: String,
    },
}
