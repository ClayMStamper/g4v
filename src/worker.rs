//! Background job runner. Every git operation runs off the UI thread so the
//! interface never freezes, and jobs for one repository run strictly one at a
//! time so they can never trip over each other's `index.lock`.

use crate::git::Repo;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

type Job<T> = Box<dyn FnOnce(&Repo) -> T + Send + 'static>;

pub struct Worker<T: Send + 'static> {
    tx: Option<Sender<(String, Job<T>)>>,
    rx: Receiver<T>,
    errors: Receiver<String>,
    pending: Arc<AtomicUsize>,
    current: Arc<Mutex<Option<String>>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl<T: Send + 'static> Worker<T> {
    pub fn new(repo: Repo, name: &str, repaint: impl Fn() + Send + 'static) -> Self {
        let (tx, job_rx) = channel::<(String, Job<T>)>();
        let (res_tx, rx) = channel::<T>();
        let (error_tx, errors) = channel();
        let pending = Arc::new(AtomicUsize::new(0));
        let current = Arc::new(Mutex::new(None));
        let p = pending.clone();
        let cur = current.clone();
        let handle = std::thread::Builder::new()
            .name(format!("g4v-{name}"))
            .spawn(move || {
                while let Ok((label, job)) = job_rx.recv() {
                    if let Ok(mut c) = cur.lock() {
                        *c = Some(label.clone());
                    }
                    repaint();
                    // A panicking job must never take the worker (and the UI's view of the repo) down.
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&repo)));
                    if let Ok(mut c) = cur.lock() {
                        *c = None;
                    }
                    // Send first, then report idle: "not busy" then always means "every result has been delivered".
                    let delivered = match result {
                        Ok(r) => res_tx.send(r).is_ok(),
                        Err(_) => error_tx.send(format!("{label} stopped unexpectedly. Refresh the project and check its state before retrying; backup shelves are available in Shelves.")).is_ok(),
                    };
                    p.fetch_sub(1, Ordering::SeqCst);
                    if !delivered {
                        break;
                    }
                    repaint();
                }
            })
            .expect("failed to spawn worker thread");
        Worker { tx: Some(tx), rx, errors, pending, current, handle: Some(handle) }
    }

    pub fn submit(&self, label: impl Into<String>, job: impl FnOnce(&Repo) -> T + Send + 'static) {
        if let Some(tx) = &self.tx {
            self.pending.fetch_add(1, Ordering::SeqCst);
            if tx.send((label.into(), Box::new(job))).is_err() {
                self.pending.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }

    pub fn poll(&self) -> Vec<T> {
        self.rx.try_iter().collect()
    }

    pub fn poll_errors(&self) -> Vec<String> {
        self.errors.try_iter().collect()
    }

    pub fn busy(&self) -> bool {
        self.pending.load(Ordering::SeqCst) > 0
    }

    pub fn queued(&self) -> usize {
        self.pending.load(Ordering::SeqCst)
    }

    pub fn current(&self) -> Option<String> {
        self.current.lock().ok().and_then(|c| c.clone())
    }
}

impl<T: Send + 'static> Drop for Worker<T> {
    fn drop(&mut self) {
        // Closing the channel lets the thread exit once the current job is done.
        self.tx.take();
        if let Some(h) = self.handle.take() {
            // Don't block the UI on a long network operation; detach instead.
            if h.is_finished() {
                let _ = h.join();
            }
        }
    }
}
