mod support;

use std::fs;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use gitwatch_rust::document::DocumentContent;
use gitwatch_rust::snapshot::collect_snapshot;
use gitwatch_rust::watcher::{RepositoryWatcher, WatchEvent, WatchSource};
use gitwatch_rust::worker::{RefreshResult, SnapshotWorker};
use support::TestRepository;

#[test]
fn watcher_and_worker_follow_atomic_save_and_commit() {
    let repository = TestRepository::new();
    repository.write("tracked.txt", b"original\n");
    repository.commit_all("initial");

    let initial = collect_snapshot(repository.path(), 1).expect("initial snapshot should build");
    assert!(initial.files.is_empty());
    let (_watcher, events) = RepositoryWatcher::start(&initial, Duration::from_millis(100))
        .expect("repository watcher should start");
    let worker = SnapshotWorker::spawn(repository.path().to_path_buf());

    repository.write("tracked.txt.tmp", b"changed\n");
    fs::rename(
        repository.join("tracked.txt.tmp"),
        repository.join("tracked.txt"),
    )
    .expect("atomic replacement should succeed");
    wait_for_change(&events);

    worker.request(2).expect("refresh should be queued");
    let changed = wait_for_result(&worker);
    let changed = changed.snapshot.expect("changed snapshot should build");
    assert_eq!(changed.generation, 2);
    assert_eq!(changed.files.len(), 1);

    while events.try_recv().is_ok() {}
    repository.write("tracked.txt", b"changed again\n");
    wait_for_source(&events, WatchSource::Polling);

    worker.request(3).expect("poll refresh should be queued");
    let polled = wait_for_result(&worker)
        .snapshot
        .expect("polled snapshot should build");
    assert_eq!(polled.generation, 3);
    assert_eq!(polled.files.len(), 1);

    while events.try_recv().is_ok() {}
    repository.commit_all("accept change");
    wait_for_change(&events);

    worker.request(4).expect("clean refresh should be queued");
    let clean = wait_for_result(&worker);
    let clean = clean.snapshot.expect("clean snapshot should build");
    assert_eq!(clean.generation, 4);
    assert!(clean.files.is_empty());
}

#[test]
fn snapshot_worker_recovers_after_a_collection_error() {
    let repository = TestRepository::new();
    repository.write("tracked.txt", b"content\n");
    repository.commit_all("initial");
    let worker = SnapshotWorker::spawn(repository.path().to_path_buf());
    let git_dir = repository.join(".git");
    let hidden_git_dir = repository.join(".git-temporarily-hidden");

    fs::rename(&git_dir, &hidden_git_dir).expect("Git directory should be hidden");
    worker.request(1).expect("failing refresh should be queued");
    assert!(wait_for_result(&worker).snapshot.is_err());

    fs::rename(&hidden_git_dir, &git_dir).expect("Git directory should be restored");
    worker
        .request(2)
        .expect("recovery refresh should be queued");
    let recovered = wait_for_result(&worker)
        .snapshot
        .expect("worker should recover on its next request");
    assert_eq!(recovered.generation, 2);
    assert!(recovered.files.is_empty());
}

#[test]
fn worker_reloads_markdown_after_an_on_disk_edit() {
    let repository = TestRepository::new();
    repository.write("README.md", b"# Before\n");
    repository.commit_all("initial documentation");

    let initial = collect_snapshot(repository.path(), 1).expect("initial snapshot should build");
    let (_watcher, events) = RepositoryWatcher::start(&initial, Duration::from_millis(100))
        .expect("repository watcher should start");
    let worker = SnapshotWorker::spawn(repository.path().to_path_buf());

    repository.write("README.md", b"# After\n\nUpdated on disk.\n");
    wait_for_change(&events);
    worker
        .request(2)
        .expect("documentation refresh should queue");

    let refreshed = wait_for_result(&worker)
        .documentation
        .expect("documentation snapshot should build");
    let readme = refreshed
        .documents
        .iter()
        .find(|document| document.path == std::path::Path::new("README.md"))
        .expect("README should remain in the documentation inventory");
    assert_eq!(
        readme.content,
        DocumentContent::Text("# After\n\nUpdated on disk.\n".into())
    );
}

fn wait_for_change(events: &Receiver<WatchEvent>) {
    wait_for_event(events, |_| true);
}

fn wait_for_source(events: &Receiver<WatchEvent>, wanted: WatchSource) {
    wait_for_event(events, |source| source == wanted);
}

fn wait_for_event(events: &Receiver<WatchEvent>, accepts: impl Fn(WatchSource) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for filesystem change"
        );
        match events.recv_timeout(remaining) {
            Ok(WatchEvent::Change(source)) if accepts(source) => return,
            Ok(WatchEvent::Change(_)) => {}
            Ok(WatchEvent::Error(_)) => {}
            Err(RecvTimeoutError::Timeout) => panic!("timed out waiting for filesystem change"),
            Err(RecvTimeoutError::Disconnected) => panic!("filesystem watcher stopped"),
        }
    }
}

fn wait_for_result(worker: &SnapshotWorker) -> RefreshResult {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match worker.try_result() {
            Ok(Some(result)) => return result,
            Ok(None) => {}
            Err(error) => panic!("{error}"),
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for snapshot worker"
        );
        thread::sleep(Duration::from_millis(10));
    }
}
