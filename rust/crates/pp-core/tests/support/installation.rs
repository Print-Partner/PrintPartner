//! Included by the backup and restore test binaries.

#[path = "installation_response.rs"]
mod response;
#[path = "installation_server.rs"]
mod server;

pub use server::Server;
use std::sync::Arc;

impl Server {
    /// Holds the owner lock on another thread until [`Held::release`], so a
    /// request that needs the owner waits.
    pub fn hold_owner(&self) -> Held {
        let (release, released) = std::sync::mpsc::channel();
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let owner = Arc::clone(&self.owner);
        let thread = std::thread::spawn(move || {
            let guard = owner.lock().unwrap();
            held_tx.send(()).unwrap();
            let _ = released.recv();
            drop(guard);
        });
        held_rx.recv().unwrap();
        Held { release, thread }
    }
}

/// The owner lock, held by [`Server::hold_owner`].
pub struct Held {
    release: std::sync::mpsc::Sender<()>,
    thread: std::thread::JoinHandle<()>,
}

impl Held {
    pub fn release(self) {
        self.release.send(()).unwrap();
        self.thread.join().unwrap();
    }
}
