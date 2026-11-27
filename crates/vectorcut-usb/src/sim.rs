//! A simulated machine: answers the queries a real one does and records every byte it is sent.
//! For tests, for the command line's `--simulate`, and for trying the app without a cutter.

use crate::link::{Link, LinkError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use vectorcut_core::Model;

/// What the simulated machine has been sent and how it is set up. Shared, so a caller can watch
/// a run from another thread.
#[derive(Debug, Default)]
pub struct SimState {
    /// Every byte written, in order.
    pub received: Vec<u8>,
    /// Status replies still to give before reporting Ready: how long the machine stays "moving"
    /// after each write, counted in status queries.
    pub busy_polls: u32,
    /// Report no media loaded.
    pub unloaded: bool,
    /// Accept at most this many bytes per write (to exercise partial writes).
    pub max_write: Option<usize>,
    /// Replies waiting to be read.
    pending: Vec<u8>,
    remaining_busy: u32,
}

/// A simulated machine of a given model.
#[derive(Clone)]
pub struct SimulatedLink {
    model: &'static Model,
    state: Arc<Mutex<SimState>>,
}

impl SimulatedLink {
    /// A simulated `model`, ready, with media loaded.
    pub fn new(model: &'static Model) -> SimulatedLink {
        SimulatedLink { model, state: Arc::new(Mutex::new(SimState { busy_polls: 1, ..SimState::default() })) }
    }

    /// The machine's shared state.
    pub fn state(&self) -> Arc<Mutex<SimState>> {
        Arc::clone(&self.state)
    }

    /// The model simulated.
    pub fn model(&self) -> &'static Model {
        self.model
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SimState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Link for SimulatedLink {
    fn write(&mut self, bytes: &[u8], _timeout: Duration) -> Result<usize, LinkError> {
        let mut s = self.lock();
        let n = s.max_write.map_or(bytes.len(), |m| m.min(bytes.len()));
        let bytes = &bytes[..n];
        s.received.extend_from_slice(bytes);
        let mut replies = Vec::new();
        let mut commands = 0;
        // Answer the queries in what was written.
        let mut i = 0;
        while i < bytes.len() {
            match (bytes[i], bytes.get(i + 1)) {
                (0x1b, Some(0x05)) => {
                    let status = if s.unloaded {
                        b'2'
                    } else if s.remaining_busy > 0 {
                        s.remaining_busy -= 1;
                        b'1'
                    } else {
                        b'0'
                    };
                    replies.extend_from_slice(&[status, 0x03]);
                    i += 2;
                }
                (0x1b, Some(0x15)) => {
                    // Holder 1: AutoBlade; holder 2: pen.
                    let tools: &[u8] = if self.model.two_holders { b"2,7\x03" } else { b"2\x03" };
                    replies.extend_from_slice(tools);
                    i += 2;
                }
                (0x1b, Some(_)) => i += 2,
                (b'F', Some(b'G')) if bytes.get(i + 2) == Some(&0x03) => {
                    replies.extend_from_slice(format!("{} V1.00 (simulated)\x03", self.model.name.to_uppercase()).as_bytes());
                    i += 3;
                }
                (0x03, _) => {
                    commands += 1;
                    i += 1;
                }
                _ => i += 1,
            }
        }
        if commands > 0 {
            s.remaining_busy = s.busy_polls;
        }
        s.pending.extend_from_slice(&replies);
        Ok(n)
    }

    fn read(&mut self, _timeout: Duration) -> Result<Vec<u8>, LinkError> {
        Ok(std::mem::take(&mut self.lock().pending))
    }

    fn describe(&self) -> String {
        format!("Simulated {}", self.model.name)
    }
}
