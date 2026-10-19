use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::Result;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::model::RepositorySnapshot;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WatchSource {
    Filesystem,
    Polling,
}

#[derive(Debug)]
pub enum WatchEvent {
    Change(WatchSource),
    Error(String),
}

#[derive(Clone)]
struct EventFilter {
    root: PathBuf,
    git_dir: PathBuf,
    common_git_dir: PathBuf,
}

pub struct RepositoryWatcher {
    _native: Option<RecommendedWatcher>,
    _polling: MetadataPoller,
}

impl RepositoryWatcher {
    pub fn start(
        snapshot: &RepositorySnapshot,
        poll_interval: Duration,
    ) -> Result<(Self, Receiver<WatchEvent>)> {
        let filter = EventFilter {
            root: snapshot.repository_root.clone(),
            git_dir: snapshot.git_dir.clone(),
            common_git_dir: snapshot.common_git_dir.clone(),
        };
        let (sender, receiver) = mpsc::channel();

        let polling = MetadataPoller::start(filter.clone(), poll_interval, sender.clone())?;

        let native_handler = event_handler(sender.clone(), filter.clone(), WatchSource::Filesystem);
        let native = match notify::recommended_watcher(native_handler) {
            Ok(mut watcher) => match add_watches(&mut watcher, &filter) {
                Ok(()) => Some(watcher),
                Err(error) => {
                    let _ = sender.send(WatchEvent::Error(format!(
                        "native filesystem watcher unavailable: {error}"
                    )));
                    None
                }
            },
            Err(error) => {
                let _ = sender.send(WatchEvent::Error(format!(
                    "native filesystem watcher unavailable: {error}"
                )));
                None
            }
        };

        Ok((
            Self {
                _native: native,
                _polling: polling,
            },
            receiver,
        ))
    }
}

