use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use crate::github::{Gh, PrMeta};
use crate::state::{self, ReviewFile};

#[derive(Debug)]
pub enum JobCommand {
    RefreshFiles,
    MarkFileViewed {
        pull_request_id: String,
        path: String,
    },
    Shutdown,
}

#[derive(Debug)]
pub enum JobEvent {
    Progress(String),
    RefreshFinished(Result<Vec<ReviewFile>, String>),
    MarkFileViewedFinished {
        path: String,
        result: Result<(), String>,
    },
}

pub struct JobWorker {
    tx: Sender<JobCommand>,
    rx: Receiver<JobEvent>,
    handle: Option<JoinHandle<()>>,
}

impl JobWorker {
    pub fn start(root: PathBuf, pr: PrMeta, gh: Gh) -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let handle = thread::spawn(move || run_worker(root, pr, gh, command_rx, event_tx));

        Self {
            tx: command_tx,
            rx: event_rx,
            handle: Some(handle),
        }
    }

    pub fn refresh_files(&self) -> Result<(), mpsc::SendError<JobCommand>> {
        self.tx.send(JobCommand::RefreshFiles)
    }

    pub fn mark_file_viewed(
        &self,
        pull_request_id: String,
        path: String,
    ) -> Result<(), mpsc::SendError<JobCommand>> {
        self.tx.send(JobCommand::MarkFileViewed {
            pull_request_id,
            path,
        })
    }

    pub fn drain_events(&self) -> Vec<JobEvent> {
        self.rx.try_iter().collect()
    }
}

impl Drop for JobWorker {
    fn drop(&mut self) {
        let _ = self.tx.send(JobCommand::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn run_worker(
    root: PathBuf,
    pr: PrMeta,
    gh: Gh,
    command_rx: Receiver<JobCommand>,
    event_tx: Sender<JobEvent>,
) {
    while let Ok(command) = command_rx.recv() {
        match command {
            JobCommand::RefreshFiles => {
                let result = state::load_review_files(&root, &pr, &gh, |message| {
                    let _ = event_tx.send(JobEvent::Progress(message));
                })
                .map_err(|error| error.to_string());
                let _ = event_tx.send(JobEvent::RefreshFinished(result));
            }
            JobCommand::MarkFileViewed {
                pull_request_id,
                path,
            } => {
                let result = gh
                    .mark_file_viewed(&pull_request_id, &path)
                    .map_err(|error| error.to_string());
                let _ = event_tx.send(JobEvent::MarkFileViewedFinished { path, result });
            }
            JobCommand::Shutdown => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_event_can_report_refresh_success() {
        let files = Vec::<ReviewFile>::new();
        let event = JobEvent::RefreshFinished(Ok(files));

        match event {
            JobEvent::RefreshFinished(Ok(files)) => assert!(files.is_empty()),
            _ => panic!("unexpected event"),
        }
    }
}
