//! The byte pipe to a machine: what the USB link and the simulated machine both provide.

use std::time::Duration;

/// Why the pipe failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    #[error("the machine was disconnected")]
    Disconnected,
    #[error("the machine refused the transfer (endpoint stalled)")]
    Stalled,
    #[error("USB error: {0}")]
    Io(String),
}

/// A two-way byte pipe to a cutter.
pub trait Link: Send {
    /// Writes as much of `bytes` as the machine accepts within `timeout`, and returns how many
    /// bytes it accepted. A machine whose buffer is full accepts fewer, or none.
    fn write(&mut self, bytes: &[u8], timeout: Duration) -> Result<usize, LinkError>;

    /// Returns what the machine has sent, waiting up to `timeout` for something to arrive. Empty
    /// when nothing came.
    fn read(&mut self, timeout: Duration) -> Result<Vec<u8>, LinkError>;

    /// What the pipe is connected to, for people (`Cameo 4 (USB 20-1.2)`).
    fn describe(&self) -> String;
}

impl<L: Link + ?Sized> Link for Box<L> {
    fn write(&mut self, bytes: &[u8], timeout: Duration) -> Result<usize, LinkError> {
        (**self).write(bytes, timeout)
    }

    fn read(&mut self, timeout: Duration) -> Result<Vec<u8>, LinkError> {
        (**self).read(timeout)
    }

    fn describe(&self) -> String {
        (**self).describe()
    }
}
