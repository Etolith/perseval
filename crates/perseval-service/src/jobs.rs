#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerLease {
    pub owner_id: String,
    pub expires_at: String,
}

pub(crate) struct CandidateJobWorker {
    pub sender: mpsc::SyncSender<String>,
    pub shutdown: Arc<AtomicBool>,
    pub thread: thread::JoinHandle<()>,
}

pub(crate) fn spawn_candidate_job_worker(
    store: Arc<WorkspaceStore>,
    writer: WorkspaceWriterHandle,
) -> std::io::Result<CandidateJobWorker> {
    let (sender, receiver) = mpsc::sync_channel::<String>(64);
    let shutdown = Arc::new(AtomicBool::new(false));
    let worker_shutdown = shutdown.clone();
    let thread = thread::Builder::new()
        .name("perseval-candidate-worker".into())
        .spawn(move || {
            while !worker_shutdown.load(Ordering::Acquire) {
                match receiver.recv_timeout(Duration::from_millis(100)) {
                    Ok(job_id) => {
                        let _ = writer.execute_candidate_job(job_id);
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if let Ok(pending) = store.pending_candidate_generation_job_ids() {
                            for job_id in pending {
                                if worker_shutdown.load(Ordering::Acquire) {
                                    break;
                                }
                                let _ = writer.execute_candidate_job(job_id);
                            }
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        })?;
    Ok(CandidateJobWorker {
        sender,
        shutdown,
        thread,
    })
}
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use perseval_store::WorkspaceStore;

use crate::live::WorkspaceWriterHandle;
