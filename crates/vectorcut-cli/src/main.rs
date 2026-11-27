//! `vectorcut`: plan, dry-run and send SVG cuts to a Silhouette from the command line.

use clap::{Args, Parser, Subcommand};
use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use vectorcut_core::{Design, Mat, Model, Plan, Preset, Tool, job, machine};
use vectorcut_usb::{AnyLink, Cutter, Event, RunOptions, SimulatedLink};

#[derive(Parser)]
#[command(name = "vectorcut", version, about = "Cut SVG designs on a Silhouette cutter")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List the Silhouette machines plugged in.
    Devices,
    /// List the models, mats and material presets VectorCut knows.
    Models,
    /// Show what a design would cut, without a machine.
    Plan {
        file: PathBuf,
        #[command(flatten)]
        setup: Setup,
        /// Print the GP-GL commands, one per line.
        #[arg(long)]
        transcript: bool,
    },
    /// Cut a design.
    Cut {
        file: PathBuf,
        #[command(flatten)]
        setup: Setup,
        #[command(flatten)]
        target: Target,
        /// Only trace the design's bounding box (placement check).
        #[arg(long)]
        outline: bool,
    },
    /// Cut a 10 mm square with a triangle inside, to check the settings on a material.
    TestCut {
        #[command(flatten)]
        setup: Setup,
        #[command(flatten)]
        target: Target,
    },
    /// Ask the machine for its status, firmware and tools.
    Status {
        #[command(flatten)]
        target: Target,
    },
}

#[derive(Args)]
struct Setup {
    /// Model id (see `vectorcut models`); defaults to the machine plugged in, else cameo4.
    #[arg(long)]
    model: Option<String>,
    /// Mat: none, 12x12, 12x24, 8x12, 15x15, 24x24, 8.5x12. Defaults to the model's usual mat.
    #[arg(long)]
    mat: Option<String>,
    /// Where the design's top-left corner goes, `right,down` in mm.
    #[arg(long, value_parser = parse_pair, default_value = "0,0")]
    offset: (f64, f64),
    /// Material preset (see `vectorcut models`).
    #[arg(long, default_value = "vinyl")]
    preset: String,
    #[arg(long)]
    force: Option<u32>,
    #[arg(long)]
    speed: Option<u32>,
    /// AutoBlade depth 0..=10.
    #[arg(long)]
    depth: Option<u8>,
    #[arg(long)]
    passes: Option<u8>,
    /// Overcut at sharp corners, mm.
    #[arg(long)]
    overcut: Option<f64>,
    /// Tool holder (1 or 2).
    #[arg(long)]
    holder: Option<u8>,
    /// Cut only these colour groups (`#ff0000`, a layer name); repeat for more.
    #[arg(long = "only")]
    only: Vec<String>,
    /// Keep the drawing's order instead of cutting inside-first by shortest travel.
    #[arg(long)]
    keep_order: bool,
}

#[derive(Args)]
struct Target {
    /// Use a simulated machine instead of one on USB.
    #[arg(long)]
    simulate: bool,
    /// Do not ask before cutting.
    #[arg(long, short = 'y')]
    yes: bool,
}

