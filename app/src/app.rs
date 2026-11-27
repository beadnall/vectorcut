//! The VectorCut window: state, the shell around it, and what each control does.

use crate::canvas;
use crate::worker::{Reply, Request, Target, Worker};
use craft::dialogs::{Answer, Dialogs, FileDialog, Wake};
use craft::egui::{self, Rect, Ui, UiBuilder, pos2};
use craft::theme::variant::Tone;
use craft::theme::{Mode, ModeChoice};
use craft::ui::buttons::Button;
use craft::ui::choice::Checkbox;
use craft::ui::contract::{Bind, Entry, Props as _, Show as _, ShowContainer as _};
use craft::ui::display::Text;
use craft::ui::form::Field;
use craft::ui::notices::Notice;
use craft::ui::pickers::Select;
use craft::ui::popover::DropVeil;
use craft::ui::runtime::env::{RootEnv, WindowKey};
use craft::ui::runtime::frame::FrameInput;
use craft::ui::sections::{Panel, Section};
use craft::ui::shell::AppShell;
use craft::ui::shell::app::ShellApp;
use craft::ui::shell::embed::CloseDecision;
use craft::ui::style::Variant;
use craft::ui::text::NumberField;
use raw_window_handle::{DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, WindowHandle};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use vectorcut_core::machine::mat_name;
use vectorcut_core::silhouette::Tool as HolderTool;
use vectorcut_core::{Design, MODELS, Mat, Model, PRESETS, Plan, Status, Tool, job};
use vectorcut_usb::{Event, Found};

/// How often an idle machine's status is asked.
const REFRESH_EVERY: Duration = Duration::from_secs(2);

/// Width of the settings panel, points.
const PANEL_WIDTH: f32 = 340.0;

/// What a file dialog was asked for.
enum Pick {
    Open,
}

/// The machine VectorCut is connected to.
struct Connection {
    label: String,
    model: Option<&'static Model>,
    firmware: Option<String>,
}

/// A cut in progress.
struct Running {
    what: &'static str,
    sent: usize,
    total: usize,
    phase: String,
}

/// What a run sends.
#[derive(Clone, Copy, PartialEq)]
enum RunKind {
    Cut,
    Outline,
    TestCut,
}

pub struct VectorCut {
    root: RootEnv,
    worker: Worker,
    dialogs: Dialogs<Pick>,
    design: Option<Design>,
    design_path: Option<PathBuf>,
    plan: Option<Plan>,
    /// The model picked by hand, into [`MODELS`]; a connected machine's own model wins.
    model_pick: usize,
    /// The machines on USB, from the last scan.
    machines: Vec<Found>,
    /// Into the machine list: the USB machines, then the simulated one.
    machine_pick: usize,
    connection: Option<Connection>,
    status: Option<Status>,
    tools: Vec<Option<HolderTool>>,
    running: Option<Running>,
    notice: Option<(Tone, String)>,
    /// The preset last applied to each group, into [`PRESETS`].
    preset_picks: Vec<usize>,
    /// The tool new groups start with.
    default_tool: Tool,
    /// Connect to the simulated machine once the first scan is in.
    connect_simulated: bool,
    scanned: bool,
    /// When the machine's status was last asked.
    last_refresh: Option<Instant>,
}

impl VectorCut {
    pub fn new(ctx: &egui::Context, open: Option<PathBuf>, simulate: bool) -> VectorCut {
        let repaint = ctx.clone();
        let worker = Worker::start(move || repaint.request_repaint());
        let wake_ctx = ctx.clone();
        let dialogs = Dialogs::new(Wake::new(move || wake_ctx.request_repaint()));
        let model_pick = MODELS.iter().position(|m| m.id == "cameo4").unwrap_or(0);
        let model = &MODELS[model_pick];
        let preset = PRESETS[0];
        let mut app = VectorCut {
            root: system_appearance(),
            worker,
            dialogs,
            design: None,
            design_path: None,
            plan: None,
            model_pick,
            machines: Vec::new(),
            machine_pick: 0,
            connection: None,
            status: None,
            tools: Vec::new(),
            running: None,
            notice: None,
            preset_picks: Vec::new(),
            default_tool: Tool::from_preset(&preset, model),
            connect_simulated: simulate,
            scanned: false,
            last_refresh: None,
        };
        app.worker.send(Request::Scan);
        if let Some(path) = open {
            app.open_path(&path);
        }
        app
    }

