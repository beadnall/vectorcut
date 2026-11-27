//! The machines VectorCut knows: USB ids, which generation of the Silhouette dialect each speaks,
//! how wide it cuts, its tool holders and the range of its settings.
//!
//! The ids, widths and ranges are the ones the community driver projects publish
//! (inkscape-silhouette's device table). They are a starting point: the cutting widths of the
//! larger machines in particular are approximate, and none of this has been checked on every
//! model. A machine not listed is still found by vendor id and identified by its firmware reply.

use craft::plotter::gpgl::silhouette::{Generation, Mat};
use serde::{Deserialize, Serialize};

/// Graphtec's USB vendor id, which Silhouette machines carry.
pub const VENDOR_GRAPHTEC: u16 = 0x0b4d;
/// The vendor id the Cameo 5 Alpha machines carry.
pub const VENDOR_CAMEO5_ALPHA: u16 = 0x3844;

/// One machine model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Model {
    /// Stable id, for settings files (`cameo4`).
    pub id: &'static str,
    /// The name people know it by.
    pub name: &'static str,
    pub vendor_id: u16,
    pub product_id: u16,
    pub generation: Generation,
    /// Two tool holders (the Cameo 3 and later Cameos).
    pub two_holders: bool,
    /// Width the machine cuts across, mm.
    pub width_mm: f64,
    /// Longest cut the machine feeds without a mat, mm.
    pub max_length_mm: f64,
    /// Highest force step.
    pub max_force: u32,
    /// Highest speed step.
    pub max_speed: u32,
    /// The mats the machine takes, the usual one first.
    pub mats: &'static [Mat],
}

const CAMEO_MATS: &[Mat] = &[Mat::Cameo12x12, Mat::Cameo12x24, Mat::None];
const PLUS_MATS: &[Mat] = &[Mat::CameoPlus15x15, Mat::Cameo12x12, Mat::Cameo12x24, Mat::None];
const PRO_MATS: &[Mat] = &[Mat::CameoPro24x24, Mat::Cameo12x12, Mat::Cameo12x24, Mat::None];
const PORTRAIT_MATS: &[Mat] = &[Mat::Portrait8x12, Mat::Portrait8_5x12, Mat::None];

const fn model(id: &'static str, name: &'static str, product_id: u16, generation: Generation, width_mm: f64, mats: &'static [Mat]) -> Model {
    // The Cameo 3 and later Cameos have two holders; the Portraits override this to one.
    let two_holders = !matches!(generation, Generation::Legacy);
    let max_speed = if matches!(generation, Generation::Cameo4) { 30 } else { 10 };
    Model { id, name, vendor_id: VENDOR_GRAPHTEC, product_id, generation, two_holders, width_mm, max_length_mm: 3000.0, max_force: 33, max_speed, mats }
}

/// Every model VectorCut knows, newest line first.
pub const MODELS: &[Model] = &[
    model("cameo5", "Cameo 5", 0x1140, Generation::Cameo4, 330.2, CAMEO_MATS),
    model("cameo5-plus", "Cameo 5 Plus", 0x1141, Generation::Cameo4, 372.0, PLUS_MATS),
    Model { vendor_id: VENDOR_CAMEO5_ALPHA, max_force: 40, ..model("cameo5-alpha", "Cameo 5 Alpha", 0x0001, Generation::Cameo4, 330.2, CAMEO_MATS) },
    Model { vendor_id: VENDOR_CAMEO5_ALPHA, max_force: 40, ..model("cameo5-alpha-plus", "Cameo 5 Alpha Plus", 0x0002, Generation::Cameo4, 372.0, PLUS_MATS) },
    Model { max_length_mm: 18290.0, ..model("cameo-pro-mk2", "Cameo Pro MK II", 0x1146, Generation::Cameo4, 609.0, PRO_MATS) },
    model("cameo4", "Cameo 4", 0x1137, Generation::Cameo4, 304.8, CAMEO_MATS),
    model("cameo4-plus", "Cameo 4 Plus", 0x1138, Generation::Cameo4, 372.0, PLUS_MATS),
    model("cameo4-pro", "Cameo 4 Pro", 0x1139, Generation::Cameo4, 600.0, PRO_MATS),
    model("cameo3", "Cameo 3", 0x112f, Generation::Cameo3, 304.8, CAMEO_MATS),
    model("cameo2", "Cameo 2", 0x112b, Generation::Legacy, 304.0, CAMEO_MATS),
    model("cameo", "Cameo", 0x1121, Generation::Legacy, 304.0, CAMEO_MATS),
    Model { max_length_mm: 18290.0, two_holders: false, ..model("portrait4", "Portrait 4", 0x113f, Generation::Cameo4, 216.0, PORTRAIT_MATS) },
    Model { max_length_mm: 18290.0, two_holders: false, ..model("portrait3", "Portrait 3", 0x113a, Generation::Cameo4, 203.0, PORTRAIT_MATS) },
    model("portrait2", "Portrait 2", 0x1132, Generation::Legacy, 203.0, PORTRAIT_MATS),
    model("portrait", "Portrait", 0x1123, Generation::Legacy, 206.0, PORTRAIT_MATS),
];

