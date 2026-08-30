//! A single background thread that runs backend jobs one at a time.
//!
//! DDC/CI over i2c is inherently a one-transaction-at-a-time bus — that's
//! why `ddcutil` itself takes an `flock()` on the i2c device before talking
//! to it. Before this existed, `commands::dispatch` spawned a bare OS
//! thread per backend call, so a cache-hit probe (see `spawn_probe`) could
//! fire a dozen-plus concurrent `ddcutil getvcp` calls at once — all fighting
//! over the same i2c device's flock, several timing out, and on this
//! project's own hardware, wedging the GPU's display pipe badly enough to
//! freeze the whole machine (`amdgpu: flip_done timed out`, unrecoverable).
//!
//! `Worker` fixes that at the root instead of papering over it with a
//! mutex sprinkled through `commands.rs`: every `Cmd` still gets *described*
//! and dispatched exactly as before, but the actual I/O closure is handed
//! to this one thread's queue instead of `std::thread::spawn`, so at most
//! one `ddcutil` process (or native backend call, once one exists) ever
//! touches the bus at a time. The render/event loop in `main` stays
//! non-blocking either way — `submit` only queues the job.

use std::sync::mpsc;
use std::thread;

type Job = Box<dyn FnOnce() + Send + 'static>;

pub struct Worker {
    job_tx: mpsc::Sender<Job>,
}

impl Worker {
    /// Spawns the single worker thread. Its receiver end simply outlives
    /// every `Worker` clone via the channel; the thread exits on its own
    /// once every sender (including this one) is dropped.
    pub fn new() -> Self {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        thread::spawn(move || {
            for job in job_rx {
                job();
            }
        });
        Worker { job_tx }
    }

    /// Queues `job` to run on the worker thread, after every job already
    /// queued ahead of it. Never blocks the caller.
    pub fn submit(&self, job: Job) {
        // The receiver only goes away when the Worker (and every clone of
        // its sender) is dropped, which for this app's lifetime means
        // "never while `main`'s loop is still running" — so a send error
        // here would only happen during shutdown, and is safe to ignore.
        let _ = self.job_tx.send(job);
    }
}

impl Clone for Worker {
    fn clone(&self) -> Self {
        Worker {
            job_tx: self.job_tx.clone(),
        }
    }
}