fn parse_pair(s: &str) -> Result<(f64, f64), String> {
    let (a, b) = s.split_once(',').ok_or("expected right,down")?;
    let a: f64 = a.trim().parse().map_err(|_| format!("not a number: {a}"))?;
    let b: f64 = b.trim().parse().map_err(|_| format!("not a number: {b}"))?;
    Ok((a, b))
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("vectorcut: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Command::Devices => devices(),
        Command::Models => {
            models();
            Ok(())
        }
        Command::Plan { file, setup, transcript } => {
            let design = read(&file)?;
            let model = pick_model(setup.model.as_deref(), None)?;
            let plan = make_plan(&design, model, &setup)?;
            describe(&design, &plan);
            let program = plan.encode(&design).map_err(|e| e.to_string())?;
            println!("{} commands, {} bytes", program.commands.len(), program.to_stream().len());
            if transcript {
                for line in program.transcript() {
                    println!("{line}");
                }
            }
            Ok(())
        }
        Command::Cut { file, setup, target, outline } => {
            let design = read(&file)?;
            cut(&design, &setup, &target, outline)
        }
        Command::TestCut { setup, target } => {
            let design = job::test_cut_design(setup.offset.0.max(0.0), setup.offset.1.max(0.0));
            let setup = Setup { offset: (0.0, 0.0), ..setup };
            cut(&design, &setup, &target, false)
        }
        Command::Status { target } => {
            let (mut cutter, _) = connect(&target, None)?;
            println!("{}", cutter.describe());
            if let Some(model) = cutter.model {
                println!("model: {} ({})", model.name, model.id);
            }
            if let Some(firmware) = &cutter.firmware {
                println!("firmware: {firmware}");
            }
            println!("status: {:?}", cutter.status().map_err(|e| e.to_string())?);
            let tools = cutter.tools().map_err(|e| e.to_string())?;
            for (i, tool) in tools.iter().enumerate() {
                println!("holder {}: {}", i + 1, tool.map_or("unknown".to_string(), |t| format!("{t:?}")));
            }
            Ok(())
        }
    }
}

fn read(file: &PathBuf) -> Result<Design, String> {
    let bytes = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let name = file.file_name().map_or_else(|| file.display().to_string(), |n| n.to_string_lossy().into_owned());
    let design = Design::from_bytes(&bytes, &name).map_err(|e| format!("{name}: {e}"))?;
    for warning in &design.warnings {
        eprintln!("warning: {warning}");
    }
    Ok(design)
}

fn devices() -> Result<(), String> {
    let found = vectorcut_usb::find().map_err(|e| e.to_string())?;
    if found.is_empty() {
        println!("No Silhouette machine found on USB.");
    }
    for machine in found {
        println!("{}  [{:04x}:{:04x}]{}", machine.label(), machine.vendor_id, machine.product_id, machine.serial.map(|s| format!("  serial {s}")).unwrap_or_default());
    }
    Ok(())
}

fn models() {
    println!("Models:");
    for m in machine::MODELS {
        let mats: Vec<&str> = m.mats.iter().map(|mat| mat.id()).collect();
        println!(
            "  {:<18} {:<20} {:04x}:{:04x}  {:?}, {} mm wide, {} holder(s), force 1-{}, speed 1-{}, mats {}",
            m.id,
            m.name,
            m.vendor_id,
            m.product_id,
            m.generation,
            m.width_mm,
            if m.two_holders { 2 } else { 1 },
            m.max_force,
            m.max_speed,
            mats.join(" ")
        );
    }
    println!("Presets:");
    for p in machine::PRESETS {
        println!("  {:<14} {:<22} force {:>2}, speed {:>2}, depth {}, passes {}", p.id, p.name, p.force, p.speed, p.depth.map_or("-".to_string(), |d| d.to_string()), p.passes);
    }
}

