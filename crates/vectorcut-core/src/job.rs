//! A design, a machine and the user's choices, turned into the bytes the machine runs.
//!
//! [`Plan`] holds what the user picked: the model and mat, where the design sits on the mat,
//! and for each colour group whether it is cut and with which tool and settings. [`Plan::encode`]
//! checks that everything lies on the cutting area, orders each group's lines, builds a
//! `craft-plotter` job and encodes it in the model's GP-GL dialect.

use crate::design::{Design, Polyline, bounds};
use crate::machine::{Model, Preset};
use crate::order;
use craft::plotter::gpgl::silhouette::{self, Mat, Setup};
use craft::plotter::{Job, Operation, Program, ToolSettings};
use serde::{Deserialize, Serialize};

/// How one group is traced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tool {
    /// Tool holder, 1 or 2 (machines with one holder use 1).
    pub holder: u8,
    /// A pen: no blade offset, drawn before anything is cut.
    pub pen: bool,
    /// Force step, 1..=the model's maximum.
    pub force: u32,
    /// Speed step, 1..=the model's maximum.
    pub speed: u32,
    /// AutoBlade depth 0..=10, for machines that set the blade themselves.
    pub depth: Option<u8>,
    /// Times each line is traced, 1..=10.
    pub passes: u8,
    /// Overcut at sharp corners, mm (0..=2).
    pub overcut_mm: f64,
}

impl Tool {
    /// The settings of a material preset, for `model`.
    pub fn from_preset(preset: &Preset, model: &Model) -> Tool {
        Tool {
            holder: 1,
            pen: preset.pen,
            force: preset.force.clamp(1, model.max_force),
            speed: preset.speed_for(model),
            depth: preset.depth,
            passes: preset.passes,
            overcut_mm: if preset.pen { 0.0 } else { 0.3 },
        }
    }
}

impl Default for Tool {
    fn default() -> Self {
        Tool { holder: 1, pen: false, force: 10, speed: 5, depth: Some(1), passes: 1, overcut_mm: 0.3 }
    }
}

/// Whether and how one colour group is traced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupPlan {
    /// The [`crate::design::Group::key`] this applies to.
    pub key: String,
    pub enabled: bool,
    pub tool: Tool,
}

/// Everything needed to cut a design.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub model: Model,
    pub mat: Mat,
    /// Where the design's top-left corner sits, mm from the cutting area's top-left corner.
    pub offset_right_mm: f64,
    pub offset_down_mm: f64,
    /// One entry per design group, in the design's order. Groups without an entry are skipped.
    pub groups: Vec<GroupPlan>,
    /// Order each group's lines for inside-first cutting and short travel.
    pub optimise_order: bool,
    /// Feed past the last cut when done, mm (roll media; the machine parks there).
    pub end_feed_mm: f64,
}

impl Plan {
    /// A plan for `design` on `model` with its usual mat: every group enabled with the default
    /// tool, the design at the top-left of the mat.
    pub fn new(design: &Design, model: &Model) -> Plan {
        let preset = Preset::by_id("vinyl").copied().unwrap_or(crate::machine::PRESETS[0]);
        let tool = Tool::from_preset(&preset, model);
        Plan {
            model: *model,
            mat: model.default_mat(),
            offset_right_mm: 0.0,
            offset_down_mm: 0.0,
            groups: design.groups.iter().map(|g| GroupPlan { key: g.key.clone(), enabled: true, tool: tool.clone() }).collect(),
            optimise_order: true,
            end_feed_mm: 0.0,
        }
    }

    /// Keeps the plan's choices for groups that are still in `design`, and adds the new groups
    /// with `fallback`'s tool. For a design that was reloaded after an edit.
    pub fn follow(&mut self, design: &Design, fallback: &Tool) {
        let old = std::mem::take(&mut self.groups);
        self.groups = design
            .groups
            .iter()
            .map(|g| old.iter().find(|p| p.key == g.key).cloned().unwrap_or_else(|| GroupPlan { key: g.key.clone(), enabled: true, tool: fallback.clone() }))
            .collect();
    }

    /// The area that can be cut, `(across, along)` mm.
    pub fn cut_area_mm(&self, design: &Design) -> (f64, f64) {
        let along = design.bounds().map_or(design.height_mm, |b| b.3) + self.offset_down_mm;
        self.model.cut_area_mm(self.mat, along)
    }

    /// The machine setup the program announces.
    pub fn setup(&self, design: &Design) -> Setup {
        let (across, along) = self.cut_area_mm(design);
        Setup {
            generation: self.model.generation,
            two_holders: self.model.two_holders,
            area_down_mm: along,
            area_right_mm: across,
            mat: self.mat,
            ..Setup::default()
        }
    }

