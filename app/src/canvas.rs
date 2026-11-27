//! The mat preview: the cutting area to scale with an inch grid, the design's lines where they
//! will be cut, and dragging to move the design.

use craft::egui::{self, Color32, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, Vec2};
use vectorcut_core::{Design, Plan};

/// Millimetres per inch, for the grid.
const INCH_MM: f64 = 25.4;

/// What the preview did this frame.
#[derive(Default)]
pub struct CanvasOutput {
    /// The design was dragged by this much, mm (right, down).
    pub moved_mm: Option<(f64, f64)>,
}

/// The preview's colours, light or dark as the Craft theme resolved the window's appearance.
struct Palette {
    surround: Color32,
    sheet: Color32,
    grid: Color32,
    edge: Color32,
    origin: Color32,
    frame: Color32,
    off_area: Color32,
    /// Lines drawn in black on a dark sheet are lightened to stay visible.
    dark: bool,
}

impl Palette {
    fn new(dark: bool) -> Palette {
        if dark {
            Palette {
                surround: Color32::from_gray(30),
                sheet: Color32::from_gray(48),
                grid: Color32::from_gray(66),
                edge: Color32::from_gray(120),
                origin: Color32::from_rgb(0x4c, 0x9a, 0xff),
                frame: Color32::from_rgb(0x4c, 0x9a, 0xff),
                off_area: Color32::from_rgb(0xff, 0x6b, 0x6b),
                dark,
            }
        } else {
            Palette {
                surround: Color32::from_gray(214),
                sheet: Color32::from_gray(252),
                grid: Color32::from_gray(222),
                edge: Color32::from_gray(150),
                origin: Color32::from_rgb(0x1a, 0x73, 0xe8),
                frame: Color32::from_rgb(0x1a, 0x73, 0xe8),
                off_area: Color32::from_rgb(0xd0, 0x30, 0x30),
                dark,
            }
        }
    }

    fn line(&self, [r, g, b]: [u8; 3], enabled: bool) -> Color32 {
        let (r, g, b) = if self.dark && u16::from(r) + u16::from(g) + u16::from(b) < 120 { (230, 230, 230) } else { (r, g, b) };
        if enabled { Color32::from_rgb(r, g, b) } else { Color32::from_rgba_unmultiplied(r, g, b, 60) }
    }
}

/// Follows a press that starts on the mat until the button is released, by the pointer's own
/// positions. egui's drag tracking needs the widget hovered before the press, which a press that
/// is the window's first pointer event (the window was in the background) does not give.
fn follow_press(ui: &Ui, rect: Rect, id: egui::Id) -> Vec2 {
    let ctx = ui.ctx();
    let (pressed, down, origin, latest) = ctx.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.press_origin(), i.pointer.latest_pos()));
    let mut last: Option<Pos2> = ctx.data(|d| d.get_temp(id)).flatten();
    if pressed
        && let Some(at) = origin
        && rect.contains(at)
        && ctx.layer_id_at(at) == Some(ui.layer_id())
    {
        last = Some(at);
    }
    let mut delta = Vec2::ZERO;
    match (down, last, latest) {
        (true, Some(before), Some(now)) => {
            delta = now - before;
            last = Some(now);
        }
        _ => last = None,
    }
    ctx.data_mut(|d| d.insert_temp(id, last));
    delta
}

