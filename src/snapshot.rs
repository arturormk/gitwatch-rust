use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};

use crate::diff::build_diff;
use crate::git::{run_read_only, run_read_only_status};
use crate::model::{
    ChangeStatus, FileChange, FileDiff, FileKind, RepositorySnapshot, SnapshotWarning, TextContent,
};

#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};

pub const MAX_TEXT_FILE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Repository {
    pub root: PathBuf,
    pub git_dir: PathBuf,
    pub common_git_dir: PathBuf,
    pub head_identity: Option<String>,
}

pub fn discover_repository(requested_path: &Path) -> Result<Repository> {
    let requested_path = requested_path
        .canonicalize()
        .with_context(|| format!("cannot access {}", requested_path.display()))?;
    if !requested_path.is_dir() {
        bail!("{} is not a directory", requested_path.display());
    }

    let bare = git_text(&requested_path, ["rev-parse", "--is-bare-repository"])
        .context("the requested path is not inside a Git repository")?;
    if bare == "true" {
        bail!("bare repositories do not have a working tree");
    }

    let root = git_path(&requested_path, ["rev-parse", "--show-toplevel"])?;
    let git_dir = git_path(&requested_path, ["rev-parse", "--absolute-git-dir"])?;
    let common_git_dir = git_path(
        &requested_path,
        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;

    let head_output = run_read_only_status(&root, ["rev-parse", "--verify", "HEAD"])?;
    let head_identity = if head_output.status.success() {
        Some(trim_ascii(&head_output.stdout)?.to_owned())
    } else {
        None
    };

    Ok(Repository {
        root,
        git_dir,
        common_git_dir,
        head_identity,
    })
}

pub fn collect_snapshot(requested_path: &Path, generation: u64) -> Result<RepositorySnapshot> {
    let repository = discover_repository(requested_path)?;
    let output = run_read_only(
        &repository.root,
        [
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
    )?;
    let entries = parse_status(&output.stdout)?;
    let mut warnings = Vec::new();
    let mut files = Vec::with_capacity(entries.len());

    for entry in entries {
        files.push(assemble_change(&repository, entry, &mut warnings));
    }
    files.sort_by(|left, right| path_bytes(&left.key).cmp(path_bytes(&right.key)));

    Ok(RepositorySnapshot {
        generation,
        repository_root: repository.root,
        git_dir: repository.git_dir,
        common_git_dir: repository.common_git_dir,
        head_identity: repository.head_identity,
        collected_at: SystemTime::now(),
        files,
        warnings,
    })
}

fn assemble_change(
    repository: &Repository,
    entry: StatusEntry,
    warnings: &mut Vec<SnapshotWarning>,
) -> FileChange {
    let key = entry
        .new_path
        .clone()
        .or(entry.old_path.clone())
        .unwrap_or_default();
    let mut status = entry.status;
    let old_mode = nonzero_mode(entry.head_mode);
    let mut new_mode = nonzero_mode(entry.worktree_mode).or(nonzero_mode(entry.index_mode));
    let is_submodule = old_mode == Some(0o160000)
        || new_mode == Some(0o160000)
        || entry.index_mode == 0o160000
        || entry.submodule.first().is_some_and(|byte| *byte == b'S');

    if is_submodule {
        return FileChange {
            key,
            old_path: entry.old_path,
            new_path: entry.new_path,
            status,
            staged: entry.staged,
            unstaged: entry.unstaged,
            kind: FileKind::Submodule,
            old_content: None,
            new_content: None,
            old_mode,
            new_mode,
            mode_changed: old_mode != new_mode,
            diff: FileDiff::default(),
        };
    }

    let old_data = match entry.head_oid.as_deref() {
        Some(oid) if !is_zero_oid(oid) => read_blob(repository, oid),
        _ => Ok(Data::Bytes(Vec::new())),
    };
    let new_data = match entry.new_path.as_deref() {
        Some(path) => read_worktree_path(&repository.root, path),
        None => Ok(ObservedData {
            data: Data::Bytes(Vec::new()),
            mode: None,
            changed_during_read: false,
        }),
    };

    if let Ok(observed) = &new_data {
        new_mode = observed.mode.or(new_mode);
        if observed.changed_during_read {
            warnings.push(SnapshotWarning {
                path: entry.new_path.clone(),
                message: "file changed while the snapshot was being collected".to_owned(),
            });
        }
    }
    let mode_changed = old_mode != new_mode;
    if mode_changed && status == ChangeStatus::Modified && entry.xy.contains(&b'T') {
        status = ChangeStatus::TypeChanged;
    }

    let classification = match (old_data, new_data) {
        (Ok(old), Ok(observed)) => classify_and_diff(old, observed.data, old_mode, new_mode),
        (Err(error), _) | (_, Err(error)) => Err(error),
    };
    let (kind, old_content, new_content, diff) = match classification {
        Ok(classification) => classification,
        Err(error) => {
            let message = format!("{error:#}");
            warnings.push(SnapshotWarning {
                path: Some(key.clone()),
                message: message.clone(),
            });
            (
                FileKind::Unavailable { message },
                None,
                None,
                FileDiff::default(),
            )
        }
    };

    FileChange {
        key,
        old_path: entry.old_path,
        new_path: entry.new_path,
        status,
        staged: entry.staged,
        unstaged: entry.unstaged,
        kind,
        old_content,
        new_content,
        old_mode,
        new_mode,
        mode_changed,
        diff,
    }
}

fn classify_and_diff(
    old: Data,
    new: Data,
    old_mode: Option<u32>,
    new_mode: Option<u32>,
) -> Result<(FileKind, Option<TextContent>, Option<TextContent>, FileDiff)> {
    let old_size = old.size();
    let new_size = new.size();
    if old.is_too_large() || new.is_too_large() {
        return Ok((
            FileKind::TooLarge {
                limit: MAX_TEXT_FILE_BYTES,
                old_size,
                new_size,
            },
            None,
            None,
            FileDiff::default(),
        ));
    }

    let (Data::Bytes(old), Data::Bytes(new)) = (old, new) else {
        unreachable!("large content was handled above")
    };
    if looks_binary(&old) || looks_binary(&new) {
        return Ok((
            FileKind::Binary { old_size, new_size },
            None,
            None,
            FileDiff::default(),
        ));
    }

    let symlink = old_mode == Some(0o120000) || new_mode == Some(0o120000);
    let old_content = TextContent {
        text: String::from_utf8_lossy(&old).into_owned(),
        has_final_newline: old.last() == Some(&b'\n'),
    };
    let new_content = TextContent {
        text: String::from_utf8_lossy(&new).into_owned(),
        has_final_newline: new.last() == Some(&b'\n'),
    };
    let diff = build_diff(&old, &new)?;
    Ok((
        if symlink {
            FileKind::Symlink
        } else {
            FileKind::Text
        },
        Some(old_content),
        Some(new_content),
        diff,
    ))
}

fn read_blob(repository: &Repository, oid: &[u8]) -> Result<Data> {
    validate_oid(oid)?;
    let oid = OsString::from(String::from_utf8(oid.to_vec())?);
    let size_output = run_read_only(
        &repository.root,
        [
            OsString::from("cat-file"),
            OsString::from("-s"),
            oid.clone(),
        ],
    )?;
    let size: usize = trim_ascii(&size_output.stdout)?.parse()?;
    if size > MAX_TEXT_FILE_BYTES {
        return Ok(Data::TooLarge(size));
    }
    let output = run_read_only(
        &repository.root,
        [OsString::from("cat-file"), OsString::from("blob"), oid],
    )?;
    Ok(Data::Bytes(output.stdout))
}

fn read_worktree_path(root: &Path, relative: &Path) -> Result<ObservedData> {
    validate_relative_path(relative)?;
    let path = root.join(relative);
    let before = fs::symlink_metadata(&path)
        .with_context(|| format!("cannot inspect {}", path.display()))?;
    let file_type = before.file_type();
    let mode = filesystem_mode(&before);
    if file_type.is_dir() {
        return Ok(ObservedData {
            data: Data::Bytes(Vec::new()),
            mode,
            changed_during_read: false,
        });
    }

    let size = before.len() as usize;
    if size > MAX_TEXT_FILE_BYTES {
        return Ok(ObservedData {
            data: Data::TooLarge(size),
            mode,
            changed_during_read: false,
        });
    }
    let bytes = if file_type.is_symlink() {
        os_bytes(fs::read_link(&path)?.as_os_str()).to_vec()
    } else {
        fs::read(&path).with_context(|| format!("cannot read {}", path.display()))?
    };
    let after = fs::symlink_metadata(&path)
        .with_context(|| format!("cannot recheck {}", path.display()))?;
    let changed_during_read = before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
        || before.file_type() != after.file_type();
    Ok(ObservedData {
        data: Data::Bytes(bytes),
        mode,
        changed_during_read,
    })
}

fn validate_relative_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("Git reported an unsafe worktree path: {}", path.display());
    }
    Ok(())
}