fn pick_model(id: Option<&str>, connected: Option<&'static Model>) -> Result<&'static Model, String> {
    match (id, connected) {
        (Some(id), _) => Model::by_id(id).ok_or_else(|| format!("unknown model {id}; see `vectorcut models`")),
        (None, Some(model)) => Ok(model),
        (None, None) => Model::by_id("cameo4").ok_or_else(|| "the model table is empty".to_string()),
    }
}

fn make_plan(design: &Design, model: &'static Model, setup: &Setup) -> Result<Plan, String> {
    let mut plan = Plan::new(design, model);
    if let Some(mat) = &setup.mat {
        plan.mat = Mat::parse(mat).ok_or_else(|| format!("unknown mat {mat}"))?;
    }
    plan.offset_right_mm = setup.offset.0;
    plan.offset_down_mm = setup.offset.1;
    plan.optimise_order = !setup.keep_order;
    let preset = Preset::by_id(&setup.preset).ok_or_else(|| format!("unknown preset {}; see `vectorcut models`", setup.preset))?;
    let mut tool = Tool::from_preset(preset, model);
    tool.force = setup.force.unwrap_or(tool.force);
    tool.speed = setup.speed.unwrap_or(tool.speed);
    tool.depth = setup.depth.or(tool.depth);
    tool.passes = setup.passes.unwrap_or(tool.passes);
    tool.overcut_mm = setup.overcut.unwrap_or(tool.overcut_mm);
    tool.holder = setup.holder.unwrap_or(tool.holder);
    for group in &mut plan.groups {
        group.tool = tool.clone();
        group.enabled = setup.only.is_empty() || setup.only.iter().any(|k| k.eq_ignore_ascii_case(&group.key));
    }
    Ok(plan)
}

fn describe(design: &Design, plan: &Plan) {
    let (across, along) = plan.cut_area_mm(design);
    println!("{}: {:.1} × {:.1} mm, {} lines", design.name, design.width_mm, design.height_mm, design.path_count());
    println!("{} on {} — cutting area {across:.1} × {along:.1} mm", plan.model.name, machine::mat_name(plan.mat));
    for (group, choice) in design.groups.iter().zip(&plan.groups) {
        let t = &choice.tool;
        println!(
            "  {} {:<12} {:>4} lines {:>8.1} mm  {}",
            if choice.enabled { "✓" } else { "·" },
            group.key,
            group.paths.len(),
            group.length_mm(),
            if t.pen { format!("pen, holder {}", t.holder) } else { format!("force {}, speed {}, depth {:?}, passes {}, holder {}", t.force, t.speed, t.depth, t.passes, t.holder) }
        );
    }
    let s = plan.summary(design);
    println!("cut {:.0} mm, draw {:.0} mm, travel {:.0} mm", s.cut_mm, s.draw_mm, s.travel_mm);
}

fn connect(target: &Target, model_id: Option<&str>) -> Result<(Cutter<AnyLink>, &'static Model), String> {
    if target.simulate {
        let model = pick_model(model_id, None)?;
        let link: AnyLink = Box::new(SimulatedLink::new(model));
        let cutter = Cutter::connect(link, Some(model)).map_err(|e| e.to_string())?;
        return Ok((cutter, model));
    }
    let found = vectorcut_usb::find().map_err(|e| e.to_string())?;
    let machine = match found.as_slice() {
        [] => return Err("no Silhouette machine found on USB (use --simulate to try without one)".to_string()),
        [one] => one,
        [first, ..] => {
            eprintln!("several machines found; using {}", first.label());
            first
        }
    };
    let link: AnyLink = Box::new(machine.open().map_err(|e| e.to_string())?);
    let cutter = Cutter::connect(link, machine.model).map_err(|e| e.to_string())?;
    let model = pick_model(model_id, cutter.model)?;
    Ok((cutter, model))
}

fn cut(design: &Design, setup: &Setup, target: &Target, outline: bool) -> Result<(), String> {
    let (mut cutter, model) = connect(target, setup.model.as_deref())?;
    let plan = make_plan(design, model, setup)?;
    describe(design, &plan);
    let program = if outline { plan.encode_outline(design) } else { plan.encode(design) }.map_err(|e| e.to_string())?;
    println!("sending to {}", cutter.describe());
    if !target.yes && !confirm("Cut now? [y/N] ")? {
        return Err("not cut".to_string());
    }
    let cancel = AtomicBool::new(false);
    let mut last_percent = None;
    cutter
        .run(&program, RunOptions::default(), &cancel, &mut |event| match event {
            Event::Waiting(status) => eprintln!("waiting for the machine ({status:?}){}", if status == vectorcut_core::Status::Unloaded { ": load the mat or media" } else { "" }),
            Event::Sent { sent, total } => {
                let percent = sent * 100 / total.max(1);
                if last_percent != Some(percent / 10) {
                    last_percent = Some(percent / 10);
                    eprintln!("sent {percent}%");
                }
            }
            Event::SearchingMarks => eprintln!("looking for registration marks"),
            Event::Finishing => eprintln!("finishing"),
        })
        .map_err(|e| e.to_string())?;
    println!("done");
    Ok(())
}

fn confirm(prompt: &str) -> Result<bool, String> {
    print!("{prompt}");
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).map_err(|e| e.to_string())?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}
