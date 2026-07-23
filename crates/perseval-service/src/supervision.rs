use std::sync::Mutex;
use std::thread::JoinHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkerGroup {
    Background,
    Writer,
}

struct SupervisedWorker {
    name: &'static str,
    group: WorkerGroup,
    handle: JoinHandle<()>,
}

/// Owns every long-lived service thread and makes shutdown/join failures
/// observable. Workers are grouped because the writer must remain alive until
/// all background producers have drained.
#[derive(Default)]
pub(crate) struct WorkerSupervisor {
    workers: Mutex<Vec<SupervisedWorker>>,
    failures: Mutex<Vec<String>>,
}

impl WorkerSupervisor {
    pub(crate) fn add(&self, name: &'static str, group: WorkerGroup, handle: JoinHandle<()>) {
        self.workers
            .lock()
            .expect("worker supervisor lock poisoned")
            .push(SupervisedWorker {
                name,
                group,
                handle,
            });
    }

    pub(crate) fn join_group(&self, group: WorkerGroup) {
        let selected = {
            let mut workers = self
                .workers
                .lock()
                .expect("worker supervisor lock poisoned");
            let mut selected = Vec::new();
            let mut retained = Vec::new();
            for worker in workers.drain(..) {
                if worker.group == group {
                    selected.push(worker);
                } else {
                    retained.push(worker);
                }
            }
            *workers = retained;
            selected
        };
        for worker in selected {
            if worker.handle.join().is_err() {
                self.failures
                    .lock()
                    .expect("worker failure lock poisoned")
                    .push(format!("{} panicked", worker.name));
            }
        }
    }

    pub(crate) fn unexpected_exits(&self) -> Vec<String> {
        let mut failures = self
            .failures
            .lock()
            .expect("worker failure lock poisoned")
            .clone();
        failures.extend(
            self.workers
                .lock()
                .expect("worker supervisor lock poisoned")
                .iter()
                .filter(|worker| worker.handle.is_finished())
                .map(|worker| format!("{} exited", worker.name)),
        );
        failures
    }
}