#[cfg(unix)]
fn filesystem_mode(metadata: &fs::Metadata) -> Option<u32> {
    if metadata.file_type().is_symlink() {
        Some(0o120000)
    } else if metadata.file_type().is_file() {
        let executable = metadata.permissions().mode() & 0o111 != 0;
        Some(if executable { 0o100755 } else { 0o100644 })
    } else if metadata.file_type().is_dir() {
        Some(0o040000)
    } else if metadata.file_type().is_block_device()
        || metadata.file_type().is_char_device()
        || metadata.file_type().is_fifo()
        || metadata.file_type().is_socket()
    {
        Some(metadata.mode() & 0o170000)
    } else {
        None
    }
}

#[cfg(not(unix))]
fn filesystem_mode(metadata: &fs::Metadata) -> Option<u32> {
    if metadata.file_type().is_symlink() {
        Some(0o120000)
    } else if metadata.file_type().is_file() {
        Some(0o100644)
    } else if metadata.file_type().is_dir() {
        Some(0o040000)
    } else {
        None
    }
}

fn looks_binary(content: &[u8]) -> bool {
    content.iter().take(8 * 1024).any(|byte| *byte == 0)
}

#[derive(Debug)]
enum Data {
    Bytes(Vec<u8>),
    TooLarge(usize),
}

