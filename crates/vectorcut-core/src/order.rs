//! The order a blade traces a group's lines in.
//!
//! Two rules, in this order:
//!
//! 1. **Inside first.** A closed line that lies inside another closed line is cut before it, so a
//!    piece is never cut free of the media while its holes are still to be cut (the letter `o`).
//!    "Inside" is judged by bounding boxes, which is exact for nested outlines and errs toward
//!    cutting a neighbour first otherwise; either way nothing is cut twice or left out.
//! 2. **Shortest travel.** Of the lines rule 1 allows next, the one whose start is nearest to where
//!    the blade is goes next. A closed line may start at any of its points, so it is turned to
//!    start at the point nearest the blade; an open line may be traced from either end.
//!
//! The greedy search is quadratic in the number of lines; past [`MAX_ORDERED`] lines the input
//! order is kept, with only rule 1 applied by a stable sort on nesting depth.

use crate::design::Polyline;

/// Most lines the travel search orders.
pub const MAX_ORDERED: usize = 20_000;

/// How close two points are to count as one, mm.
const SAME_POINT_MM: f64 = 1e-6;

fn is_closed(path: &Polyline) -> bool {
    path.len() > 2 && path.first().zip(path.last()).is_some_and(|(a, b)| (a.0 - b.0).hypot(a.1 - b.1) < SAME_POINT_MM)
}

type BBox = (f64, f64, f64, f64);

fn bbox(path: &Polyline) -> BBox {
    path.iter().fold((f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY), |(x0, y0, x1, y1), &(x, y)| {
        (x0.min(x), y0.min(y), x1.max(x), y1.max(y))
    })
}

/// `inner` lies within `outer` and is smaller.
fn inside(inner: &BBox, outer: &BBox) -> bool {
    let within = inner.0 >= outer.0 && inner.1 >= outer.1 && inner.2 <= outer.2 && inner.3 <= outer.3;
    let smaller = (inner.2 - inner.0) * (inner.3 - inner.1) < (outer.2 - outer.0) * (outer.3 - outer.1);
    within && smaller
}

fn distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

/// Turns a closed line to start (and end) at its point `at`.
fn rotate_closed(path: &Polyline, at: usize) -> Polyline {
    if at == 0 {
        return path.clone();
    }
    // The last point repeats the first; drop it, turn, and close again.
    let ring = &path[..path.len() - 1];
    let mut out: Polyline = ring[at..].iter().chain(&ring[..at]).copied().collect();
    out.push(ring[at]);
    out
}

/// Orders `paths` for cutting, starting from `from` (the blade's position, mm). Returns the lines
/// in cutting order, each possibly reversed or turned to start elsewhere; the set of lines and
/// their shapes are unchanged.
pub fn order(paths: &[Polyline], from: (f64, f64)) -> Vec<Polyline> {
    let n = paths.len();
    let closed: Vec<bool> = paths.iter().map(is_closed).collect();
    let boxes: Vec<BBox> = paths.iter().map(bbox).collect();
    // inner[i]: the closed lines inside line i, which must go before it.
    let mut blockers = vec![0usize; n];
    let mut contains: Vec<Vec<usize>> = vec![Vec::new(); n];
    if n <= MAX_ORDERED {
        for i in 0..n {
            if !closed[i] {
                continue;
            }
            for j in 0..n {
                if i != j && inside(&boxes[j], &boxes[i]) {
                    blockers[i] += 1;
                    contains[j].push(i);
                }
            }
        }
    } else {
        let mut by_depth: Vec<usize> = (0..n).collect();
        // Without the pairwise search, smaller boxes first approximates inside-first.
        by_depth.sort_by(|&a, &b| {
            let area = |k: usize| (boxes[k].2 - boxes[k].0) * (boxes[k].3 - boxes[k].1);
            area(a).total_cmp(&area(b))
        });
        return by_depth.into_iter().map(|i| paths[i].clone()).collect();
    }

    let mut done = vec![false; n];
    let mut out = Vec::with_capacity(n);
    let mut at = from;
    for _ in 0..n {
        // The nearest line whose inner lines are all cut.
        let mut best: Option<(f64, usize, usize, bool)> = None; // (distance, line, start point, reversed)
        for i in (0..n).filter(|&i| !done[i] && blockers[i] == 0) {
            let path = &paths[i];
            let candidate = if closed[i] {
                path.iter().enumerate().take(path.len() - 1).map(|(k, &p)| (distance(at, p), i, k, false)).min_by(|a, b| a.0.total_cmp(&b.0))
            } else {
                let (first, last) = (path[0], path[path.len() - 1]);
                let (df, dl) = (distance(at, first), distance(at, last));
                Some(if dl < df { (dl, i, 0, true) } else { (df, i, 0, false) })
            };
            if let Some(c) = candidate
                && best.is_none_or(|b| c.0 < b.0)
            {
                best = Some(c);
            }
        }
        // Mutually containing boxes (identical outlines) would block each other: take the
        // first remaining line then.
        let (_, i, start, reversed) = best.unwrap_or_else(|| {
            let i = (0..n).find(|&i| !done[i]).unwrap_or(0);
            (0.0, i, 0, false)
        });
        let line = if closed[i] {
            rotate_closed(&paths[i], start)
        } else if reversed {
            paths[i].iter().rev().copied().collect()
        } else {
            paths[i].clone()
        };
        at = line.last().copied().unwrap_or(at);
        done[i] = true;
        for &outer in &contains[i] {
            blockers[outer] = blockers[outer].saturating_sub(1);
        }
        out.push(line);
    }
    out
}

