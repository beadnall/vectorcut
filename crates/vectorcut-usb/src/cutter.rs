//! A connected machine: queries, and sending a program the way the machines expect it.
//!
//! Sending follows the pacing the community drivers found reliable: the program goes out in
//! pieces of at most [`CHUNK_BYTES`] that end on a command boundary, and after each piece the
//! sender polls the status until the machine reports Ready, so its input buffer never overflows.
//! A program with registration marks pauses after the mark commands until the machine reports it
//! found them.

use crate::link::{Link, LinkError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use vectorcut_core::silhouette::{self, Status, Tool};
use vectorcut_core::{Model, Program};

/// Largest piece of a program written before waiting for the machine.
pub const CHUNK_BYTES: usize = 1024;
/// How long a single write may wait for the machine to take bytes.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the machine may refuse bytes in a row before the send gives up.
const STALL_LIMIT: Duration = Duration::from_secs(60);
/// How long a query waits for its reply.
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);
/// Time between status polls while the machine works.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Why talking to the machine failed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CutterError {
    #[error(transparent)]
    Link(#[from] LinkError),
    #[error("the machine did not answer")]
    NoReply,
    #[error("the machine stopped taking data for {0:?}")]
    Stalled(Duration),
    #[error("the machine was not ready after {0:?} (status: {1:?})")]
    NotReady(Duration, Status),
    #[error("stopped")]
    Cancelled,
}

/// What a run reports as it goes.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Waiting for the machine to be ready; `Unloaded` asks the user to load media.
    Waiting(Status),
    /// Bytes of the program sent so far, of the total.
    Sent { sent: usize, total: usize },
    /// Waiting for the machine to find the registration marks.
    SearchingMarks,
    /// Everything sent; the machine is finishing the cut.
    Finishing,
}

/// Limits of a run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RunOptions {
    /// How long to wait for media to be loaded before starting.
    pub load_timeout: Duration,
    /// How long one piece may take the machine to work through.
    pub piece_timeout: Duration,
    /// How long the machine may take to find the registration marks.
    pub marks_timeout: Duration,
}

impl Default for RunOptions {
    fn default() -> Self {
        RunOptions { load_timeout: Duration::from_secs(300), piece_timeout: Duration::from_secs(120), marks_timeout: Duration::from_secs(120) }
    }
}

/// A machine on a link.
pub struct Cutter<L: Link> {
    link: L,
    /// The model, from the USB ids or the firmware reply.
    pub model: Option<&'static Model>,
    /// The firmware reply (`CAMEO 4 V1.10`), if the machine gave one.
    pub firmware: Option<String>,
}

impl<L: Link> Cutter<L> {
    /// Takes over `link` without talking to the machine.
    pub fn new(link: L, model: Option<&'static Model>) -> Cutter<L> {
        Cutter { link, model, firmware: None }
    }

    /// Initialises the machine, asks its firmware version and settles the model: `known` (from
    /// the USB ids) when given, else the one the firmware names.
    pub fn connect(link: L, known: Option<&'static Model>) -> Result<Cutter<L>, CutterError> {
        let mut cutter = Cutter::new(link, known);
        cutter.drain()?;
        cutter.write_all(silhouette::INIT.as_bytes())?;
        match cutter.query(silhouette::VERSION_QUERY) {
            Ok(reply) => {
                let version = silhouette::parse_version(&reply);
                if cutter.model.is_none() {
                    cutter.model = Model::by_firmware(&version);
                }
                cutter.firmware = Some(version);
            }
            // Older machines may not answer; the run still works.
            Err(CutterError::NoReply) => {}
            Err(other) => return Err(other),
        }
        Ok(cutter)
    }

    /// What the link is connected to.
    pub fn describe(&self) -> String {
        self.link.describe()
    }

    /// The machine's status.
    pub fn status(&mut self) -> Result<Status, CutterError> {
        self.query(silhouette::STATUS_QUERY.as_bytes()).map(|reply| silhouette::parse_status(&reply))
    }

    /// The tools in the holders (Cameo 4 line). An empty list when the machine does not say.
    pub fn tools(&mut self) -> Result<Vec<Option<Tool>>, CutterError> {
        match self.query(silhouette::TOOL_QUERY.as_bytes()) {
            Ok(reply) => Ok(silhouette::parse_tool_setup(&reply)),
            Err(CutterError::NoReply) => Ok(Vec::new()),
            Err(other) => Err(other),
        }
    }

    /// Reads and drops whatever the machine sent unasked.
    fn drain(&mut self) -> Result<(), CutterError> {
        for _ in 0..8 {
            if self.link.read(Duration::from_millis(10))?.is_empty() {
                break;
            }
        }
        Ok(())
    }

    /// Writes `bytes`, waiting while the machine refuses them, up to [`STALL_LIMIT`] in a row.
    fn write_all(&mut self, mut bytes: &[u8]) -> Result<(), CutterError> {
        let mut stalled = Duration::ZERO;
        while !bytes.is_empty() {
            let started = Instant::now();
            let n = self.link.write(bytes, WRITE_TIMEOUT)?.min(bytes.len());
            if n == 0 {
                stalled += started.elapsed().max(Duration::from_millis(1));
                if stalled >= STALL_LIMIT {
                    return Err(CutterError::Stalled(stalled));
                }
            } else {
                stalled = Duration::ZERO;
            }
            bytes = &bytes[n..];
        }
        Ok(())
    }

