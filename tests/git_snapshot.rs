mod support;

use std::ffi::OsString;
use std::fs;
use std::path::Path;

use gitwatch_rust::model::{ChangeStatus, DiffRow, FileKind};
use gitwatch_rust::snapshot::{MAX_TEXT_FILE_BYTES, collect_snapshot, discover_repository};
use support::TestRepository;

#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};

#[test]
fn discovers_nested_repository_and_all_primary_change_states() {
    let repository = TestRepository::new();
    repository.write("nested/tracked.txt", b"base\n");
    repository.write("combined.txt", b"base\n");
    repository.write("deleted.txt", b"delete me\n");
    repository.write("old-name.txt", b"before rename\n");
    repository.write(".gitignore", b"ignored.log\n");
    repository.commit_all("initial");

    repository.write("nested/tracked.txt", b"changed\n");
    repository.write("combined.txt", b"staged\n");
    repository.git(["add", "combined.txt"]);
    repository.write("combined.txt", b"working\n");
    fs::remove_file(repository.join("deleted.txt")).expect("tracked file should be deleted");
    repository.git(["mv", "old-name.txt", "new-name.txt"]);
    repository.write("new-name.txt", b"after rename\n");
    repository.write("untracked.txt", b"new\n");
    repository.write("ignored.log", b"ignored\n");

    let discovered = discover_repository(&repository.join("nested"))
        .expect("repository should be discovered from a subdirectory");
    assert_eq!(discovered.root, repository.path().canonicalize().unwrap());

    let snapshot = collect_snapshot(&repository.join("nested"), 7).expect("snapshot should build");
    assert_eq!(snapshot.generation, 7);
    assert_eq!(snapshot.files.len(), 5);

    let tracked = change(&snapshot.files, "nested/tracked.txt");
    assert_eq!(tracked.status, ChangeStatus::Modified);
    assert!(!tracked.staged && tracked.unstaged);
    assert!(tracked.diff.hunks.iter().any(|hunk| {
        hunk.rows
            .iter()
            .any(|row| matches!(row, DiffRow::Replace { .. }))
    }));

    let combined = change(&snapshot.files, "combined.txt");
    assert!(combined.staged && combined.unstaged);
    assert_eq!(combined.old_content.as_ref().unwrap().text, "base\n");
    assert_eq!(combined.new_content.as_ref().unwrap().text, "working\n");

    let deleted = change(&snapshot.files, "deleted.txt");
    assert_eq!(deleted.status, ChangeStatus::Deleted);
    assert_eq!(deleted.new_content.as_ref().unwrap().text, "");

    let renamed = change(&snapshot.files, "new-name.txt");
    assert_eq!(renamed.status, ChangeStatus::Renamed);
    assert_eq!(renamed.old_path.as_deref(), Some(Path::new("old-name.txt")));
    assert_eq!(renamed.new_path.as_deref(), Some(Path::new("new-name.txt")));
    assert_eq!(
        renamed.old_content.as_ref().unwrap().text,
        "before rename\n"
    );
    assert_eq!(renamed.new_content.as_ref().unwrap().text, "after rename\n");

    assert_eq!(
        change(&snapshot.files, "untracked.txt").status,
        ChangeStatus::Untracked
    );
    assert!(
        snapshot
            .files
            .iter()
            .all(|file| file.path() != Path::new("ignored.log"))
    );

    repository.commit_all("accept changes");
    let clean = collect_snapshot(repository.path(), 8).expect("clean snapshot should build");
    assert!(clean.files.is_empty());
}

#[test]
fn supports_unborn_repositories_and_unusual_names() {
    let repository = TestRepository::new();
    repository.write("staged.txt", b"staged\n");
    repository.git(["add", "staged.txt"]);
    repository.write("tab\tnewline\n☃.txt", b"unusual\n");

    #[cfg(unix)]
    let non_utf8 = {
        let name = OsString::from_vec(b"non-utf8-\xff.txt".to_vec());
        repository.write(Path::new(&name), b"bytes\n");
        Some(name)
    };

    let snapshot = collect_snapshot(repository.path(), 1).expect("unborn snapshot should build");
    assert!(snapshot.head_identity.is_none());
    let staged = change(&snapshot.files, "staged.txt");
    assert_eq!(staged.status, ChangeStatus::Added);
    assert!(staged.staged);
    assert_eq!(staged.old_content.as_ref().unwrap().text, "");
    assert!(
        snapshot
            .files
            .iter()
            .any(|file| file.path() == Path::new("tab\tnewline\n☃.txt"))
    );

    #[cfg(unix)]
    assert!(
        snapshot
            .files
            .iter()
            .any(|file| file.path().as_os_str() == non_utf8.as_ref().unwrap())
    );
}

