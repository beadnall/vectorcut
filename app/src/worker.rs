//! The machine thread. USB transfers block, and a cut runs for minutes, so everything that talks
//! to the machine happens here; the UI sends requests and reads replies once per frame.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use vectorcut_core::silhouette::Tool as HolderTool;
use vectorcut_core::{Model, Program, Status};
use vectorcut_usb::{AnyLink, Cutter, Event, Found, RunOptions, SimulatedLink};

/// What to connect to.
#[derive(Clone)]
pub enum Target {
    Usb(Box<Found>),
    Simulated(&'static Model),
}

/// What the UI asks of the machine thread.
pub enum Request {
    /// List the machines on USB.
    Scan,
    Connect(Target),
    Disconnect,
    /// Ask the status and the tools in the holders.
    Refresh,
    /// Send a program and wait for the machine to finish it.
    Run(Program),
}

/// What the machine thread answers.
pub enum Reply {
    Found(Vec<Found>),
    Connected { label: String, model: Option<&'static Model>, firmware: Option<String> },
    Disconnected,
    Status { status: Status, tools: Vec<Option<HolderTool>> },
    Progress(Event),
    /// A run ended: `Ok` when the machine finished, else why it stopped.
    Finished(Result<(), String>),
    Failed(String),
}

/// The UI's handle on the machine thread.
pub struct Worker {
    requests: Sender<Request>,
    replies: Receiver<Reply>,
    cancel: Arc<AtomicBool>,
}

impl Worker {
    /// Starts the thread; `wake` asks the UI for a frame when a reply is waiting.
    pub fn start(wake: impl Fn() + Send + 'static) -> Worker {
        let (requests, inbox) = mpsc::channel::<Request>();
        let (outbox, replies) = mpsc::channel::<Reply>();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&cancel);
        let spawned = std::thread::Builder::new().name("vectorcut-machine".into()).spawn(move || {
            let send = |reply: Reply| {
                let _ = outbox.send(reply);
                wake();
            };
            serve(&inbox, &send, &stop);
        });
        if let Err(error) = spawned {
            log::error!("the machine thread did not start: {error}");
        }
        Worker { requests, replies, cancel }
    }

    pub fn send(&self, request: Request) {
        if matches!(request, Request::Run(_)) {
            self.cancel.store(false, Ordering::Relaxed);
        }
        let _ = self.requests.send(request);
    }

    /// Stops a run between pieces.
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// The replies that came in since the last call.
    pub fn replies(&self) -> Vec<Reply> {
        self.replies.try_iter().collect()
    }
}

fn serve(inbox: &Receiver<Request>, send: &dyn Fn(Reply), cancel: &AtomicBool) {
    let mut cutter: Option<Cutter<AnyLink>> = None;
    while let Ok(request) = inbox.recv() {
        match request {
            Request::Scan => match vectorcut_usb::find() {
                Ok(found) => send(Reply::Found(found)),
                Err(error) => send(Reply::Failed(error.to_string())),
            },
            Request::Connect(target) => {
                cutter = None;
                let opened: Result<Cutter<AnyLink>, String> = match target {
                    Target::Usb(found) => found
                        .open()
                        .map_err(|e| e.to_string())
                        .and_then(|link| Cutter::connect(Box::new(link) as AnyLink, found.model).map_err(|e| e.to_string())),
                    Target::Simulated(model) => Cutter::connect(Box::new(SimulatedLink::new(model)) as AnyLink, Some(model)).map_err(|e| e.to_string()),
                };
                match opened {
                    Ok(c) => {
                        send(Reply::Connected { label: c.describe(), model: c.model, firmware: c.firmware.clone() });
                        cutter = Some(c);
                        refresh(&mut cutter, send);
                    }
                    Err(error) => send(Reply::Failed(format!("Could not connect: {error}"))),
                }
            }
            Request::Disconnect => {
                cutter = None;
                send(Reply::Disconnected);
            }
            Request::Refresh => refresh(&mut cutter, send),
            Request::Run(program) => {
                let Some(c) = cutter.as_mut() else {
                    send(Reply::Finished(Err("No machine is connected.".into())));
                    continue;
                };
                let result = c.run(&program, RunOptions::default(), cancel, &mut |event| send(Reply::Progress(event)));
                send(Reply::Finished(result.map_err(|e| e.to_string())));
                refresh(&mut cutter, send);
            }
        }
    }
}

fn refresh(cutter: &mut Option<Cutter<AnyLink>>, send: &dyn Fn(Reply)) {
    let Some(c) = cutter.as_mut() else { return };
    match c.status() {
        Ok(status) => {
            let tools = c.tools().unwrap_or_default();
            send(Reply::Status { status, tools });
        }
        Err(error) => {
            *cutter = None;
            send(Reply::Failed(format!("Lost the machine: {error}")));
            send(Reply::Disconnected);
        }
    }
}
