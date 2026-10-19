use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::model::RepositorySnapshot;

pub const MAX_DOCUMENTS: usize = 2_048;
pub const MAX_SCANNED_ENTRIES: usize = 200_000;
pub const MAX_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_TOTAL_DOCUMENT_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DocumentationSnapshot {
    pub documents: Vec<MarkdownDocument>,
    pub warnings: Vec<String>,
    pub incomplete: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownDocument {
    pub path: PathBuf,
    pub content: DocumentContent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DocumentContent {
    Text(String),
    TooLarge { size: u64, limit: usize },
    Unavailable(String),
}

pub fn collect_documents(snapshot: &RepositorySnapshot) -> Result<DocumentationSnapshot> {
    collect_from_root(
        &snapshot.repository_root,
        &[
            snapshot.git_dir.as_path(),
            snapshot.common_git_dir.as_path(),
        ],
    )
}

fn collect_from_root(root: &Path, excluded: &[&Path]) -> Result<DocumentationSnapshot> {
    let mut result = DocumentationSnapshot::default();
    let mut stack = vec![root.to_path_buf()];
    let mut total_bytes = 0usize;
    let mut scanned_entries = 0usize;

    'scan: while let Some(directory) = stack.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if directory != root => {
                result.warnings.push(format!(
                    "cannot scan {}: {error}",
                    relative_display(root, &directory)
                ));
                continue;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("cannot scan documentation in {}", root.display()));
            }
        };
        let mut paths = entries
            .filter_map(|entry| match entry {
                Ok(entry) => Some(entry.path()),
                Err(error) => {
                    result
                        .warnings
                        .push(format!("cannot inspect directory entry: {error}"));
                    None
                }
            })
            .collect::<Vec<_>>();
        paths.sort_unstable_by(|left, right| right.as_os_str().cmp(left.as_os_str()));

        for path in paths {
            scanned_entries += 1;
            if scanned_entries > MAX_SCANNED_ENTRIES {
                result.incomplete = true;
                result.warnings.push(format!(
                    "documentation scan stopped after {MAX_SCANNED_ENTRIES} filesystem entries"
                ));
                break 'scan;
            }
            if excluded.iter().any(|excluded| path == *excluded) {
                continue;
            }
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) => {
                    result.warnings.push(format!(
                        "cannot inspect {}: {error}",
                        relative_display(root, &path)
                    ));
                    continue;
                }
            };
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if !file_type.is_file() || path.extension().is_none_or(|extension| extension != "md") {
                continue;
            }
            if result.documents.len() >= MAX_DOCUMENTS {
                result.incomplete = true;
                continue;
            }

            let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            let size = metadata.len();
            let content = if size > MAX_DOCUMENT_BYTES as u64 {
                DocumentContent::TooLarge {
                    size,
                    limit: MAX_DOCUMENT_BYTES,
                }
            } else if total_bytes.saturating_add(size as usize) > MAX_TOTAL_DOCUMENT_BYTES {
                result.incomplete = true;
                DocumentContent::Unavailable(format!(
                    "not loaded because the {} byte documentation budget was reached",
                    MAX_TOTAL_DOCUMENT_BYTES
                ))
            } else {
                match fs::read(&path) {
                    Ok(bytes) => {
                        total_bytes = total_bytes.saturating_add(bytes.len());
                        if std::str::from_utf8(&bytes).is_err() {
                            result.warnings.push(format!(
                                "{} is not valid UTF-8; displaying replacement characters",
                                relative.display()
                            ));
                        }
                        DocumentContent::Text(String::from_utf8_lossy(&bytes).into_owned())
                    }
                    Err(error) => DocumentContent::Unavailable(error.to_string()),
                }
            };
            result.documents.push(MarkdownDocument {
                path: relative,
                content,
            });
        }
    }

    result
        .documents
        .sort_by(|left, right| left.path.as_os_str().cmp(right.path.as_os_str()));
    Ok(result)
}

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temporary_directory() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "gitwatch-document-test-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn discovers_hidden_and_ignored_style_directories_but_not_git_or_symlinks() {
        let root = temporary_directory();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::create_dir_all(root.join(".hidden")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("README.md"), "# Read me").unwrap();
        fs::write(root.join("docs/plan.md"), "plan").unwrap();
        fs::write(root.join(".hidden/notes.md"), "notes").unwrap();
        fs::write(root.join(".git/internal.md"), "internal").unwrap();
        fs::write(root.join("UPPER.MD"), "upper").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("docs"), root.join("linked-docs")).unwrap();

        let documents = collect_from_root(&root, &[root.join(".git").as_path()]).unwrap();
        let paths = documents
            .documents
            .iter()
            .map(|document| document.path.as_path())
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            [
                Path::new(".hidden/notes.md"),
                Path::new("README.md"),
                Path::new("docs/plan.md")
            ]
        );

        fs::remove_dir_all(root).unwrap();
    }
}