impl Data {
    fn size(&self) -> usize {
        match self {
            Self::Bytes(bytes) => bytes.len(),
            Self::TooLarge(size) => *size,
        }
    }

    fn is_too_large(&self) -> bool {
        matches!(self, Self::TooLarge(_))
    }
}

struct ObservedData {
    data: Data,
    mode: Option<u32>,
    changed_during_read: bool,
}

#[derive(Debug)]
struct StatusEntry {
    xy: [u8; 2],
    submodule: Vec<u8>,
    head_mode: u32,
    index_mode: u32,
    worktree_mode: u32,
    head_oid: Option<Vec<u8>>,
    old_path: Option<PathBuf>,
    new_path: Option<PathBuf>,
    status: ChangeStatus,
    staged: bool,
    unstaged: bool,
}

fn parse_status(input: &[u8]) -> Result<Vec<StatusEntry>> {
    let records: Vec<&[u8]> = input.split(|byte| *byte == 0).collect();
    let mut entries = Vec::new();
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        index += 1;
        if record.is_empty() {
            continue;
        }
        match record[0] {
            b'1' => entries.push(parse_ordinary(record)?),
            b'2' => {
                let old_path = records
                    .get(index)
                    .context("truncated porcelain-v2 rename record")?;
                index += 1;
                entries.push(parse_rename(record, old_path)?);
            }
            b'u' => entries.push(parse_unmerged(record)?),
            b'?' => {
                let path = record
                    .strip_prefix(b"? ")
                    .context("invalid porcelain-v2 untracked record")?;
                if path.is_empty() {
                    bail!("porcelain-v2 untracked record has an empty path");
                }
                entries.push(untracked_entry(path));
            }
            b'!' => {}
            b'#' => {}
            other => bail!("unsupported porcelain-v2 record type {other:?}"),
        }
    }
    Ok(entries)
}

