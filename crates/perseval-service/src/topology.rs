use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use perseval_store::WorkspaceStore;

use crate::live::WorkspaceWriterHandle;

pub(crate) fn spawn_topology_worker(
    store: Arc<WorkspaceStore>,
    writer: WorkspaceWriterHandle,
    chunk_rows: usize,
    shutting_down: Arc<AtomicBool>,
) -> std::io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("perseval-topology-projection".into())
        .spawn(move || {
            while !shutting_down.load(Ordering::Acquire) {
                let job = match writer.claim_topology() {
                    Ok(Some(job)) => job,
                    Ok(None) => {
                        thread::sleep(Duration::from_millis(50));
                        continue;
                    }
                    Err(_) => {
                        thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                };
                let rows = match store.build_topology_projection(&job) {
                    Ok(rows) => rows,
                    Err(error) => {
                        let _ = writer.fail_topology(job, error.to_string());
                        thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                };
                if rows.is_empty() {
                    if let Err(error) =
                        writer.commit_topology_chunk(job.clone(), Vec::new(), true, true)
                    {
                        let _ = writer.fail_topology(job, error);
                    }
                    continue;
                }
                let chunk_count = rows.len().div_ceil(chunk_rows);
                let mut failed = None;
                for (index, chunk) in rows.chunks(chunk_rows).enumerate() {
                    if let Err(error) = writer.commit_topology_chunk(
                        job.clone(),
                        chunk.to_vec(),
                        index == 0,
                        index + 1 == chunk_count,
                    ) {
                        failed = Some(error);
                        break;
                    }
                }
                if let Some(error) = failed {
                    let _ = writer.fail_topology(job, error);
                    thread::sleep(Duration::from_millis(100));
                }
            }
        })
}
