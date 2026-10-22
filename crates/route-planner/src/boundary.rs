//! Inclusive polygon containment and whole-segment checks (not spaced sampling).
use crate::{point_in_polygon, Coord};

const EPS: f64 = 1e-10;
/// Degrees; far above `EPS`-scaled slack for any real coordinate span.
const BBOX_MARGIN: f64 = 1e-6;
fn cross(a: (f64, f64), b: (f64, f64)) -> f64 {
    a.0 * b.1 - a.1 * b.0
}
fn delta(a: Coord, b: Coord) -> (f64, f64) {
    (b.lon - a.lon, b.lat - a.lat)
}
fn at(a: Coord, b: Coord, t: f64) -> Coord {
    Coord::new(a.lon + (b.lon - a.lon) * t, a.lat + (b.lat - a.lat) * t)
}
fn on_edge(a: Coord, b: Coord, p: Coord) -> bool {
    let d = delta(a, b);
    let q = delta(a, p);
    let length = d.0.hypot(d.1);
    if length < EPS {
        return q.0.hypot(q.1) < EPS;
    }
    cross(d, q).abs() <= EPS * length
        && p.lon >= a.lon.min(b.lon) - EPS
        && p.lon <= a.lon.max(b.lon) + EPS
        && p.lat >= a.lat.min(b.lat) - EPS
        && p.lat <= a.lat.max(b.lat) + EPS
}
pub fn contains(polys: &[Vec<Coord>], p: Coord) -> bool {
    p.lon.is_finite()
        && p.lat.is_finite()
        && polys.iter().any(|poly| {
            poly.len() >= 3
                && (point_in_polygon(poly, p)
                    || poly
                        .iter()
                        .zip(poly.iter().cycle().skip(1))
                        .any(|(&a, &b)| on_edge(a, b, p)))
        })
}
/// Split at every boundary intersection, then test every open interval.
/// This catches arbitrarily narrow notches and gaps between separate campuses.
pub fn segment_inside(polys: &[Vec<Coord>], a: Coord, b: Coord) -> bool {
    if !contains(polys, a) || !contains(polys, b) {
        return false;
    }
    let r = delta(a, b);
    let len2 = r.0 * r.0 + r.1 * r.1;
    if len2 < EPS * EPS {
        return true;
    }
    let mut cuts = vec![0.0, 1.0];
    // Fence edges whose bounding box misses the segment's (plus a margin wider
    // than the parametric tolerances below) cannot contribute a cut.
    let (min_x, max_x) = (
        a.lon.min(b.lon) - BBOX_MARGIN,
        a.lon.max(b.lon) + BBOX_MARGIN,
    );
    let (min_y, max_y) = (
        a.lat.min(b.lat) - BBOX_MARGIN,
        a.lat.max(b.lat) + BBOX_MARGIN,
    );
    for poly in polys {
        for (&c, &d) in poly.iter().zip(poly.iter().cycle().skip(1)) {
            if c.lon.max(d.lon) < min_x
                || c.lon.min(d.lon) > max_x
                || c.lat.max(d.lat) < min_y
                || c.lat.min(d.lat) > max_y
            {
                continue;
            }
            let s = delta(c, d);
            let q = delta(a, c);
            let den = cross(r, s);
            if den.abs() > 1e-20 {
                let t = cross(q, s) / den;
                let u = cross(q, r) / den;
                if (0.0..=1.0).contains(&t) && (-EPS..=1.0 + EPS).contains(&u) {
                    cuts.push(t);
                }
            } else {
                for p in [c, d] {
                    if on_edge(a, b, p) {
                        let v = delta(a, p);
                        cuts.push(((v.0 * r.0 + v.1 * r.1) / len2).clamp(0.0, 1.0));
                    }
                }
            }
        }
    }
    cuts.sort_by(f64::total_cmp);
    cuts.windows(2)
        .all(|w| contains(polys, at(a, b, (w[0] + w[1]) * 0.5)))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn c(x: f64, y: f64) -> Coord {
        Coord::new(x, y)
    }
    #[test]
    fn catches_narrow_notch_and_includes_boundary() {
        let p = vec![vec![
            c(0., 0.),
            c(1., 0.),
            c(1., 1.),
            c(0.56, 1.),
            c(0.56, 0.2),
            c(0.55, 0.2),
            c(0.55, 1.),
            c(0., 1.),
        ]];
        assert!(!segment_inside(&p, c(0.1, 0.5), c(0.9, 0.5)));
        assert!(segment_inside(&p, c(0., 0.), c(1., 0.)));
        assert!(segment_inside(&p, c(0.1, 0.1), c(0.9, 0.1)));
    }
    #[test]
    fn far_fence_edges_are_skipped_without_changing_results() {
        // A many-sided ring: only the few edges near each probe can matter.
        let ring: Vec<Coord> = (0..720)
            .map(|i| {
                let t = i as f64 / 720.0 * std::f64::consts::TAU;
                c(t.cos(), t.sin())
            })
            .collect();
        let mut p = vec![ring];
        assert!(segment_inside(&p, c(-0.5, 0.0), c(0.5, 0.0)));
        assert!(segment_inside(&p, c(0.6, -0.75), c(0.6, 0.75)));
        // Chord between two adjacent vertices runs along the boundary edge itself.
        assert!(segment_inside(&p, p[0][10], p[0][11]));
        // A one-degree wedge notch reaching the centre: crossing it leaves the fence.
        p[0][0] = c(0.0, 0.0);
        assert!(!segment_inside(&p, c(0.5, -0.1), c(0.5, 0.1)));
        assert!(segment_inside(&p, c(-0.5, -0.1), c(-0.5, 0.1)));
    }
    #[test]
    fn disjoint_campuses_cannot_be_joined() {
        let p = vec![
            vec![c(0., 0.), c(1., 0.), c(1., 1.), c(0., 1.)],
            vec![c(2., 0.), c(3., 0.), c(3., 1.), c(2., 1.)],
        ];
        assert!(!segment_inside(&p, c(0.5, 0.5), c(2.5, 0.5)));
    }
}