/// Paints the preview into `rect`.
pub fn show(ui: &mut Ui, rect: Rect, design: Option<&Design>, plan: Option<&Plan>, area_mm: (f64, f64), dark: bool) -> CanvasOutput {
    // When the body's size may have changed (a section of the settings panel opening or
    // closing), the shell runs the body twice in one pass: a measuring run, then the real one.
    // The mat takes no layout room of its own, so a measuring run has nothing to do here.
    // Interacting in it would register the mat's id twice at different rects (egui's
    // "Second use of widget ID") and apply a drag twice.
    if craft::ui::runtime::measure::active(ui.ctx()) {
        return CanvasOutput::default();
    }
    let response = ui.interact(rect, egui::Id::unique("vectorcut-mat"), Sense::click_and_drag());
    craft::ui::runtime::focus::host_canvas(ui.ctx(), response.id, craft::ui::runtime::focus::KeyUse::CANVAS);
    let dragged = follow_press(ui, rect, response.id.with("press"));
    let pressing = ui.ctx().data(|d| d.get_temp::<Option<Pos2>>(response.id.with("press"))).flatten().is_some();
    let colours = Palette::new(dark);
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, colours.surround);

    let (across, along) = (area_mm.0.max(1.0), area_mm.1.max(1.0));
    let margin = 24.0;
    let room = rect.shrink(margin);
    let scale = (f64::from(room.width()) / across).min(f64::from(room.height()) / along).max(0.01) as f32;
    let size = Vec2::new(across as f32 * scale, along as f32 * scale);
    let origin = Pos2::new(rect.center().x - size.x / 2.0, room.top());
    let to_screen = |(x, y): (f64, f64)| Pos2::new(origin.x + x as f32 * scale, origin.y + y as f32 * scale);
    let mat = Rect::from_min_size(origin, size);

    // The mat: a sheet with an inch grid, like the printed grid on a Silhouette mat.
    painter.rect_filled(mat, 2.0, colours.sheet);
    let mut x = INCH_MM;
    while x < across {
        painter.line_segment([to_screen((x, 0.0)), to_screen((x, along))], Stroke::new(1.0, colours.grid));
        x += INCH_MM;
    }
    let mut y = INCH_MM;
    while y < along {
        painter.line_segment([to_screen((0.0, y)), to_screen((across, y))], Stroke::new(1.0, colours.grid));
        y += INCH_MM;
    }
    painter.rect_stroke(mat, 2.0, Stroke::new(1.0, colours.edge), StrokeKind::Inside);
    // The origin corner, where the blade starts.
    painter.circle_filled(mat.left_top(), 3.0, colours.origin);

    let mut out = CanvasOutput::default();
    let (Some(design), Some(plan)) = (design, plan) else {
        painter.text(mat.center(), egui::Align2::CENTER_CENTER, "Open an SVG, or drop one here", egui::FontId::proportional(15.0), colours.edge);
        return out;
    };

    let offset = (plan.offset_right_mm, plan.offset_down_mm);
    let shift = |(x, y): (f64, f64)| to_screen((x + offset.0, y + offset.1));
    for group in &design.groups {
        let choice = plan.groups.iter().find(|g| g.key == group.key);
        let enabled = choice.is_some_and(|g| g.enabled);
        let pen = choice.is_some_and(|g| g.tool.pen);
        let stroke = Stroke::new(if pen { 1.0 } else { 1.5 }, colours.line(group.colour, enabled));
        for line in &group.paths {
            let points: Vec<Pos2> = line.iter().map(|&p| shift(p)).collect();
            if pen {
                painter.extend(Shape::dashed_line(&points, stroke, 4.0, 3.0));
            } else {
                painter.add(Shape::line(points, stroke));
            }
        }
    }
    // The design's bounding box, which the outline trace follows; red when it runs off the area.
    if let Some((x0, y0, x1, y1)) = design.bounds() {
        let bbox = Rect::from_two_pos(shift((x0, y0)), shift((x1, y1)));
        let off = plan.job(design).is_err() && plan.groups.iter().any(|g| g.enabled);
        let colour = if off { colours.off_area } else { colours.frame };
        painter.rect_stroke(bbox.expand(2.0), 0.0, Stroke::new(1.0, colour.gamma_multiply(0.8)), StrokeKind::Outside);
    }
    if response.hovered() || pressing {
        ui.ctx().set_cursor_icon(if pressing { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab });
    }
    if dragged != Vec2::ZERO {
        out.moved_mm = Some((f64::from(dragged.x / scale), f64::from(dragged.y / scale)));
    }
    out
}
