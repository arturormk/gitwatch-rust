use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

use crate::model::RepositorySnapshot;
use crate::snapshot::collect_snapshot;

#[derive(Debug)]
pub struct RefreshResult {
    pub generation: u64,
    pub snapshot: Result<RepositorySnapshot, String>,
}

pub struct SnapshotWorker {
    requests: Option<Sender<u64>>,
    results: Receiver<RefreshResult>,
    thread: Option<JoinHandle<()>>,
}

impl SnapshotWorker {
    pub fn spawn(path: PathBuf) -> Self {
        let (request_tx, request_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("gitwatch-snapshot".to_owned())
            .spawn(move || {
                while let Ok(generation) = request_rx.recv() {
                    let snapshot =
                        collect_snapshot(&path, generation).map_err(|error| format!("{error:#}"));
                    if result_tx
                        .send(RefreshResult {
                            generation,
                            snapshot,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .expect("snapshot worker thread should start");
        Self {
            requests: Some(request_tx),
            results: result_rx,
            thread: Some(thread),
        }
    }

    pub fn request(&self, generation: u64) -> Result<(), String> {
        self.requests
            .as_ref()
            .ok_or_else(|| "snapshot worker has stopped".to_owned())?
            .send(generation)
            .map_err(|_| "snapshot worker has stopped".to_owned())
    }

    pub fn try_result(&self) -> Result<Option<RefreshResult>, String> {
        match self.results.try_recv() {
            Ok(result) => Ok(Some(result)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err("snapshot worker has stopped".to_owned()),
        }
    }
}

impl Drop for SnapshotWorker {
    fn drop(&mut self) {
        self.requests.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
