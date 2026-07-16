use std::sync::mpsc;
use std::thread;

use gpui::{AppContext, Task};

/// Starts the synchronous operation on its own worker immediately, then waits
/// for its result on GPUI's background executor. This prevents a saturated
/// executor from delaying the service mutation itself.
pub(crate) fn run<R, F>(
    name: &'static str,
    operation: F,
    cx: &impl AppContext,
) -> Task<Result<R, String>>
where
    R: Send + 'static,
    F: FnOnce() -> R + Send + 'static,
{
    let (sender, receiver) = mpsc::sync_channel(1);
    match thread::Builder::new().name(name.into()).spawn(move || {
        let _ = sender.send(operation());
    }) {
        Ok(_) => cx.background_spawn(async move {
            receiver
                .recv()
                .map_err(|_| format!("{name} ended without a result"))
        }),
        Err(error) => {
            cx.background_spawn(async move { Err(format!("could not start {name}: {error}")) })
        }
    }
}