    /// The model the plan is for: the connected machine's, else the one picked.
    fn model(&self) -> &'static Model {
        self.connection.as_ref().and_then(|c| c.model).unwrap_or(&MODELS[self.model_pick.min(MODELS.len() - 1)])
    }

    fn simulated_model(&self) -> &'static Model {
        &MODELS[self.model_pick.min(MODELS.len() - 1)]
    }

    fn open_path(&mut self, path: &Path) {
        let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        match std::fs::read(path) {
            Ok(bytes) => {
                self.open_bytes(&bytes, &name);
                self.design_path = Some(path.to_path_buf());
            }
            Err(error) => self.notice = Some((Tone::Bad, format!("Could not read {name}: {error}"))),
        }
    }

    fn open_bytes(&mut self, bytes: &[u8], name: &str) {
        match Design::from_bytes(bytes, name) {
            Ok(design) => {
                let model = self.model();
                let plan = match self.plan.take() {
                    Some(mut plan) => {
                        plan.follow(&design, &self.default_tool);
                        plan
                    }
                    None => {
                        let mut plan = Plan::new(&design, model);
                        for g in &mut plan.groups {
                            g.tool = self.default_tool.clone();
                        }
                        plan
                    }
                };
                self.preset_picks = vec![0; plan.groups.len()];
                self.notice = match design.warnings.len() {
                    0 => None,
                    n => Some((Tone::Warn, format!("{name}: {n} warning(s). {}", design.warnings.first().cloned().unwrap_or_default()))),
                };
                self.plan = Some(plan);
                self.design = Some(design);
            }
            Err(error) => self.notice = Some((Tone::Bad, format!("{name}: {error}"))),
        }
    }

    /// Points the plan at `model`, keeping the mat when the model takes it.
    fn follow_model(&mut self) {
        let model = *self.model();
        if let Some(plan) = &mut self.plan {
            if plan.model == model {
                return;
            }
            plan.model = model;
            if !model.mats.contains(&plan.mat) {
                plan.mat = model.default_mat();
            }
        }
    }

    fn cut_area(&self) -> (f64, f64) {
        match (&self.plan, &self.design) {
            (Some(plan), Some(design)) => plan.cut_area_mm(design),
            _ => {
                let model = self.model();
                model.cut_area_mm(model.default_mat(), 300.0)
            }
        }
    }

    fn start(&mut self, kind: RunKind) {
        let program = match kind {
            RunKind::TestCut => {
                // In the top-right corner, where designs seldom are.
                let (across, _) = self.cut_area();
                let test = job::test_cut_design((across - 15.0).max(0.0), 5.0);
                let mut plan = Plan::new(&test, self.model());
                if let Some(current) = &self.plan {
                    plan.mat = current.mat;
                    if let (Some(tool), Some(g)) = (current.groups.iter().find(|g| g.enabled && !g.tool.pen).map(|g| g.tool.clone()), plan.groups.first_mut()) {
                        g.tool = tool;
                    }
                }
                plan.encode(&test)
            }
            RunKind::Cut | RunKind::Outline => {
                let (Some(design), Some(plan)) = (&self.design, &self.plan) else { return };
                if kind == RunKind::Cut { plan.encode(design) } else { plan.encode_outline(design) }
            }
        };
        match program {
            Ok(program) => {
                let what = match kind {
                    RunKind::Cut => "Cutting",
                    RunKind::Outline => "Tracing the outline",
                    RunKind::TestCut => "Test cut",
                };
                self.running = Some(Running { what, sent: 0, total: program.to_stream().len(), phase: "Starting".into() });
                self.notice = None;
                self.worker.send(Request::Run(program));
            }
            Err(error) => self.notice = Some((Tone::Bad, error.to_string())),
        }
    }

    fn handle(&mut self, reply: Reply) {
        match reply {
            Reply::Found(found) => {
                let first = !self.scanned;
                self.scanned = true;
                self.machines = found;
                self.machine_pick = self.machine_pick.min(self.machines.len());
                if first && self.connection.is_none() {
                    if self.connect_simulated {
                        self.machine_pick = self.machines.len();
                        self.connect();
                    } else if self.machines.len() == 1 {
                        self.machine_pick = 0;
                        self.connect();
                    }
                }
            }
            Reply::Connected { label, model, firmware } => {
                self.connection = Some(Connection { label, model, firmware });
                self.follow_model();
            }
            Reply::Disconnected => {
                self.connection = None;
                self.status = None;
                self.tools.clear();
            }
            Reply::Status { status, tools } => {
                self.status = Some(status);
                self.tools = tools;
            }
            Reply::Progress(event) => {
                if let Some(run) = &mut self.running {
                    match event {
                        Event::Waiting(Status::Unloaded) => run.phase = "Load the mat (press the load button on the machine)".into(),
                        Event::Waiting(status) => run.phase = format!("Waiting for the machine ({status:?})"),
                        Event::Sent { sent, total } => {
                            run.sent = sent;
                            run.total = total;
                            run.phase = "Sending".into();
                        }
                        Event::SearchingMarks => run.phase = "Looking for registration marks".into(),
                        Event::Finishing => run.phase = "Finishing".into(),
                    }
                }
            }
            Reply::Finished(result) => {
                let what = self.running.take().map_or("The job", |r| r.what);
                self.notice = Some(match result {
                    Ok(()) => (Tone::Good, format!("{what}: done.")),
                    Err(error) => (Tone::Warn, format!("{what}: {error}.")),
                });
            }
            Reply::Failed(message) => self.notice = Some((Tone::Bad, message)),
        }
    }

    fn connect(&mut self) {
        let target = match self.machines.get(self.machine_pick) {
            Some(found) => Target::Usb(Box::new(found.clone())),
            None => Target::Simulated(self.simulated_model()),
        };
        self.worker.send(Request::Connect(target));
    }

    // ----- The shell's regions -----

    fn toolbar(&mut self, ui: &mut Ui) {
        let busy = self.running.is_some();
        let connected = self.connection.is_some();
        let cuttable = self.design.is_some() && self.plan.as_ref().zip(self.design.as_ref()).is_some_and(|(p, d)| p.job(d).is_ok());
        ui.horizontal_centered(|ui| {
            if Button::new("Open…").tooltip("Open an SVG file").show(ui).clicked() {
                self.dialogs.ask(FileDialog::open().title("Open a design").filter("SVG drawing", &["svg"]), Pick::Open);
            }
            if let Some(path) = self.design_path.clone()
                && Button::new("Reload").tooltip("Read the file again, keeping the settings").enabled(!busy).show(ui).clicked()
            {
                self.open_path(&path);
            }
            ui.separator();
            if Button::new("Test cut").tooltip("Cut a 10 mm square with a triangle in the top-right corner, with the first colour's settings").enabled(connected && !busy).show(ui).clicked() {
                self.start(RunKind::TestCut);
            }
            if Button::new("Trace outline").tooltip("Move round the design's bounding box, to check the placement").enabled(connected && !busy && cuttable).show(ui).clicked() {
                self.start(RunKind::Outline);
            }
            if Button::new("Cut").variant(Variant::PRIMARY).enabled(connected && !busy && cuttable).show(ui).clicked() {
                self.start(RunKind::Cut);
            }
            if busy && Button::new("Stop").variant(Variant::DANGER).tooltip("Stop sending; the machine finishes the piece it has").show(ui).clicked() {
                self.worker.stop();
            }
        });
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        let machine = match &self.connection {
            Some(c) => {
                let status = match self.status {
                    Some(Status::Ready) => "ready".to_string(),
                    Some(Status::Moving) => "working".to_string(),
                    Some(Status::Unloaded) => "no mat loaded".to_string(),
                    Some(Status::Unknown(code)) => format!("status {code}"),
                    None => "connected".to_string(),
                };
                format!("{} — {status}", c.label)
            }
            None if self.scanned && self.machines.is_empty() => "No machine found on USB".to_string(),
            None => "Not connected".to_string(),
        };
        let job = match (&self.running, &self.plan, &self.design) {
            (Some(r), _, _) => format!("{}: {} {}%", r.what, r.phase, r.sent * 100 / r.total.max(1)),
            (None, Some(plan), Some(design)) => {
                let s = plan.summary(design);
                format!("{} lines · cut {:.0} mm · draw {:.0} mm · travel {:.0} mm", s.lines, s.cut_mm, s.draw_mm, s.travel_mm)
            }
            _ => String::new(),
        };
        ui.horizontal_centered(|ui| {
            Text::new(machine).show(ui);
            ui.separator();
            Text::new(job).show(ui);
        });
    }

    fn body(&mut self, ui: &mut Ui) {
        let full = ui.max_rect();
        let panel_width = PANEL_WIDTH.min(full.width() * 0.5);
        let split = full.max.x - panel_width;
        let canvas_rect = Rect::from_min_max(full.min, pos2(split, full.max.y));
        let panel_rect = Rect::from_min_max(pos2(split, full.min.y), full.max);
        let area = self.cut_area();
        let dark = self.root.appearance(ui.ctx()).mode == Mode::Dark;
        let shown = canvas::show(ui, canvas_rect, self.design.as_ref(), self.plan.as_ref(), area, dark);
        if let (Some((dx, dy)), Some(plan), None) = (shown.moved_mm, &mut self.plan, &self.running) {
            plan.offset_right_mm = round_tenth((plan.offset_right_mm + dx).max(0.0));
            plan.offset_down_mm = round_tenth((plan.offset_down_mm + dy).max(0.0));
        }
        ui.scope_builder(UiBuilder::new().max_rect(panel_rect), |ui| {
            Panel::new("Cut settings").show(ui, |ui, _| self.panel(ui));
        });
    }

    fn panel(&mut self, ui: &mut Ui) {
        if let Some((tone, text)) = self.notice.clone() {
            Notice::new(tone, text).id_salt("vectorcut-notice").show(ui);
        }
        self.machine_section(ui);
        if self.plan.is_some() {
            self.placement_section(ui);
            self.group_sections(ui);
        }
    }

    fn machine_section(&mut self, ui: &mut Ui) {
        let busy = self.running.is_some();
        Section::new("Machine").show(ui, |ui, _| {
            let mut entries: Vec<Entry<usize>> = self.machines.iter().enumerate().map(|(i, m)| Entry::new(i, m.label())).collect();
            entries.push(Entry::new(self.machines.len(), format!("Simulated {}", self.simulated_model().name)));
            let mut pick = self.machine_pick.min(self.machines.len());
            Field::new("Machine").show(ui, |ui, _| Select::new("Machine", Bind::bound(&mut pick), &entries).enabled(!busy).show(ui));
            self.machine_pick = pick;
            ui.horizontal(|ui| {
                let label = if self.connection.is_some() { "Reconnect" } else { "Connect" };
                if Button::new(label).enabled(!busy).show(ui).clicked() {
                    self.connect();
                }
                if Button::new("Find machines").enabled(!busy).show(ui).clicked() {
                    self.worker.send(Request::Scan);
                }
                if self.connection.is_some() && Button::new("Disconnect").enabled(!busy).show(ui).clicked() {
                    self.worker.send(Request::Disconnect);
                }
            });
            if let Some(c) = &self.connection {
                let firmware = c.firmware.clone().unwrap_or_else(|| "firmware not reported".into());
                Text::new(firmware).show(ui);
                if !self.tools.is_empty() {
                    let tools: Vec<String> = self.tools.iter().enumerate().map(|(i, t)| format!("holder {}: {}", i + 1, t.map_or("?".into(), |t| format!("{t:?}")))).collect();
                    Text::new(tools.join(" · ")).show(ui);
                }
            }

            // The model: a connected machine names its own; otherwise it is picked here.
            let models: Vec<Entry<usize>> = MODELS.iter().enumerate().map(|(i, m)| Entry::new(i, m.name)).collect();
            let fixed = self.connection.as_ref().and_then(|c| c.model);
            let mut model_pick = fixed.and_then(|m| MODELS.iter().position(|x| x.id == m.id)).unwrap_or(self.model_pick);
            Field::new("Model").show(ui, |ui, _| Select::new("Model", Bind::bound(&mut model_pick), &models).enabled(fixed.is_none() && !busy).show(ui));
            if fixed.is_none() && model_pick != self.model_pick {
                self.model_pick = model_pick;
                self.follow_model();
            }

            let model = self.model();
            if let Some(plan) = &mut self.plan {
                let mats: Vec<Entry<Mat>> = model.mats.iter().map(|&m| Entry::new(m, mat_name(m))).collect();
                let mut mat = plan.mat;
                Field::new("Mat").show(ui, |ui, _| Select::new("Mat", Bind::bound(&mut mat), &mats).enabled(!busy).show(ui));
                plan.mat = mat;
            }
        });
    }

    fn placement_section(&mut self, ui: &mut Ui) {
        let busy = self.running.is_some();
        let Some(plan) = &mut self.plan else { return };
        Section::new("Placement").show(ui, |ui, _| {
            number(ui, "From left", &mut plan.offset_right_mm, 0.0..=2000.0, 1.0, "mm", !busy);
            number(ui, "From top", &mut plan.offset_down_mm, 0.0..=20000.0, 1.0, "mm", !busy);
            ui.horizontal(|ui| {
                if Button::new("Top-left").enabled(!busy).show(ui).clicked() {
                    plan.offset_right_mm = 0.0;
                    plan.offset_down_mm = 0.0;
                }
            });
            let mut keep_order = !plan.optimise_order;
            Checkbox::new(Bind::bound(&mut keep_order)).label("Keep the drawing's order").enabled(!busy).show(ui);
            plan.optimise_order = !keep_order;
            Text::new("Drag the design on the mat to move it.").show(ui);
        });
    }

    fn group_sections(&mut self, ui: &mut Ui) {
        let busy = self.running.is_some();
        let model = *self.model();
        let (Some(plan), Some(design)) = (&mut self.plan, &self.design) else { return };
        if let Err(error) = plan.job(design)
            && plan.groups.iter().any(|g| g.enabled)
        {
            Notice::new(Tone::Warn, error.to_string()).id_salt("vectorcut-plan").show(ui);
        }
        let presets: Vec<Entry<usize>> = PRESETS.iter().enumerate().map(|(i, p)| Entry::new(i, p.name)).collect();
        let holders = [Entry::new(1u8, "1 (left)"), Entry::new(2u8, "2 (right)")];
        self.preset_picks.resize(plan.groups.len(), 0);
        for (index, (choice, group)) in plan.groups.iter_mut().zip(&design.groups).enumerate() {
            let title = if group.key.starts_with('#') { format!("Colour {}", group.key) } else { group.key.clone() };
            Section::new(title).show(ui, |ui, _| {
                Text::new(format!("{} lines, {:.0} mm", group.paths.len(), group.length_mm())).show(ui);
                Checkbox::new(Bind::bound(&mut choice.enabled)).label("Cut this colour").enabled(!busy).show(ui);
                if !choice.enabled {
                    return;
                }
                let before = self.preset_picks[index];
                let mut pick = before;
                Field::new("Material").show(ui, |ui, _| Select::new("Material", Bind::bound(&mut pick), &presets).enabled(!busy).show(ui));
                if pick != before {
                    self.preset_picks[index] = pick;
                    let holder = choice.tool.holder;
                    choice.tool = Tool { holder, ..Tool::from_preset(&PRESETS[pick], &model) };
                }
                let tool = &mut choice.tool;
                Checkbox::new(Bind::bound(&mut tool.pen)).label("Pen (draw, don't cut)").enabled(!busy).show(ui);
                if model.two_holders {
                    let mut holder = tool.holder.clamp(1, 2);
                    Field::new("Tool holder").show(ui, |ui, _| Select::new("Tool holder", Bind::bound(&mut holder), &holders).enabled(!busy).show(ui));
                    tool.holder = holder;
                }
                integer(ui, "Force", &mut tool.force, 1..=model.max_force, !busy);
                integer(ui, "Speed", &mut tool.speed, 1..=model.max_speed, !busy);
                if !tool.pen {
                    let mut depth = u32::from(tool.depth.unwrap_or(0));
                    integer(ui, "Blade depth", &mut depth, 0..=10, !busy);
                    tool.depth = Some(depth.min(10) as u8);
                    number(ui, "Overcut", &mut tool.overcut_mm, 0.0..=2.0, 0.1, "mm", !busy);
                }
                let mut passes = u32::from(tool.passes);
                integer(ui, "Passes", &mut passes, 1..=10, !busy);
                tool.passes = passes.clamp(1, 10) as u8;
            });
        }
    }
}