impl Model {
    /// The model with USB ids `vendor_id`:`product_id`.
    pub fn by_usb(vendor_id: u16, product_id: u16) -> Option<&'static Model> {
        MODELS.iter().find(|m| m.vendor_id == vendor_id && m.product_id == product_id)
    }

    /// The model with settings id `id`.
    pub fn by_id(id: &str) -> Option<&'static Model> {
        MODELS.iter().find(|m| m.id == id)
    }

    /// The model a firmware reply names (`CAMEO 4 PRO V1.02`): the longest model name the reply
    /// starts with, ignoring case, spaces and the word "Silhouette".
    pub fn by_firmware(reply: &str) -> Option<&'static Model> {
        let squash = |s: &str| s.to_ascii_lowercase().replace("silhouette", "").chars().filter(char::is_ascii_alphanumeric).collect::<String>();
        let reply = squash(reply);
        MODELS.iter().filter(|m| reply.starts_with(&squash(m.name))).max_by_key(|m| m.name.len())
    }

    /// Whether `vendor_id` is one a Silhouette machine carries.
    pub fn is_silhouette_vendor(vendor_id: u16) -> bool {
        vendor_id == VENDOR_GRAPHTEC || vendor_id == VENDOR_CAMEO5_ALPHA
    }

    /// The mat the machine is usually used with.
    pub fn default_mat(&self) -> Mat {
        self.mats.first().copied().unwrap_or(Mat::None)
    }

    /// The area that can be cut on `mat`, `(across, along)` in mm: the mat's extent, limited to
    /// the machine's width; without a mat, the machine's width by `length_mm`.
    pub fn cut_area_mm(&self, mat: Mat, length_mm: f64) -> (f64, f64) {
        if mat == Mat::None {
            return (self.width_mm, length_mm.clamp(1.0, self.max_length_mm));
        }
        let (down, right) = mat.extent_mm();
        (right.min(self.width_mm), down)
    }
}

/// A starting point for a material. Always make a test cut: blades wear and materials differ.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    pub force: u32,
    /// Speed on the 1..=10 scale of the older machines; [`Preset::speed_for`] maps it.
    pub speed: u32,
    /// AutoBlade depth, 1..=10; `None` for a pen.
    pub depth: Option<u8>,
    pub passes: u8,
    pub pen: bool,
}

impl Preset {
    /// The speed step for `model`: the Cameo 4 line runs 1..=30, where the older scale's 10 is
    /// the fastest that suits most materials, so the value is kept as it is.
    pub fn speed_for(&self, model: &Model) -> u32 {
        self.speed.clamp(1, model.max_speed)
    }
}

/// Material presets, from the media table the community drivers publish.
pub const PRESETS: &[Preset] = &[
    Preset { id: "vinyl", name: "Vinyl sticker", force: 10, speed: 5, depth: Some(1), passes: 1, pen: false },
    Preset { id: "htv", name: "Heat transfer vinyl", force: 14, speed: 5, depth: Some(2), passes: 1, pen: false },
    Preset { id: "copy-paper", name: "Copy paper (light)", force: 5, speed: 10, depth: Some(1), passes: 1, pen: false },
    Preset { id: "cardstock", name: "Cardstock (90 g)", force: 30, speed: 10, depth: Some(3), passes: 1, pen: false },
    Preset { id: "thick-card", name: "Thick card", force: 27, speed: 10, depth: Some(6), passes: 2, pen: false },
    Preset { id: "sticker-sheet", name: "Sticker sheet", force: 20, speed: 10, depth: Some(1), passes: 1, pen: false },
    Preset { id: "magnet", name: "Magnetic sheet", force: 30, speed: 3, depth: Some(10), passes: 2, pen: false },
    Preset { id: "pen", name: "Sketch pen", force: 18, speed: 10, depth: None, passes: 1, pen: true },
];

impl Preset {
    pub fn by_id(id: &str) -> Option<&'static Preset> {
        PRESETS.iter().find(|p| p.id == id)
    }
}

/// The short name of a mat, for people.
pub fn mat_name(mat: Mat) -> &'static str {
    match mat {
        Mat::None => "No mat (roll)",
        Mat::Cameo12x12 => "12 × 12 in",
        Mat::Cameo12x24 => "12 × 24 in",
        Mat::Portrait8x12 => "8 × 12 in",
        Mat::CameoPlus15x15 => "15 × 15 in",
        Mat::CameoPro24x24 => "24 × 24 in",
        Mat::Portrait8_5x12 => "8.5 × 12 in",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn usb_ids_and_firmware_replies_name_the_model() {
        assert_eq!(Model::by_usb(0x0b4d, 0x1137).unwrap().id, "cameo4");
        assert_eq!(Model::by_firmware("CAMEO 4 PRO V1.02").unwrap().id, "cameo4-pro");
        assert_eq!(Model::by_firmware("CAMEO 4 V1.10").unwrap().id, "cameo4");
        assert_eq!(Model::by_firmware("Silhouette Cameo 3 V1.40").unwrap().id, "cameo3");
        assert!(Model::by_firmware("Something else").is_none());
    }

    #[test]
    fn model_ids_are_unique() {
        let mut ids: Vec<&str> = MODELS.iter().map(|m| m.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), MODELS.len());
    }

    #[test]
    fn portraits_have_one_holder_and_cameo3_on_have_two() {
        assert!(!Model::by_id("portrait4").unwrap().two_holders);
        assert!(Model::by_id("cameo3").unwrap().two_holders);
        assert!(!Model::by_id("cameo2").unwrap().two_holders);
    }

    #[test]
    fn the_cut_area_is_the_mat_within_the_machine() {
        let cameo4 = Model::by_id("cameo4").unwrap();
        let (across, along) = cameo4.cut_area_mm(Mat::Cameo12x24, 0.0);
        assert!((across - 304.8).abs() < 1e-9 && (along - 609.6).abs() < 1e-9, "{across} {along}");
        assert_eq!(cameo4.cut_area_mm(Mat::None, 500.0), (304.8, 500.0));
    }
}
