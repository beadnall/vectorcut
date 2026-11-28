//! The VectorCut window: state, the shell around it, and what each control does.

use crate::canvas;
use crate::worker::{Reply, Request, Target, Worker};
use craft::dialogs::{Answer, Dialogs, FileDialog, Wake};
use craft::egui::{self, Rect, Ui, UiBuilder, pos2};
use craft::theme::variant::Tone;
use craft::theme::{Mode, ModeChoice};
use craft::ui::buttons::Button;
use craft::ui::choice::{Checkbox, Segmented};
use craft::ui::contract::{Bind, Entry, Props as _, Show as _, ShowContainer as _};
use craft::ui::contract::RowAlign;
use craft::ui::display::{Divider, Text};
use craft::ui::layout_components::Row;
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
        Row::new().align(RowAlign::Center).show(ui, |ui, _| {
            if Button::new("Open…").tooltip("Open an SVG file").show(ui).clicked() {
                self.dialogs.ask(FileDialog::open().title("Open a design").filter("SVG drawing", &["svg"]), Pick::Open);
            }
            if let Some(path) = self.design_path.clone()
                && Button::new("Reload").tooltip("Read the file again, keeping the settings").enabled(!busy).show(ui).clicked()
            {
                self.open_path(&path);
            }
            Divider::vertical().show(ui);
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
        Row::new().align(RowAlign::Center).show(ui, |ui, _| {
            Text::new(machine.clone()).show(ui);
            Divider::vertical().show(ui);
            Text::new(job.clone()).show(ui);
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
        Section::new("Machine").id_salt("machine").show(ui, |ui, _| {
            let mut entries: Vec<Entry<usize>> = self.machines.iter().enumerate().map(|(i, m)| Entry::new(i, m.label())).collect();
            entries.push(Entry::new(self.machines.len(), format!("Simulated {}", self.simulated_model().name)));
            let mut pick = self.machine_pick.min(self.machines.len());
            Field::new("Machine").show(ui, |ui, _| Select::new("Machine", Bind::bound(&mut pick), &entries).enabled(!busy).show(ui));
            self.machine_pick = pick;
            Row::new().show(ui, |ui, _| {
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
        Section::new("Placement").id_salt("placement").show(ui, |ui, _| {
            number(ui, "From left", &mut plan.offset_right_mm, 0.0..=2000.0, 1.0, "mm", !busy);
            number(ui, "From top", &mut plan.offset_down_mm, 0.0..=20000.0, 1.0, "mm", !busy);
            Row::new().show(ui, |ui, _| {
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
        // Materials are for blades; drawing has its own settings, from the pen preset.
        let presets: Vec<Entry<usize>> = PRESETS.iter().enumerate().filter(|(_, p)| !p.pen).map(|(i, p)| Entry::new(i, p.name)).collect();
        let pen = PRESETS.iter().find(|p| p.pen);
        let actions = [Entry::new(Action::Cut, "Cut"), Entry::new(Action::Draw, "Draw"), Entry::new(Action::Skip, "Skip")];
        let holders = [Entry::new(1u8, "1 (left)"), Entry::new(2u8, "2 (right)")];
        self.preset_picks.resize(plan.groups.len(), 0);
        for (index, (choice, group)) in plan.groups.iter_mut().zip(&design.groups).enumerate() {
            let title = if group.key.starts_with('#') { format!("Colour {}", group.key) } else { group.key.clone() };
            Section::new(title).id_salt(("group", group.key.as_str())).show(ui, |ui, _| {
                Text::new(format!("{} lines, {:.0} mm", group.paths.len(), group.length_mm())).show(ui);
                let before = Action::of(choice.enabled, choice.tool.pen);
                let mut action = before;
                Field::new("Action").show(ui, |ui, _| Segmented::new("Action", Bind::bound(&mut action), &actions).enabled(!busy).show(ui));
                if action != before {
                    choice.enabled = action != Action::Skip;
                    let holder = choice.tool.holder;
                    // Switching between blade and pen starts from that tool's own settings: a blade's
                    // force would press a pen too hard, and a pen has no depth or overcut.
                    match (action, pen) {
                        (Action::Cut, _) if choice.tool.pen => {
                            let pick = self.preset_picks[index];
                            let preset = PRESETS.get(pick).filter(|p| !p.pen).unwrap_or(&PRESETS[0]);
                            choice.tool = Tool { holder, ..Tool::from_preset(preset, &model) };
                        }
                        (Action::Draw, Some(pen)) if !choice.tool.pen => choice.tool = Tool { holder, ..Tool::from_preset(pen, &model) },
                        (Action::Draw, None) => choice.tool.pen = true,
                        _ => {}
                    }
                    // And from the holder the machine reports holding that tool.
                    if action != Action::Skip
                        && model.two_holders
                        && let Some(found) = holder_with(&self.tools, choice.tool.pen)
                    {
                        choice.tool.holder = found;
                    }
                }
                match action {
                    Action::Skip => {
                        Text::new("Left out: the machine neither cuts nor draws these lines.").show(ui);
                        return;
                    }
                    Action::Cut => {
                        let before = self.preset_picks[index];
                        let mut pick = before;
                        Field::new("Material").show(ui, |ui, _| Select::new("Material", Bind::bound(&mut pick), &presets).enabled(!busy).show(ui));
                        if pick != before {
                            self.preset_picks[index] = pick;
                            let holder = choice.tool.holder;
                            choice.tool = Tool { holder, ..Tool::from_preset(&PRESETS[pick], &model) };
                        }
                    }
                    Action::Draw => {}
                }
                let tool = &mut choice.tool;
                if model.two_holders {
                    let mut holder = tool.holder.clamp(1, 2);
                    Field::new("Tool holder").show(ui, |ui, _| Select::new("Tool holder", Bind::bound(&mut holder), &holders).enabled(!busy).show(ui));
                    tool.holder = holder;
                }
                // The machine may report a different tool in that holder than this colour needs, as when
                // the pen and blade were swapped, or the colours were set before the machine connected.
                if let Some(problem) = holder_problem(&self.tools, tool.holder, tool.pen) {
                    Notice::new(Tone::Warn, problem).id_salt(("vectorcut-holder", group.key.as_str())).show(ui);
                    if model.two_holders
                        && let Some(found) = holder_with(&self.tools, tool.pen)
                        && found != tool.holder
                        && Button::new(format!("Use holder {found}")).enabled(!busy).show(ui).clicked()
                    {
                        tool.holder = found;
                    }
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

/// What the machine does with one colour's lines.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    /// With a blade.
    Cut,
    /// With a pen.
    Draw,
    /// Not at all.
    Skip,
}

impl Action {
    fn of(enabled: bool, pen: bool) -> Action {
        match (enabled, pen) {
            (false, _) => Action::Skip,
            (true, true) => Action::Draw,
            (true, false) => Action::Cut,
        }
    }
}

fn is_blade(tool: HolderTool) -> bool {
    matches!(tool, HolderTool::RatchetBlade | HolderTool::AutoBlade | HolderTool::DeepCutBlade | HolderTool::KraftBlade | HolderTool::RotaryBlade)
}

/// The first holder (numbered from 1) the machine reports holding a pen, or a blade.
fn holder_with(tools: &[Option<HolderTool>], pen: bool) -> Option<u8> {
    let at = tools.iter().position(|t| t.is_some_and(|t| if pen { t == HolderTool::Pen } else { is_blade(t) }))?;
    u8::try_from(at + 1).ok()
}

/// Why `holder` will not do for a pen (or a blade), as far as the machine's report says; `None`
/// when it will, or when the machine has not said what it holds.
fn holder_problem(tools: &[Option<HolderTool>], holder: u8, pen: bool) -> Option<String> {
    let reported = (*tools.get(usize::from(holder).checked_sub(1)?)?)?;
    let wanted = if pen { "a pen" } else { "a blade" };
    let has = match reported {
        HolderTool::Pen if pen => return None,
        tool if !pen && is_blade(tool) => return None,
        HolderTool::Empty => return Some(format!("Holder {holder} is empty; this colour needs {wanted}.")),
        HolderTool::Error => return Some(format!("Holder {holder} reports a fault.")),
        HolderTool::Pen => "a pen",
        HolderTool::RatchetBlade => "a ratchet blade",
        HolderTool::AutoBlade => "an AutoBlade",
        HolderTool::DeepCutBlade => "a Deep-Cut blade",
        HolderTool::KraftBlade => "a Kraft blade",
        HolderTool::RotaryBlade => "a rotary blade",
    };
    Some(format!("Holder {holder} has {has}, not {wanted}."))
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

#[cfg(test)]
mod tests {
    use super::*;

    const CAMEO4: [Option<HolderTool>; 2] = [Some(HolderTool::AutoBlade), Some(HolderTool::Pen)];

    #[test]
    fn the_holder_is_the_one_holding_the_tool() {
        assert_eq!(holder_with(&CAMEO4, true), Some(2));
        assert_eq!(holder_with(&CAMEO4, false), Some(1));
        assert_eq!(holder_with(&[Some(HolderTool::Empty), Some(HolderTool::Pen)], false), None);
        assert_eq!(holder_with(&[], true), None);
    }

    #[test]
    fn a_holder_with_the_wrong_tool_is_named() {
        assert_eq!(holder_problem(&CAMEO4, 1, false), None);
        assert_eq!(holder_problem(&CAMEO4, 2, true), None);
        assert_eq!(holder_problem(&CAMEO4, 1, true).as_deref(), Some("Holder 1 has an AutoBlade, not a pen."));
        assert_eq!(holder_problem(&CAMEO4, 2, false).as_deref(), Some("Holder 2 has a pen, not a blade."));
        assert_eq!(holder_problem(&[Some(HolderTool::Empty)], 1, true).as_deref(), Some("Holder 1 is empty; this colour needs a pen."));
        // Nothing known, nothing said: not connected, an unknown code, or a holder past the report.
        assert_eq!(holder_problem(&[], 1, true), None);
        assert_eq!(holder_problem(&[None], 1, true), None);
        assert_eq!(holder_problem(&CAMEO4, 3, true), None);
        assert_eq!(holder_problem(&CAMEO4, 0, true), None);
    }
}

#[cfg(test)]
mod panel_tests {
    #![allow(clippy::expect_used)]
    use super::*;
    use craft::ui::testing::Fixture;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// The colour sections for the sample tag (black outline, blue line), on a Cameo 4 that reports
    /// an AutoBlade in holder 1 and a pen in holder 2.
    fn panel() -> (Fixture, Rc<RefCell<Option<VectorCut>>>) {
        let app = Rc::new(RefCell::new(None::<VectorCut>));
        let shown = Rc::clone(&app);
        let mut fixture = Fixture::new(move |ui| {
            let mut slot = shown.borrow_mut();
            let app = slot.get_or_insert_with(|| {
                let mut app = VectorCut::new(ui.ctx(), None, false);
                app.tools = vec![Some(HolderTool::AutoBlade), Some(HolderTool::Pen)];
                app.open_bytes(include_bytes!("../../samples/tag.svg"), "tag.svg");
                app
            });
            app.group_sections(ui);
        });
        fixture.room(egui::Vec2::new(PANEL_WIDTH, 2000.0)).settle();
        (fixture, app)
    }

    fn black(app: &Rc<RefCell<Option<VectorCut>>>) -> vectorcut_core::job::GroupPlan {
        let app = app.borrow();
        let plan = app.as_ref().and_then(|a| a.plan.as_ref()).expect("a plan");
        plan.groups.iter().find(|g| g.key == "#000000").expect("the black group").clone()
    }

    fn shows(fixture: &Fixture, text: &str) -> bool {
        fixture.texts().iter().any(|t| t.text == text)
    }

    #[test]
    fn draw_takes_the_pen_holder_and_cut_the_blade_holder() {
        let (mut fixture, app) = panel();
        let start = black(&app);
        assert!(start.enabled && !start.tool.pen && start.tool.holder == 1);
        assert!(!shows(&fixture, "Holder 1 has an AutoBlade, not a pen."));

        let draw = fixture.by_label("Draw").expect("the black colour's Draw");
        fixture.click(&draw).settle();
        let drawn = black(&app);
        assert!(drawn.enabled && drawn.tool.pen, "Draw picks a pen");
        assert_eq!(drawn.tool.holder, 2, "the pen is in holder 2");
        assert_eq!(drawn.tool.depth, None);
        assert!(fixture.by_label("Material").is_some(), "the blue colour still cuts, so a Material field is left");

        let skip = fixture.by_label("Skip").expect("the black colour's Skip");
        fixture.click(&skip).settle();
        assert!(!black(&app).enabled);
        assert!(shows(&fixture, "Left out: the machine neither cuts nor draws these lines."));

        let cut = fixture.by_label("Cut").expect("the black colour's Cut");
        fixture.click(&cut).settle();
        let cutting = black(&app);
        assert!(cutting.enabled && !cutting.tool.pen);
        assert_eq!(cutting.tool.holder, 1, "the blade is in holder 1");
    }

    #[test]
    fn a_holder_with_the_wrong_tool_is_flagged_and_fixed_in_one_click() {
        let (mut fixture, app) = panel();
        let draw = fixture.by_label("Draw").expect("the black colour's Draw");
        fixture.click(&draw).settle();
        // As if the pen were set to holder 1 by hand, or the tools were swapped.
        if let Some(group) = app.borrow_mut().as_mut().and_then(|a| a.plan.as_mut()).and_then(|p| p.groups.iter_mut().find(|g| g.key == "#000000")) {
            group.tool.holder = 1;
        }
        fixture.settle();
        assert!(shows(&fixture, "Holder 1 has an AutoBlade, not a pen."));
        let fix = fixture.by_label("Use holder 2").expect("the fix button");
        fixture.click(&fix).settle();
        assert_eq!(black(&app).tool.holder, 2);
        assert!(!shows(&fixture, "Holder 1 has an AutoBlade, not a pen."));
        assert!(fixture.by_label("Use holder 2").is_none());
    }
}
