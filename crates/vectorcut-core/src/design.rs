//! An SVG drawing read as cut lines: polylines in millimetres, grouped by stroke colour.
//!
//! The geometry comes from `craft-svg`'s reader, which applies transforms, viewport units,
//! `<use>` instances, the CSS cascade and clip paths. Every drawable element becomes a cut line,
//! whether it is stroked, filled or both; what decides the tool is the stroke colour (or the
//! Inkscape layer, when the drawing has layers and no stroke colours), the convention cutting
//! software shares. Text that is still text is not read: convert it to paths first.

use craft::svg::paths::{self, Shape};
use kurbo::{BezPath, PathEl};

/// Largest SVG file read (craft-svg has its own, tighter limits on nodes and depth).
pub const MAX_FILE_BYTES: usize = craft::svg::paths::MAX_SVG_BYTES;

/// Default flattening tolerance, mm: well under the machine's 0.05 mm step.
pub const FLATTEN_TOLERANCE_MM: f64 = 0.01;

/// A polyline in mm, `(x right, y down)` from the drawing's top-left corner.
pub type Polyline = Vec<(f64, f64)>;

/// Why a drawing could not be read.
#[derive(Debug, thiserror::Error)]
pub enum DesignError {
    #[error("{0}")]
    Svg(String),
    #[error("the file is not UTF-8 text")]
    NotText,
    #[error("the file is larger than {MAX_FILE_BYTES} bytes")]
    TooLarge,
    #[error("the drawing has no shapes to cut")]
    Empty,
}

/// The shapes that share one stroke colour (or layer): one tool, one set of settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    /// What the shapes share: `#rrggbb`, a layer name, or [`Group::UNSTROKED`].
    pub key: String,
    /// The colour to draw the group's lines in, as `[r, g, b]`.
    pub colour: [u8; 3],
    pub paths: Vec<Polyline>,
}

impl Group {
    /// The key of shapes with no stroke colour outside any layer.
    pub const UNSTROKED: &'static str = "no stroke";

    /// The total length of the group's lines, mm.
    pub fn length_mm(&self) -> f64 {
        self.paths.iter().map(|p| polyline_length(p)).sum()
    }
}

/// A drawing read for cutting.
#[derive(Debug, Clone, PartialEq)]
pub struct Design {
    /// The name the drawing was read under (its file name).
    pub name: String,
    /// The drawing's page size, mm.
    pub width_mm: f64,
    pub height_mm: f64,
    /// Groups in first-seen order.
    pub groups: Vec<Group>,
    /// What the reader skipped or approximated.
    pub warnings: Vec<String>,
}

impl Design {
    /// Reads an SVG file's bytes.
    pub fn from_bytes(bytes: &[u8], name: &str) -> Result<Design, DesignError> {
        if bytes.len() > MAX_FILE_BYTES {
            return Err(DesignError::TooLarge);
        }
        let text = std::str::from_utf8(bytes).map_err(|_| DesignError::NotText)?;
        Design::from_svg(text, name)
    }

    /// Reads SVG text. `name` labels errors and warnings.
    pub fn from_svg(svg: &str, name: &str) -> Result<Design, DesignError> {
        let imported: paths::Imported<()> = paths::import(svg, name).map_err(|e| DesignError::Svg(e.to_string()))?;
        let mut warnings = imported.warnings.clone();
        if imported.dropped_warnings > 0 {
            warnings.push(format!("{} more warnings not shown", imported.dropped_warnings));
        }
        let by_layer = !imported.shapes.iter().any(|s| s.has_stroke) && imported.shapes.iter().any(|s| s.layer.is_some());
        let mut groups: Vec<Group> = Vec::new();
        for shape in &imported.shapes {
            let path = if shape.clips.is_empty() {
                shape.path.clone()
            } else {
                match imported.clipped_path(shape) {
                    Ok(path) => path,
                    Err(error) => {
                        warnings.push(format!("{}: clip not applied ({error})", shape.label));
                        shape.path.clone()
                    }
                }
            };
            let lines = flatten(&path, FLATTEN_TOLERANCE_MM);
            if lines.is_empty() {
                continue;
            }
            let (key, colour) = group_key(shape, by_layer);
            match groups.iter_mut().find(|g| g.key == key) {
                Some(group) => group.paths.extend(lines),
                None => groups.push(Group { key, colour, paths: lines }),
            }
        }
        if groups.is_empty() {
            return Err(DesignError::Empty);
        }
        Ok(Design { name: name.to_string(), width_mm: imported.width_mm, height_mm: imported.height_mm, groups, warnings })
    }

    /// The bounding box of every line, `(min_x, min_y, max_x, max_y)` in mm.
    pub fn bounds(&self) -> Option<(f64, f64, f64, f64)> {
        bounds(self.groups.iter().flat_map(|g| g.paths.iter()))
    }

    /// How many lines the drawing has.
    pub fn path_count(&self) -> usize {
        self.groups.iter().map(|g| g.paths.len()).sum()
    }
}

/// The group a shape belongs to and the colour it is drawn in.
fn group_key(shape: &Shape<()>, by_layer: bool) -> (String, [u8; 3]) {
    if by_layer && let Some(layer) = &shape.layer {
        return (layer.clone(), layer_colour(layer));
    }
    let parsed = shape.stroke.as_deref().filter(|_| shape.has_stroke).and_then(|s| craft::svg::color::parse_color(s).ok());
    match parsed {
        Some(c) => (format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b), [c.r, c.g, c.b]),
        None => (Group::UNSTROKED.to_string(), [0, 0, 0]),
    }
}

