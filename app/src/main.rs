//! VectorCut for the desktop: open an SVG, place it on the mat, pick settings per colour, cut.
//!
//! Built on the ArtCraft framework (`craft-libs`): the window, shell, controls and theme are the
//! Craft apps' own; the cutting engine is `vectorcut-core` and the machine link `vectorcut-usb`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod canvas;
mod worker;

use craft::behavior::placement::Defaults;
use craft::ui::shell::app::{AppSpec, run};
use std::path::PathBuf;

const APP_ID: &str = "org.vectorcut.VectorCut";
const TITLE: &str = "VectorCut";

fn main() {
    // `VectorCut file.svg` opens the file; `--simulate` starts with a simulated machine.
    let mut open: Option<PathBuf> = None;
    let mut simulate = false;
    for arg in std::env::args_os().skip(1) {
        if arg == "--simulate" {
            simulate = true;
        } else if !arg.to_string_lossy().starts_with("-psn_") {
            open = Some(PathBuf::from(arg));
        }
    }
    let spec = AppSpec::new(APP_ID, TITLE, Defaults::new([1280.0, 820.0], [900.0, 600.0]));
    if let Err(error) = run(spec, move |ctx| app::VectorCut::new(ctx, open, simulate)) {
        eprintln!("VectorCut: {error}");
        std::process::exit(1);
    }
}