    /// Sends `bytes` and returns the reply, up to its ETX terminator.
    fn query(&mut self, bytes: &[u8]) -> Result<Vec<u8>, CutterError> {
        self.drain()?;
        self.write_all(bytes)?;
        let deadline = Instant::now() + REPLY_TIMEOUT;
        let mut reply = Vec::new();
        while Instant::now() < deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            reply.extend(self.link.read(left.min(Duration::from_millis(250)))?);
            if reply.contains(&0x03) || reply.len() >= silhouette::MAX_REPLY_LEN {
                return Ok(reply);
            }
            if reply.is_empty() {
                // A simulated link answers at once and never blocks; give a real one time.
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        if reply.is_empty() { Err(CutterError::NoReply) } else { Ok(reply) }
    }

    /// Polls until the machine reports Ready, for at most `limit`.
    fn wait_ready(&mut self, limit: Duration, cancel: &AtomicBool, events: &mut dyn FnMut(Event), report: bool) -> Result<(), CutterError> {
        let deadline = Instant::now() + limit;
        let mut last = None;
        loop {
            let status = self.status()?;
            if status == Status::Ready {
                return Ok(());
            }
            if report && last != Some(status) {
                events(Event::Waiting(status));
            }
            last = Some(status);
            if cancel.load(Ordering::Relaxed) {
                return Err(CutterError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(CutterError::NotReady(limit, status));
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    /// Sends `program` and waits for the machine to finish it. `cancel` stops the send between
    /// pieces; the machine then finishes the piece it has (at most [`CHUNK_BYTES`] of commands).
    pub fn run(&mut self, program: &Program, options: RunOptions, cancel: &AtomicBool, events: &mut dyn FnMut(Event)) -> Result<(), CutterError> {
        self.wait_ready(options.load_timeout, cancel, events, true)?;
        let parts = vectorcut_core::program_parts(program);
        let total: usize = parts.iter().map(Vec::len).sum();
        let mut sent = 0;
        events(Event::Sent { sent, total });
        for (index, part) in parts.iter().enumerate() {
            if index > 0 {
                events(Event::SearchingMarks);
                self.wait_ready(options.marks_timeout, cancel, events, false)?;
            }
            for piece in vectorcut_core::chunks(part, CHUNK_BYTES) {
                if cancel.load(Ordering::Relaxed) {
                    return Err(CutterError::Cancelled);
                }
                self.write_all(piece)?;
                sent += piece.len();
                events(Event::Sent { sent, total });
                self.wait_ready(options.piece_timeout, cancel, events, false)?;
            }
        }
        events(Event::Finishing);
        self.wait_ready(options.piece_timeout, cancel, events, false)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::sim::SimulatedLink;
    use vectorcut_core::{Design, Plan};

    fn cameo4() -> &'static Model {
        Model::by_id("cameo4").unwrap()
    }

    const DESIGN: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100mm" height="100mm" viewBox="0 0 100 100">
        <circle cx="50" cy="50" r="40" fill="none" stroke="black"/>
        <circle cx="50" cy="50" r="20" fill="none" stroke="black"/>
    </svg>"##;

    #[test]
    fn connecting_identifies_the_machine_by_firmware() {
        let link = SimulatedLink::new(cameo4());
        let state = link.state();
        let cutter = Cutter::connect(link, None).unwrap();
        assert_eq!(cutter.model.map(|m| m.id), Some("cameo4"));
        assert_eq!(cutter.firmware.as_deref(), Some("CAMEO 4 V1.00 (simulated)"));
        assert!(state.lock().unwrap().received.starts_with(b"\x1b\x04"));
    }

    #[test]
    fn a_run_sends_the_whole_program_in_paced_pieces() {
        let design = Design::from_svg(DESIGN, "c.svg").unwrap();
        let program = Plan::new(&design, cameo4()).encode(&design).unwrap();
        let link = SimulatedLink::new(cameo4());
        let state = link.state();
        state.lock().unwrap().max_write = Some(300);
        let mut cutter = Cutter::connect(link, Some(cameo4())).unwrap();
        let mut events = Vec::new();
        cutter.run(&program, RunOptions::default(), &AtomicBool::new(false), &mut |e| events.push(e)).unwrap();
        let received = state.lock().unwrap().received.clone();
        let stream = program.to_stream();
        // Everything sent arrived in order, between the queries.
        let without_queries: Vec<u8> = strip_queries(&received);
        assert!(without_queries.ends_with(&stream), "the program arrived whole");
        let last = events.iter().rev().find_map(|e| if let Event::Sent { sent, total } = e { Some((*sent, *total)) } else { None }).unwrap();
        assert_eq!(last, (stream.len(), stream.len()));
        assert_eq!(events.last(), Some(&Event::Finishing));
        assert!(stream.len() > CHUNK_BYTES, "the test design is big enough to need several pieces");
    }

    #[test]
    fn a_run_waits_for_media_and_can_be_stopped() {
        let design = Design::from_svg(DESIGN, "c.svg").unwrap();
        let program = Plan::new(&design, cameo4()).encode(&design).unwrap();
        let link = SimulatedLink::new(cameo4());
        link.state().lock().unwrap().unloaded = true;
        let mut cutter = Cutter::connect(link, Some(cameo4())).unwrap();
        let cancel = AtomicBool::new(true);
        let mut events = Vec::new();
        let result = cutter.run(&program, RunOptions::default(), &cancel, &mut |e| events.push(e));
        assert_eq!(result, Err(CutterError::Cancelled));
        assert_eq!(events, [Event::Waiting(Status::Unloaded)]);
    }

    /// The bytes a run wrote, with the status and version queries taken out.
    fn strip_queries(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == 0x1b && i + 1 < bytes.len() {
                i += 2;
            } else if bytes[i..].starts_with(b"FG\x03") {
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        out
    }
}