/// A stable colour for a layer name, so layers stay told apart in the preview.
fn layer_colour(name: &str) -> [u8; 3] {
    const PALETTE: [[u8; 3]; 6] = [[0x1f, 0x77, 0xb4], [0xd6, 0x27, 0x28], [0x2c, 0xa0, 0x2c], [0x94, 0x67, 0xbd], [0xff, 0x7f, 0x0e], [0x17, 0xbe, 0xcf]];
    let hash = name.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
    PALETTE[(hash as usize) % PALETTE.len()]
}

/// Flattens a Bézier path into polylines within `tolerance` mm. A closed subpath ends where it
/// started; subpaths of fewer than two distinct points are dropped.
pub fn flatten(path: &BezPath, tolerance: f64) -> Vec<Polyline> {
    let mut out: Vec<Polyline> = Vec::new();
    let mut current: Polyline = Vec::new();
    let finish = |current: &mut Polyline, out: &mut Vec<Polyline>| {
        let line = std::mem::take(current);
        if line.len() >= 2 && line.iter().any(|p| *p != line[0]) {
            out.push(line);
        }
    };
    kurbo::flatten(path, tolerance, |el| match el {
        PathEl::MoveTo(p) => {
            finish(&mut current, &mut out);
            current.push((p.x, p.y));
        }
        PathEl::LineTo(p) => {
            if !current.last().is_some_and(|&q| near(q, (p.x, p.y))) {
                current.push((p.x, p.y));
            }
        }
        PathEl::ClosePath => {
            if let Some(&first) = current.first() {
                // A last point a rounding error from the first is the first: a closed line
                // ends exactly where it starts, with no hair-length closing segment.
                match current.last() {
                    Some(&last) if last == first => {}
                    Some(&last) if near(last, first) && current.len() > 2 => {
                        if let Some(end) = current.last_mut() {
                            *end = first;
                        }
                    }
                    _ => current.push(first),
                }
            }
            finish(&mut current, &mut out);
        }
        // kurbo::flatten emits only the three element kinds above.
        PathEl::QuadTo(..) | PathEl::CurveTo(..) => {}
    });
    finish(&mut current, &mut out);
    out
}

/// Points closer than this are one point, mm (far below the machine's 0.05 mm step).
const SAME_POINT_MM: f64 = 1e-6;

fn near(a: (f64, f64), b: (f64, f64)) -> bool {
    (a.0 - b.0).hypot(a.1 - b.1) < SAME_POINT_MM
}

/// The length of a polyline, mm.
pub fn polyline_length(path: &[(f64, f64)]) -> f64 {
    path.windows(2).map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1)).sum()
}

/// The bounding box of some polylines, `(min_x, min_y, max_x, max_y)`.
pub fn bounds<'a>(paths: impl IntoIterator<Item = &'a Polyline>) -> Option<(f64, f64, f64, f64)> {
    let mut bb: Option<(f64, f64, f64, f64)> = None;
    for &(x, y) in paths.into_iter().flatten() {
        bb = Some(match bb {
            None => (x, y, x, y),
            Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
        });
    }
    bb
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const TWO_COLOURS: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100mm" height="50mm" viewBox="0 0 100 50">
        <rect x="10" y="10" width="20" height="10" fill="none" stroke="#ff0000"/>
        <circle cx="60" cy="25" r="10" fill="none" stroke="blue"/>
        <path d="M 80 5 L 95 5" stroke="red"/>
    </svg>"##;

    #[test]
    fn groups_shapes_by_stroke_colour_in_millimetres() {
        let design = Design::from_svg(TWO_COLOURS, "two.svg").unwrap();
        assert_eq!((design.width_mm, design.height_mm), (100.0, 50.0));
        let keys: Vec<&str> = design.groups.iter().map(|g| g.key.as_str()).collect();
        assert_eq!(keys, ["#ff0000", "#0000ff"]);
        assert_eq!(design.groups[0].paths.len(), 2);
        // The rectangle is 60 mm round.
        let rect = &design.groups[0].paths[0];
        assert!((polyline_length(rect) - 60.0).abs() < 1e-6, "{}", polyline_length(rect));
        assert_eq!(rect.first(), rect.last());
        // The circle flattens to within the tolerance of 2πr.
        let circle = polyline_length(&design.groups[1].paths[0]);
        assert!((circle - std::f64::consts::TAU * 10.0).abs() < 0.05, "{circle}");
    }

    #[test]
    fn closed_curves_end_exactly_where_they_start() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="40mm" height="40mm" viewBox="0 0 40 40"><circle cx="20" cy="20" r="13" stroke="black" fill="none"/></svg>"#;
        let design = Design::from_svg(svg, "c.svg").unwrap();
        let circle = &design.groups[0].paths[0];
        assert_eq!(circle.first(), circle.last());
        let shortest = circle.windows(2).map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1)).fold(f64::INFINITY, f64::min);
        assert!(shortest > 1e-3, "no hair-length segment: {shortest}");
    }

    #[test]
    fn filled_shapes_without_stroke_are_cut_too() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10mm" height="10mm" viewBox="0 0 10 10"><rect width="4" height="2"/></svg>"#;
        let design = Design::from_svg(svg, "fill.svg").unwrap();
        assert_eq!(design.groups.len(), 1);
        assert_eq!(design.groups[0].key, Group::UNSTROKED);
    }

    #[test]
    fn a_drawing_with_nothing_to_cut_is_an_error() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10mm" height="10mm"/>"#;
        assert!(matches!(Design::from_svg(svg, "empty.svg"), Err(DesignError::Empty)));
    }
}
