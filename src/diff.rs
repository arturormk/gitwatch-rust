use std::ffi::OsString;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};

use crate::git::run_read_only_status;
use crate::model::{DiffHunk, DiffRow, FileDiff};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn build_diff(old: &[u8], new: &[u8]) -> Result<FileDiff> {
    if old == new {
        return Ok(FileDiff::default());
    }

    let temporary = TemporaryDiffFiles::create(old, new)?;
    let arguments = vec![
        OsString::from("diff"),
        OsString::from("--no-index"),
        OsString::from("--no-color"),
        OsString::from("--no-prefix"),
        OsString::from("--unified=3"),
        OsString::from("--"),
        temporary.old.as_os_str().to_owned(),
        temporary.new.as_os_str().to_owned(),
    ];
    let output = run_read_only_status(temporary.directory(), arguments)?;
    if !matches!(output.status.code(), Some(0 | 1)) {
        bail!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    parse_unified_diff(&output.stdout)
}

fn parse_unified_diff(output: &[u8]) -> Result<FileDiff> {
    let lines: Vec<&[u8]> = output.split(|byte| *byte == b'\n').collect();
    let mut hunks = Vec::new();
    let mut index = 0;

    while index < lines.len() {
        if !lines[index].starts_with(b"@@ ") {
            index += 1;
            continue;
        }

        let (old_start, old_len, new_start, new_len) = parse_hunk_header(lines[index])?;
        index += 1;
        let mut old_line = old_start;
        let mut new_line = new_start;
        let mut raw_rows = Vec::new();

        while index < lines.len() && !lines[index].starts_with(b"@@ ") {
            let line = lines[index];
            if line.starts_with(b"\\ No newline at end of file") {
                index += 1;
                continue;
            }
            let Some((&prefix, content)) = line.split_first() else {
                index += 1;
                continue;
            };
            let content = String::from_utf8_lossy(content).into_owned();
            match prefix {
                b' ' => {
                    raw_rows.push(RawRow::Equal {
                        old_line,
                        new_line,
                        content,
                    });
                    old_line += 1;
                    new_line += 1;
                }
                b'-' => {
                    raw_rows.push(RawRow::Delete { old_line, content });
                    old_line += 1;
                }
                b'+' => {
                    raw_rows.push(RawRow::Insert { new_line, content });
                    new_line += 1;
                }
                _ => break,
            }
            index += 1;
        }

        hunks.push(DiffHunk {
            old_start,
            old_len,
            new_start,
            new_len,
            rows: align_change_blocks(raw_rows),
        });
    }

    Ok(FileDiff { hunks })
}

fn parse_hunk_header(line: &[u8]) -> Result<(usize, usize, usize, usize)> {
    let header = std::str::from_utf8(line).context("Git emitted a non-UTF-8 hunk header")?;
    let mut fields = header.split_ascii_whitespace();
    if fields.next() != Some("@@") {
        bail!("invalid hunk header: {header}");
    }
    let old = fields
        .next()
        .context("hunk header is missing the old range")?;
    let new = fields
        .next()
        .context("hunk header is missing the new range")?;
    let (old_start, old_len) = parse_range(old, '-')?;
    let (new_start, new_len) = parse_range(new, '+')?;
    Ok((old_start, old_len, new_start, new_len))
}

fn parse_range(value: &str, prefix: char) -> Result<(usize, usize)> {
    let value = value
        .strip_prefix(prefix)
        .with_context(|| format!("invalid hunk range {value:?}"))?;
    let (start, length) = value.split_once(',').unwrap_or((value, "1"));
    Ok((start.parse()?, length.parse()?))
}

#[derive(Debug)]
enum RawRow {
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
}

fn align_change_blocks(raw_rows: Vec<RawRow>) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    let mut pending_deletes = Vec::new();
    let mut pending_inserts = Vec::new();

    let flush = |rows: &mut Vec<DiffRow>,
                 deletes: &mut Vec<(usize, String)>,
                 inserts: &mut Vec<(usize, String)>| {
        let paired = deletes.len().min(inserts.len());
        for ((old_line, old_content), (new_line, new_content)) in
            deletes.drain(..paired).zip(inserts.drain(..paired))
        {
            rows.push(DiffRow::Replace {
                old_line,
                old_content,
                new_line,
                new_content,
            });
        }
        rows.extend(
            deletes
                .drain(..)
                .map(|(old_line, content)| DiffRow::Delete { old_line, content }),
        );
        rows.extend(
            inserts
                .drain(..)
                .map(|(new_line, content)| DiffRow::Insert { new_line, content }),
        );
    };

    for raw in raw_rows {
        match raw {
            RawRow::Equal {
                old_line,
                new_line,
                content,
            } => {
                flush(&mut rows, &mut pending_deletes, &mut pending_inserts);
                rows.push(DiffRow::Equal {
                    old_line,
                    new_line,
                    content,
                });
            }
            RawRow::Delete { old_line, content } => {
                pending_deletes.push((old_line, content));
            }
            RawRow::Insert { new_line, content } => {
                pending_inserts.push((new_line, content));
            }
        }
    }
    flush(&mut rows, &mut pending_deletes, &mut pending_inserts);
    rows
}

struct TemporaryDiffFiles {
    root: PathBuf,
    old: PathBuf,
    new: PathBuf,
}

impl TemporaryDiffFiles {
    fn create(old_content: &[u8], new_content: &[u8]) -> Result<Self> {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("gitwatch-diff-{}-{sequence}", std::process::id()));
        fs::create_dir(&root)
            .with_context(|| format!("failed to create temporary directory {}", root.display()))?;
        let old = root.join("old");
        let new = root.join("new");
        write_file(&old, old_content)?;
        write_file(&new, new_content)?;
        Ok(Self { root, old, new })
    }

    fn directory(&self) -> &Path {
        &self.root
    }
}

impl Drop for TemporaryDiffFiles {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn write_file(path: &Path, content: &[u8]) -> Result<()> {
    let mut file = File::create(path)
        .with_context(|| format!("failed to create temporary file {}", path.display()))?;
    file.write_all(content)
        .with_context(|| format!("failed to write temporary file {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_aligned_replace_insert_and_context_rows() {
        let diff = build_diff(b"one\ntwo\nthree\n", b"one\nTWO\nextra\nthree\n")
            .expect("diff should build");
        assert_eq!(diff.hunks.len(), 1);
        assert!(matches!(diff.hunks[0].rows[1], DiffRow::Replace { .. }));
        assert!(matches!(diff.hunks[0].rows[2], DiffRow::Insert { .. }));
    }

    #[test]
    fn preserves_final_newline_difference_as_a_change() {
        let diff = build_diff(b"line\n", b"line").expect("diff should build");
        assert_eq!(diff.hunks.len(), 1);
    }
}