/// The distance the blade travels lifted between lines, starting from `from`, mm.
pub fn travel_mm(paths: &[Polyline], from: (f64, f64)) -> f64 {
    let mut at = from;
    let mut total = 0.0;
    for path in paths {
        if let (Some(&first), Some(&last)) = (path.first(), path.last()) {
            total += distance(at, first);
            at = last;
        }
    }
    total
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn square(x: f64, y: f64, s: f64) -> Polyline {
        vec![(x, y), (x + s, y), (x + s, y + s), (x, y + s), (x, y)]
    }

    #[test]
    fn holes_are_cut_before_the_outline_round_them() {
        let outer = square(0.0, 0.0, 50.0);
        let hole = square(20.0, 20.0, 10.0);
        let ordered = order(&[outer.clone(), hole.clone()], (0.0, 0.0));
        assert_eq!(bbox(&ordered[0]), bbox(&hole));
        assert_eq!(bbox(&ordered[1]), bbox(&outer));
    }

    #[test]
    fn nearest_line_goes_next_and_open_lines_may_run_backwards() {
        let far = vec![(100.0, 0.0), (110.0, 0.0)];
        let near = vec![(20.0, 0.0), (10.0, 0.0)];
        let ordered = order(&[far, near], (0.0, 0.0));
        assert_eq!(ordered[0], vec![(10.0, 0.0), (20.0, 0.0)]);
        assert_eq!(ordered[1], vec![(100.0, 0.0), (110.0, 0.0)]);
    }

    #[test]
    fn a_closed_line_starts_at_its_point_nearest_the_blade() {
        let ordered = order(&[square(10.0, 10.0, 10.0)], (30.0, 30.0));
        assert_eq!(ordered[0].first(), Some(&(20.0, 20.0)));
        assert_eq!(ordered[0].first(), ordered[0].last());
        assert_eq!(ordered[0].len(), 5);
    }

    #[test]
    fn ordering_keeps_every_line_and_shortens_travel() {
        let lines: Vec<Polyline> = (0..30).map(|i| square(f64::from((i * 7) % 30) * 10.0, f64::from((i * 11) % 5) * 10.0, 5.0)).collect();
        let ordered = order(&lines, (0.0, 0.0));
        assert_eq!(ordered.len(), lines.len());
        assert!(travel_mm(&ordered, (0.0, 0.0)) < travel_mm(&lines, (0.0, 0.0)));
    }

    #[test]
    fn identical_outlines_do_not_block_each_other() {
        let a = square(0.0, 0.0, 10.0);
        assert_eq!(order(&[a.clone(), a.clone()], (0.0, 0.0)).len(), 2);
    }
}