#[test]
fn classifies_binary_large_symlink_and_mode_only_changes() {
    let repository = TestRepository::new();
    repository.write("binary.bin", b"text\n");
    repository.write("large.txt", b"small\n");
    repository.write("mode.sh", b"#!/bin/sh\n");
    #[cfg(unix)]
    symlink("first-target", repository.join("link")).expect("symlink should be created");
    repository.commit_all("initial");

    repository.write("binary.bin", b"binary\0content");
    repository.write("large.txt", vec![b'x'; MAX_TEXT_FILE_BYTES + 1]);
    #[cfg(unix)]
    {
        fs::remove_file(repository.join("link")).unwrap();
        symlink("second-target", repository.join("link")).unwrap();
        let mut permissions = fs::metadata(repository.join("mode.sh"))
            .unwrap()
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(repository.join("mode.sh"), permissions).unwrap();
    }

    let snapshot = collect_snapshot(repository.path(), 1).expect("snapshot should build");
    assert!(matches!(
        change(&snapshot.files, "binary.bin").kind,
        FileKind::Binary { .. }
    ));
    assert!(matches!(
        change(&snapshot.files, "large.txt").kind,
        FileKind::TooLarge { .. }
    ));
    #[cfg(unix)]
    {
        assert_eq!(change(&snapshot.files, "link").kind, FileKind::Symlink);
        let mode = change(&snapshot.files, "mode.sh");
        assert!(mode.mode_changed);
        assert!(mode.diff.hunks.is_empty());
    }
}

#[test]
fn identifies_gitlink_entries_as_submodules() {
    let repository = TestRepository::new();
    repository.write("tracked.txt", b"base\n");
    repository.commit_all("initial");
    let head = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repository.path())
        .output()
        .expect("git should start");
    assert!(head.status.success());
    let head = String::from_utf8(head.stdout).unwrap();
    let cache_info = format!("160000,{},vendor/example", head.trim());
    repository.git(["update-index", "--add", "--cacheinfo", &cache_info]);

    let snapshot = collect_snapshot(repository.path(), 1).expect("snapshot should build");
    let submodule = change(&snapshot.files, "vendor/example");
    assert_eq!(submodule.kind, FileKind::Submodule);
}

#[test]
fn repeated_snapshots_do_not_change_the_index() {
    let repository = TestRepository::new();
    repository.write("tracked.txt", b"base\n");
    repository.commit_all("initial");
    repository.write("tracked.txt", b"changed\n");

    let index = repository.join(".git/index");
    let before = fs::read(&index).expect("index should exist");
    collect_snapshot(repository.path(), 1).expect("first snapshot should build");
    collect_snapshot(repository.path(), 2).expect("second snapshot should build");
    let after = fs::read(index).expect("index should still exist");
    assert_eq!(before, after);
}

#[test]
fn discovers_and_reads_a_linked_worktree() {
    let repository = TestRepository::new();
    repository.write("tracked.txt", b"base\n");
    repository.commit_all("initial");
    let linked_path = repository.path().with_extension("linked-worktree");
    let cleanup = LinkedWorktreeCleanup {
        main_repository: repository.path().to_owned(),
        linked_path: linked_path.clone(),
    };
    repository.git(vec![
        OsString::from("worktree"),
        OsString::from("add"),
        OsString::from("--quiet"),
        OsString::from("--detach"),
        linked_path.as_os_str().to_owned(),
    ]);
    fs::write(linked_path.join("tracked.txt"), b"linked change\n").unwrap();

    let discovered = discover_repository(&linked_path).expect("linked worktree should be found");
    assert_eq!(discovered.root, linked_path.canonicalize().unwrap());
    assert_eq!(
        discovered.common_git_dir,
        repository.join(".git").canonicalize().unwrap()
    );
    assert_ne!(discovered.git_dir, discovered.common_git_dir);
    let snapshot = collect_snapshot(&linked_path, 1).expect("linked snapshot should build");
    assert_eq!(
        change(&snapshot.files, "tracked.txt").status,
        ChangeStatus::Modified
    );
    drop(cleanup);
}

struct LinkedWorktreeCleanup {
    main_repository: std::path::PathBuf,
    linked_path: std::path::PathBuf,
}

impl Drop for LinkedWorktreeCleanup {
    fn drop(&mut self) {
        let _ = std::process::Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&self.linked_path)
            .current_dir(&self.main_repository)
            .output();
        let _ = fs::remove_dir_all(&self.linked_path);
    }
}

fn change<'a>(
    files: &'a [gitwatch_rust::model::FileChange],
    path: &str,
) -> &'a gitwatch_rust::model::FileChange {
    files
        .iter()
        .find(|file| file.path() == Path::new(path))
        .unwrap_or_else(|| panic!("missing change for {path:?}"))
}
