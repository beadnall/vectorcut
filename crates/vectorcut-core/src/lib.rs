//! VectorCut's engine: SVG designs in, Silhouette GP-GL programs out. No I/O and no UI.
//!
//! - [`design`]: reads an SVG as polylines in millimetres, grouped by stroke colour.
//! - [`order`]: orders a group's lines, inside first, then by shortest travel.
//! - [`machine`]: the models, their USB ids, widths, mats and setting ranges, and material presets.
//! - [`job`]: the user's choices ([`job::Plan`]) encoded through `craft-plotter`.
//!
//! The GP-GL encoding itself is `craft-plotter`'s (from the ArtCraft `craft-libs`), which writes
//! the Silhouette dialect of each machine generation.

pub mod design;
pub mod job;
pub mod machine;
pub mod order;

pub use craft::plotter::gpgl::silhouette::{self, Generation, Mat, Status};
pub use craft::plotter::{Command, Program};
pub use design::{Design, DesignError, Group, Polyline};
pub use job::{GroupPlan, Plan, PlanError, Summary, Tool};
pub use machine::{MODELS, Model, PRESETS, Preset};

/// The bytes of `program` as the sender writes them: one part, or two around the pause where
/// the machine looks for registration marks (send the first, wait for Ready, send the second).
pub fn program_parts(program: &Program) -> Vec<Vec<u8>> {
    if program.registration_wait.is_none() {
        return vec![program.to_stream()];
    }
    let (before, after) = program.split_at_registration();
    vec![craft::plotter::stream(before, program.framing), craft::plotter::stream(after, program.framing)]
}

/// Splits a GP-GL byte stream into pieces of at most `max` bytes that each end on a command's
/// ETX terminator, so a command is never cut in two. A single command longer than `max` (a long
/// draw command) is its own piece.
pub fn chunks(stream: &[u8], max: usize) -> Vec<&[u8]> {
    const ETX: u8 = 0x03;
    let mut out = Vec::new();
    let mut start = 0;
    while start < stream.len() {
        let window_end = (start + max.max(1)).min(stream.len());
        let window = &stream[start..window_end];
        let end = if window_end == stream.len() {
            window_end
        } else {
            match window.iter().rposition(|&b| b == ETX) {
                Some(i) => start + i + 1,
                // No terminator in reach: run on to the end of this command.
                None => stream[window_end..].iter().position(|&b| b == ETX).map_or(stream.len(), |i| window_end + i + 1),
            }
        };
        out.push(&stream[start..end]);
        start = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::chunks;

    #[test]
    fn chunks_end_on_command_boundaries() {
        let stream = b"M10,10\x03D20,20,30,30\x03D40,40\x03";
        let parts = chunks(stream, 12);
        assert_eq!(parts.concat(), stream.to_vec());
        assert_eq!(parts[0], b"M10,10\x03");
        assert!(parts.iter().all(|p| p.ends_with(b"\x03")));
        // A command longer than the limit is sent whole.
        assert_eq!(parts[1], b"D20,20,30,30\x03");
    }
}