/// A labelled number field over an `f64`.
fn number(ui: &mut Ui, label: &str, value: &mut f64, range: RangeInclusive<f64>, step: f64, unit: &str, enabled: bool) {
    let mut shown = Some(*value);
    Field::new(label.to_string()).show(ui, |ui, _| {
        NumberField::new(label.to_string(), Bind::bound(&mut shown)).range(range.clone()).step(step).unit(unit.to_string()).max_decimals(1).enabled(enabled).show(ui)
    });
    if let Some(v) = shown
        && v.is_finite()
    {
        *value = v.clamp(*range.start(), *range.end());
    }
}

/// A labelled number field over a whole number.
fn integer(ui: &mut Ui, label: &str, value: &mut u32, range: RangeInclusive<u32>, enabled: bool) {
    let mut shown = Some(f64::from(*value));
    let (lo, hi) = (f64::from(*range.start()), f64::from(*range.end()));
    Field::new(label.to_string()).show(ui, |ui, _| NumberField::new(label.to_string(), Bind::bound(&mut shown)).range(lo..=hi).step(1.0).max_decimals(0).enabled(enabled).show(ui));
    if let Some(v) = shown
        && v.is_finite()
    {
        *value = v.round().clamp(lo, hi) as u32;
    }
}

/// The window's settings: the defaults, light or dark as the Mac is set.
fn system_appearance() -> RootEnv {
    let mut root = RootEnv::default();
    root.mode = ModeChoice::System;
    root
}