struct MetadataPoller {
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl MetadataPoller {
    fn start(filter: EventFilter, interval: Duration, sender: Sender<WatchEvent>) -> Result<Self> {
        let mut fingerprint = repository_fingerprint(&filter)?;
        let (stop_tx, stop_rx) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("gitwatch-poll".to_owned())
            .spawn(move || {
                while let Err(RecvTimeoutError::Timeout) = stop_rx.recv_timeout(interval) {
                    match repository_fingerprint(&filter) {
                        Ok(next) if next != fingerprint => {
                            fingerprint = next;
                            if sender
                                .send(WatchEvent::Change(WatchSource::Polling))
                                .is_err()
                            {
                                break;
                            }
                        }
                        Ok(_) => {}
                        Err(error) => {
                            if sender
                                .send(WatchEvent::Error(format!(
                                    "filesystem polling error: {error}"
                                )))
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
            })?;
        Ok(Self {
            stop: Some(stop_tx),
            thread: Some(thread),
        })
    }
}

impl Drop for MetadataPoller {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn repository_fingerprint(filter: &EventFilter) -> io::Result<u64> {
    let mut hasher = DefaultHasher::new();
    hash_tree(&filter.root, filter, true, &mut hasher)?;
    for path in [
        filter.git_dir.join("HEAD"),
        filter.git_dir.join("index"),
        filter.common_git_dir.join("HEAD"),
        filter.common_git_dir.join("packed-refs"),
        filter.common_git_dir.join("refs"),
    ] {
        hash_tree(&path, filter, false, &mut hasher)?;
    }
    Ok(hasher.finish())
}

fn hash_tree(
    path: &Path,
    filter: &EventFilter,
    working_tree: bool,
    hasher: &mut DefaultHasher,
) -> io::Result<()> {
    if working_tree
        && (path == filter.git_dir || path == filter.common_git_dir || temporary_path(path))
    {
        return Ok(());
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    path.hash(hasher);
    metadata.file_type().is_dir().hash(hasher);
    metadata.file_type().is_file().hash(hasher);
    metadata.file_type().is_symlink().hash(hasher);

    if metadata.is_dir() {
        let mut children = match fs::read_dir(path) {
            Ok(entries) => entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .collect::<Vec<_>>(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        children.sort_unstable();
        for child in children {
            hash_tree(&child, filter, working_tree, hasher)?;
        }
    } else {
        metadata.len().hash(hasher);
        metadata.modified()?.hash(hasher);
        metadata.permissions().readonly().hash(hasher);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode().hash(hasher);
        }
    }
    Ok(())
}

fn event_handler(
    sender: Sender<WatchEvent>,
    filter: EventFilter,
    source: WatchSource,
) -> impl FnMut(notify::Result<Event>) {
    move |result| match result {
        Ok(event) if relevant_event(&event, &filter) => {
            let _ = sender.send(WatchEvent::Change(source));
        }
        Ok(_) => {}
        Err(error) => {
            let _ = sender.send(WatchEvent::Error(format!(
                "filesystem watcher error: {error}"
            )));
        }
    }
}

fn add_watches<W: Watcher>(watcher: &mut W, filter: &EventFilter) -> notify::Result<()> {
    watcher.watch(&filter.root, RecursiveMode::Recursive)?;

    if !filter.git_dir.starts_with(&filter.root) {
        watcher.watch(&filter.git_dir, RecursiveMode::NonRecursive)?;
    }
    if filter.common_git_dir != filter.git_dir && !filter.common_git_dir.starts_with(&filter.root) {
        watcher.watch(&filter.common_git_dir, RecursiveMode::NonRecursive)?;
    }

    let refs = filter.common_git_dir.join("refs");
    if refs.is_dir() && !refs.starts_with(&filter.root) {
        watcher.watch(&refs, RecursiveMode::Recursive)?;
    }
    Ok(())
}

fn relevant_event(event: &Event, filter: &EventFilter) -> bool {
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    event.paths.is_empty() || event.paths.iter().any(|path| relevant_path(path, filter))
}

fn relevant_path(path: &Path, filter: &EventFilter) -> bool {
    if metadata_path(path, filter) {
        return true;
    }
    path.starts_with(&filter.root)
        && !path.starts_with(&filter.git_dir)
        && !path.starts_with(&filter.common_git_dir)
        && !temporary_path(path)
}

fn metadata_path(path: &Path, filter: &EventFilter) -> bool {
    path == filter.git_dir.join("HEAD")
        || path == filter.git_dir.join("index")
        || path == filter.common_git_dir.join("HEAD")
        || path == filter.common_git_dir.join("packed-refs")
        || path.starts_with(filter.common_git_dir.join("refs"))
}

fn temporary_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.ends_with('~')
        || name.ends_with(".swp")
        || name.ends_with(".swx")
        || name.ends_with(".tmp")
        || name.starts_with(".#")
        || (name.starts_with('#') && name.ends_with('#'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter() -> EventFilter {
        EventFilter {
            root: PathBuf::from("/repo"),
            git_dir: PathBuf::from("/repo/.git"),
            common_git_dir: PathBuf::from("/repo/.git"),
        }
    }

    #[test]
    fn filters_git_noise_and_editor_temporaries() {
        let filter = filter();
        assert!(relevant_path(Path::new("/repo/src/main.rs"), &filter));
        assert!(!relevant_path(Path::new("/repo/src/.main.rs.swp"), &filter));
        assert!(!relevant_path(
            Path::new("/repo/.git/objects/aa/bb"),
            &filter
        ));
        assert!(relevant_path(Path::new("/repo/.git/index"), &filter));
        assert!(relevant_path(
            Path::new("/repo/.git/refs/heads/main"),
            &filter
        ));
    }

    #[test]
    fn linked_worktree_metadata_is_relevant() {
        let filter = EventFilter {
            root: PathBuf::from("/worktree"),
            git_dir: PathBuf::from("/repo/.git/worktrees/worktree"),
            common_git_dir: PathBuf::from("/repo/.git"),
        };
        assert!(relevant_path(
            Path::new("/repo/.git/worktrees/worktree/index"),
            &filter
        ));
        assert!(relevant_path(
            Path::new("/repo/.git/refs/heads/main"),
            &filter
        ));
    }
}