fn parse_ordinary(record: &[u8]) -> Result<StatusEntry> {
    let fields: Vec<&[u8]> = record.splitn(9, |byte| *byte == b' ').collect();
    if fields.len() != 9 || fields[0] != b"1" {
        bail!("invalid porcelain-v2 ordinary record");
    }
    if fields[8].is_empty() {
        bail!("porcelain-v2 ordinary record has an empty path");
    }
    status_entry(
        fields[1],
        fields[2],
        fields[3],
        fields[4],
        fields[5],
        fields[6],
        None,
        Some(fields[8]),
        false,
    )
}

fn parse_rename(record: &[u8], old_path: &[u8]) -> Result<StatusEntry> {
    let fields: Vec<&[u8]> = record.splitn(10, |byte| *byte == b' ').collect();
    if fields.len() != 10 || fields[0] != b"2" {
        bail!("invalid porcelain-v2 rename record");
    }
    if fields[9].is_empty() || old_path.is_empty() {
        bail!("porcelain-v2 rename record has an empty path");
    }
    status_entry(
        fields[1],
        fields[2],
        fields[3],
        fields[4],
        fields[5],
        fields[6],
        Some(old_path),
        Some(fields[9]),
        true,
    )
}

fn parse_unmerged(record: &[u8]) -> Result<StatusEntry> {
    let fields: Vec<&[u8]> = record.splitn(11, |byte| *byte == b' ').collect();
    if fields.len() != 11 || fields[0] != b"u" {
        bail!("invalid porcelain-v2 unmerged record");
    }
    if fields[10].is_empty() {
        bail!("porcelain-v2 unmerged record has an empty path");
    }
    let mut entry = status_entry(
        fields[1],
        fields[2],
        fields[3],
        fields[4],
        fields[6],
        fields[7],
        None,
        Some(fields[10]),
        false,
    )?;
    entry.status = ChangeStatus::Conflicted;
    Ok(entry)
}

#[allow(clippy::too_many_arguments)]
fn status_entry(
    xy: &[u8],
    submodule: &[u8],
    head_mode: &[u8],
    index_mode: &[u8],
    worktree_mode: &[u8],
    head_oid: &[u8],
    old_path: Option<&[u8]>,
    new_path: Option<&[u8]>,
    renamed: bool,
) -> Result<StatusEntry> {
    if xy.len() != 2 {
        bail!("invalid porcelain-v2 XY field");
    }
    let xy = [xy[0], xy[1]];
    let staged = xy[0] != b'.';
    let unstaged = xy[1] != b'.';
    let status = status_from_xy(xy, renamed);
    let supplied_old_path = old_path.map(path_from_bytes);
    let supplied_new_path = new_path.map(path_from_bytes);
    let (old_path, new_path) = match status {
        ChangeStatus::Added => (None, supplied_new_path),
        ChangeStatus::Deleted => (supplied_new_path.or(supplied_old_path), None),
        ChangeStatus::Renamed => (supplied_old_path, supplied_new_path),
        _ => (
            supplied_old_path.or_else(|| supplied_new_path.clone()),
            supplied_new_path,
        ),
    };
    Ok(StatusEntry {
        xy,
        submodule: submodule.to_vec(),
        head_mode: parse_mode(head_mode)?,
        index_mode: parse_mode(index_mode)?,
        worktree_mode: parse_mode(worktree_mode)?,
        head_oid: Some(head_oid.to_vec()),
        old_path,
        new_path,
        status,
        staged,
        unstaged,
    })
}

fn untracked_entry(path: &[u8]) -> StatusEntry {
    StatusEntry {
        xy: *b"??",
        submodule: b"N...".to_vec(),
        head_mode: 0,
        index_mode: 0,
        worktree_mode: 0,
        head_oid: None,
        old_path: None,
        new_path: Some(path_from_bytes(path)),
        status: ChangeStatus::Untracked,
        staged: false,
        unstaged: true,
    }
}