    /// The operations in the order they run: pens first, then blades, each in design order.
    fn operations(&self, design: &Design) -> Vec<Operation> {
        let mut ops: Vec<(bool, Operation)> = Vec::new();
        let mut at = (0.0, 0.0);
        for group in &design.groups {
            let Some(plan) = self.groups.iter().find(|p| p.key == group.key && p.enabled) else {
                continue;
            };
            let paths: Vec<Polyline> = if self.optimise_order { order::order(&group.paths, at) } else { group.paths.clone() };
            if let Some(&end) = paths.last().and_then(|p| p.last()) {
                at = end;
            }
            let tool = &plan.tool;
            let holder = if self.model.two_holders { tool.holder.clamp(1, 2) } else { 1 };
            let settings = ToolSettings {
                force: Some(tool.force.clamp(1, self.model.max_force)),
                speed: Some(tool.speed.clamp(1, self.model.max_speed)),
                passes: tool.passes.clamp(1, craft::plotter::MAX_PASSES),
                depth: if tool.pen { None } else { tool.depth.map(|d| d.min(10)) },
                overcut_mm: if tool.pen { 0.0 } else { tool.overcut_mm.clamp(0.0, craft::plotter::MAX_OVERCUT_MM) },
                ..ToolSettings::default()
            };
            ops.push((tool.pen, Operation { pen: tool.pen, holder, paths, settings }));
        }
        // A stable sort: pens draw before anything is cut loose.
        ops.sort_by_key(|(pen, _)| !*pen);
        ops.into_iter().map(|(_, op)| op).collect()
    }

    /// The craft-plotter job for `design`, after checking it fits the cutting area.
    pub fn job(&self, design: &Design) -> Result<Job, PlanError> {
        let operations = self.operations(design);
        let Some((x0, y0, x1, y1)) = bounds(operations.iter().flat_map(|op| op.paths.iter())) else {
            return Err(PlanError::NothingToCut);
        };
        let (across, along) = self.cut_area_mm(design);
        let (left, top, right, bottom) = (x0 + self.offset_right_mm, y0 + self.offset_down_mm, x1 + self.offset_right_mm, y1 + self.offset_down_mm);
        const SLACK: f64 = 1e-6;
        if left < -SLACK || top < -SLACK || right > across + SLACK || bottom > along + SLACK {
            return Err(PlanError::OffArea { left, top, right, bottom, across, along });
        }
        Ok(Job {
            operations,
            origin_down_mm: self.offset_down_mm,
            origin_right_mm: self.offset_right_mm,
            end_feed_mm: self.end_feed_mm,
            ..Job::default()
        })
    }

    /// The program for `design`: the bytes to send, after the initialise escape.
    pub fn encode(&self, design: &Design) -> Result<Program, PlanError> {
        let job = self.job(design)?;
        silhouette::encode(&self.setup(design), &job).map_err(PlanError::Encode)
    }

    /// A program that only traces the design's bounding box with the first tool raised: the
    /// placement check (`bbox_only`). Use a pen or a low force.
    pub fn encode_outline(&self, design: &Design) -> Result<Program, PlanError> {
        let job = Job { bbox_only: true, ..self.job(design)? };
        silhouette::encode(&self.setup(design), &job).map_err(PlanError::Encode)
    }

    /// What the plan cuts: lines, lengths and lifted travel.
    pub fn summary(&self, design: &Design) -> Summary {
        let ops = self.operations(design);
        let mut s = Summary::default();
        let mut at = (0.0, 0.0);
        for op in &ops {
            let passes = f64::from(op.settings.passes.max(1));
            s.lines += op.paths.len();
            let length: f64 = op.paths.iter().map(|p| crate::design::polyline_length(p)).sum();
            if op.pen {
                s.draw_mm += length * passes;
            } else {
                s.cut_mm += length * passes;
            }
            s.travel_mm += order::travel_mm(&op.paths, at);
            if let Some(&end) = op.paths.last().and_then(|p| p.last()) {
                at = end;
            }
        }
        s.operations = ops.len();
        s
    }
}

/// Totals of a plan.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Summary {
    pub operations: usize,
    pub lines: usize,
    /// Length cut by blades, passes counted, mm.
    pub cut_mm: f64,
    /// Length drawn by pens, mm.
    pub draw_mm: f64,
    /// Distance travelled with the tool raised, mm.
    pub travel_mm: f64,
}

/// Why a plan cannot be cut.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PlanError {
    #[error("no group is enabled, or the enabled groups have no lines")]
    NothingToCut,
    #[error(
        "the design runs off the cutting area: it spans {left:.1}–{right:.1} mm across and {top:.1}–{bottom:.1} mm along, \
         and the area is {across:.1} × {along:.1} mm"
    )]
    OffArea { left: f64, top: f64, right: f64, bottom: f64, across: f64, along: f64 },
    #[error("the job could not be encoded: {0}")]
    Encode(craft::plotter::Error),
}

