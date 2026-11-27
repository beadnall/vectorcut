//! Talking to Silhouette cutters.
//!
//! - [`usb`]: finding machines on USB and opening them ([`usb::find`], [`usb::Found::open`]).
//! - [`cutter`]: a connected machine ([`Cutter`]): firmware and status queries, and sending a
//!   program paced the way the machines need.
//! - [`sim`]: a simulated machine for tests and for trying VectorCut without a cutter.
//! - [`link`]: the byte pipe both provide.

pub mod cutter;
pub mod link;
pub mod sim;
pub mod usb;

pub use cutter::{Cutter, CutterError, Event, RunOptions};
pub use link::{Link, LinkError};
pub use sim::SimulatedLink;
pub use usb::{Found, UsbError, UsbLink, find};

/// Any link: a machine on USB or a simulated one.
pub type AnyLink = Box<dyn Link>;