fn status_from_xy(xy: [u8; 2], renamed: bool) -> ChangeStatus {
    if renamed || xy.contains(&b'R') {
        ChangeStatus::Renamed
    } else if xy.contains(&b'U') {
        ChangeStatus::Conflicted
    } else if xy.contains(&b'A') || xy.contains(&b'C') {
        ChangeStatus::Added
    } else if xy.contains(&b'D') {
        ChangeStatus::Deleted
    } else if xy.contains(&b'T') {
        ChangeStatus::TypeChanged
    } else {
        ChangeStatus::Modified
    }
}

fn parse_mode(value: &[u8]) -> Result<u32> {
    Ok(u32::from_str_radix(std::str::from_utf8(value)?, 8)?)
}

fn nonzero_mode(mode: u32) -> Option<u32> {
    (mode != 0).then_some(mode)
}

fn validate_oid(oid: &[u8]) -> Result<()> {
    if !matches!(oid.len(), 40 | 64) || !oid.iter().all(u8::is_ascii_hexdigit) {
        bail!("Git reported an invalid object ID");
    }
    Ok(())
}

fn is_zero_oid(oid: &[u8]) -> bool {
    !oid.is_empty() && oid.iter().all(|byte| *byte == b'0')
}

fn git_text<I, S>(directory: &Path, arguments: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = run_read_only(directory, arguments)?;
    Ok(trim_ascii(&output.stdout)?.to_owned())
}

fn git_path<I, S>(directory: &Path, arguments: I) -> Result<PathBuf>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = run_read_only(directory, arguments)?;
    let bytes = trim_one_line_ending(&output.stdout);
    if bytes.is_empty() {
        bail!("Git returned an empty path");
    }
    Ok(PathBuf::from(os_string(bytes)))
}

fn trim_ascii(bytes: &[u8]) -> Result<&str> {
    Ok(std::str::from_utf8(trim_one_line_ending(bytes))?.trim())
}

fn trim_one_line_ending(mut bytes: &[u8]) -> &[u8] {
    if bytes.ends_with(b"\n") {
        bytes = &bytes[..bytes.len() - 1];
    }
    if bytes.ends_with(b"\r") {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

#[cfg(unix)]
fn os_string(bytes: &[u8]) -> OsString {
    OsString::from_vec(bytes.to_vec())
}

#[cfg(not(unix))]
fn os_string(bytes: &[u8]) -> OsString {
    OsString::from(String::from_utf8_lossy(bytes).into_owned())
}

fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(os_string(bytes))
}

#[cfg(unix)]
fn os_bytes(value: &OsStr) -> &[u8] {
    value.as_bytes()
}

#[cfg(not(unix))]
fn os_bytes(value: &OsStr) -> &[u8] {
    value.to_str().unwrap_or_default().as_bytes()
}

fn path_bytes(path: &Path) -> &[u8] {
    os_bytes(path.as_os_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ordinary_rename_untracked_and_unmerged_records() {
        let input = b"1 .M N... 100644 100644 100644 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb file name\0\
2 R. N... 100644 100644 100644 cccccccccccccccccccccccccccccccccccccccc dddddddddddddddddddddddddddddddddddddddd R100 new\0old\0\
? untracked\0\
u UU N... 100644 100644 100644 100644 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb cccccccccccccccccccccccccccccccccccccccc conflict\0";
        let entries = parse_status(input).expect("status should parse");
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].new_path.as_deref(), Some(Path::new("file name")));
        assert_eq!(entries[1].old_path.as_deref(), Some(Path::new("old")));
        assert_eq!(entries[1].new_path.as_deref(), Some(Path::new("new")));
        assert_eq!(entries[2].status, ChangeStatus::Untracked);
        assert_eq!(entries[3].status, ChangeStatus::Conflicted);
    }

    #[test]
    fn rejects_truncated_rename_record() {
        let input = b"2 R. N... 100644 100644 100644 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb R100 new\0";
        assert!(parse_status(input).is_err());
    }
}