/// The lines of a test cut at `(right, down)` mm: a 10 mm square with a 5 mm triangle inside,
/// the usual check that the blade cuts through the material and not the backing.
pub fn test_cut(right: f64, down: f64) -> Vec<Polyline> {
    let square = vec![(right, down), (right + 10.0, down), (right + 10.0, down + 10.0), (right, down + 10.0), (right, down)];
    let triangle = vec![(right + 2.5, down + 7.5), (right + 5.0, down + 2.5), (right + 7.5, down + 7.5), (right + 2.5, down + 7.5)];
    vec![triangle, square]
}

/// A design holding only a test cut at `(right, down)` mm, in one group keyed `test`.
pub fn test_cut_design(right: f64, down: f64) -> Design {
    Design {
        name: "Test cut".to_string(),
        width_mm: right + 10.0,
        height_mm: down + 10.0,
        groups: vec![crate::design::Group { key: "test".to_string(), colour: [0xd6, 0x27, 0x28], paths: test_cut(right, down) }],
        warnings: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::machine::Model;

    const DESIGN: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100mm" height="50mm" viewBox="0 0 100 50">
        <rect x="10" y="10" width="20" height="10" fill="none" stroke="#ff0000"/>
        <path d="M 40 10 L 60 10" stroke="blue"/>
    </svg>"##;

    fn cameo4() -> &'static Model {
        Model::by_id("cameo4").unwrap()
    }

    #[test]
    fn a_plan_encodes_to_silhouette_commands() {
        let design = Design::from_svg(DESIGN, "d.svg").unwrap();
        let plan = Plan::new(&design, cameo4());
        let program = plan.encode(&design).unwrap();
        let lines = program.transcript();
        // Cameo 4 line: mat select first, then tool commands, then moves and draws.
        assert!(lines.iter().any(|l| l.starts_with("TG")), "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with('M')), "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with('D')), "{lines:?}");
        assert!(program.to_stream().ends_with(b"\x03"));
    }

    #[test]
    fn pens_run_before_blades_and_disabled_groups_are_left_out() {
        let design = Design::from_svg(DESIGN, "d.svg").unwrap();
        let mut plan = Plan::new(&design, cameo4());
        plan.groups[1].tool.pen = true;
        let job = plan.job(&design).unwrap();
        assert!(job.operations[0].pen && !job.operations[1].pen);
        plan.groups[0].enabled = false;
        assert_eq!(plan.job(&design).unwrap().operations.len(), 1);
        plan.groups[1].enabled = false;
        assert_eq!(plan.job(&design), Err(PlanError::NothingToCut));
    }

    #[test]
    fn a_design_moved_off_the_mat_is_refused() {
        let design = Design::from_svg(DESIGN, "d.svg").unwrap();
        let mut plan = Plan::new(&design, cameo4());
        plan.offset_right_mm = 290.0;
        assert!(matches!(plan.job(&design), Err(PlanError::OffArea { .. })));
        plan.offset_right_mm = -20.0;
        assert!(matches!(plan.job(&design), Err(PlanError::OffArea { .. })));
        plan.offset_right_mm = 200.0;
        assert!(plan.job(&design).is_ok());
    }

    #[test]
    fn settings_are_kept_in_the_models_range() {
        let design = Design::from_svg(DESIGN, "d.svg").unwrap();
        let mut plan = Plan::new(&design, Model::by_id("cameo3").unwrap());
        plan.groups[0].tool.speed = 30;
        plan.groups[0].tool.holder = 7;
        let job = plan.job(&design).unwrap();
        assert_eq!(job.operations[0].settings.speed, Some(10));
        assert_eq!(job.operations[0].holder, 2);
    }

    #[test]
    fn following_a_reloaded_design_keeps_known_groups() {
        let design = Design::from_svg(DESIGN, "d.svg").unwrap();
        let mut plan = Plan::new(&design, cameo4());
        plan.groups[0].tool.force = 22;
        plan.groups[1].enabled = false;
        let edited = DESIGN.replace("blue", "green");
        let design = Design::from_svg(&edited, "d.svg").unwrap();
        plan.follow(&design, &Tool::default());
        assert_eq!(plan.groups[0].tool.force, 22);
        assert_eq!(plan.groups[1].key, "#008000");
        assert!(plan.groups[1].enabled);
    }

    #[test]
    fn the_test_cut_is_a_square_round_a_triangle() {
        let design = test_cut_design(5.0, 5.0);
        let plan = Plan::new(&design, cameo4());
        let s = plan.summary(&design);
        assert_eq!(s.lines, 2);
        assert!(s.cut_mm > 40.0);
        // The triangle, inside the square, is cut first.
        let job = plan.job(&design).unwrap();
        assert_eq!(job.operations[0].paths[0].len(), 4);
    }
}