fn round_tenth(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// The open panel needs no parent window on macOS; this stands for "none".
struct NoWindow;

impl HasWindowHandle for NoWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        Err(HandleError::Unavailable)
    }
}

impl HasDisplayHandle for NoWindow {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        Err(HandleError::Unavailable)
    }
}

impl ShellApp for VectorCut {
    fn frame_input(&mut self) -> FrameInput {
        FrameInput::new(self.root.clone())
    }

    fn logic(&mut self, _ctx: &egui::Context) {
        for reply in self.worker.replies() {
            self.handle(reply);
        }
        for (pick, answer) in self.dialogs.take_answers() {
            match (pick, answer) {
                (Pick::Open, Answer::Paths(paths)) => {
                    if let Some(path) = paths.first() {
                        self.open_path(path);
                    }
                }
                (Pick::Open, Answer::Unavailable(why)) => self.notice = Some((Tone::Bad, why)),
                _ => {}
            }
        }
    }

    fn ui(&mut self, ui: &mut Ui) {
        let shell = |ui: &mut Ui, app: &mut VectorCut| {
            AppShell::new(WindowKey::main())
                .options_bar(true, |_| None, |ui, _, app: &mut VectorCut| app.toolbar(ui))
                .status(true, |_| None, |ui, _, app: &mut VectorCut| app.status_bar(ui))
                .body_label("Mat")
                .body(|ui, _, app: &mut VectorCut| app.body(ui))
                .show(ui, app);
        };
        let dropped = DropVeil::new("Drop an SVG to open it").show_with_files(ui, |ui, _| shell(ui, self)).dropped.clone();
        if let Some(file) = dropped.as_ref().and_then(|d| d.files.first()) {
            match (&file.path, &file.bytes) {
                (Some(path), _) => self.open_path(&path.clone()),
                (None, Some(bytes)) => {
                    let (bytes, name) = (bytes.clone(), file.name.clone());
                    self.open_bytes(&bytes, &name);
                }
                (None, None) => {}
            }
        }
        self.dialogs.show_pending(None::<&NoWindow>);
    }

    fn tick(&mut self, now: Instant) -> Option<Instant> {
        // While connected and idle, ask the machine's status now and then, so loading or
        // unloading the mat shows.
        if self.connection.is_none() || self.running.is_some() {
            return None;
        }
        let due = self.last_refresh.map_or(now, |at| at + REFRESH_EVERY);
        if now >= due {
            self.worker.send(Request::Refresh);
            self.last_refresh = Some(now);
            return Some(now + REFRESH_EVERY);
        }
        Some(due)
    }

    fn close_requested(&mut self) -> CloseDecision {
        if self.running.is_some() {
            self.notice = Some((Tone::Warn, "A cut is running. Stop it before quitting.".into()));
            return CloseDecision::Hold;
        }
        CloseDecision::Close
    }
}
