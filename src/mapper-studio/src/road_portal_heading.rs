//! Cheap terrain-contour guidance for a fixed portal. Samples rank proposals;
//! they never certify a template mesh or relax the full composition gates.
use papyrus_terrain::path_terrain;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub(crate) struct Audit {
    pub baseline_sampled_excess: Option<f64>,
    pub selected_sampled_excess: f64,
    pub turn_degrees: Option<i32>,
    pub candidates_scored: usize,
    pub height_queries: usize,
}
pub(crate) struct Choice {
    pub delta: [f64; 2],
    pub audit: Audit,
}
struct Sampler<'a, F> {
    grid: path_terrain::Grid<'a>,
    work: &'a mut usize,
    maximum: usize,
    queries: usize,
    cancelled: &'a F,
}
impl<F: Fn() -> bool> Sampler<'_, F> {
    fn height(&mut self, p: [f64; 2]) -> Result<Option<f64>, String> {
        if (self.cancelled)() {
            return Ok(None);
        }
        let p = p.map(|v| v as f32);
        if p.iter()
            .any(|v| !v.is_finite() || v.abs() > self.grid.size * 0.5)
        {
            return Ok(None);
        }
        *self.work = self
            .work
            .checked_add(8)
            .filter(|v| *v <= self.maximum)
            .ok_or("Portal heading work exhausted")?;
        self.queries += 1;
        path_terrain::sample_height(self.grid, [p[0], 0., p[1]]).map(|v| Some(f64::from(v)))
    }
    fn score(
        &mut self,
        portal: [f64; 2],
        neighbor: [f64; 2],
        radius: f64,
    ) -> Result<Option<f64>, String> {
        let d = [neighbor[0] - portal[0], neighbor[1] - portal[1]];
        let length = d[0].hypot(d[1]);
        if length <= 1e-6 {
            return Ok(None);
        }
        let side = [d[1] / length, -d[0] / length];
        let Some(middle) = self.height(portal)? else {
            return Ok(None);
        };
        let mut excess = 0f64;
        for sign in [-1., 1.] {
            let end = std::array::from_fn::<_, 2, _>(|k| portal[k] + side[k] * radius * sign);
            let Some(height) = self.height(end)? else {
                return Ok(None);
            };
            for step in 1..16 {
                let t = f64::from(step) / 16.;
                let p = std::array::from_fn(|k| portal[k] + (end[k] - portal[k]) * t);
                let Some(ground) = self.height(p)? else {
                    return Ok(None);
                };
                excess = excess.max(ground - (middle + (height - middle) * t));
            }
        }
        Ok(Some(excess))
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn choose(
    portal: [f32; 3],
    neighbor: [f32; 3],
    baseline_delta: [f64; 2],
    width: f64,
    signed_turn: f64,
    maximum_displacement: f64,
    grid: path_terrain::Grid<'_>,
    work: &mut usize,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Choice>, String> {
    if cancelled() {
        return Ok(None);
    }
    if !width.is_finite()
        || !(0.1..=128.).contains(&width)
        || !matches!(signed_turn, -1. | 1.)
        || !maximum_displacement.is_finite()
        || maximum_displacement <= 0.
        || portal.iter().chain(&neighbor).any(|v| !v.is_finite())
        || baseline_delta.iter().any(|v| !v.is_finite())
    {
        return Err("Invalid portal heading inputs".into());
    }
    let portal = [f64::from(portal[0]), f64::from(portal[2])];
    let neighbor = [f64::from(neighbor[0]), f64::from(neighbor[2])];
    let d = [neighbor[0] - portal[0], neighbor[1] - portal[1]];
    let length = d[0].hypot(d[1]);
    if length <= 1e-6 {
        return Err("Degenerate portal approach".into());
    }
    let mut sampler = Sampler {
        grid,
        work,
        maximum: maximum_work,
        queries: 0,
        cancelled: &cancelled,
    };
    let baseline = sampler.score(
        portal,
        std::array::from_fn(|k| neighbor[k] + baseline_delta[k]),
        width * 0.5,
    )?;
    let mut best = baseline.map(|score| (score, baseline_delta, None));
    let mut scored = usize::from(baseline.is_some());
    let angle = d[1].atan2(d[0]);
    let precision = 8. * f64::from(f32::EPSILON) * f64::from(grid.vertical_span).max(1.);
    for degrees in (15..=120).step_by(15) {
        if cancelled() {
            return Ok(None);
        }
        let bearing = angle + signed_turn * f64::from(degrees).to_radians();
        let target = [
            portal[0] + length * bearing.cos(),
            portal[1] + length * bearing.sin(),
        ];
        let delta = [target[0] - neighbor[0], target[1] - neighbor[1]];
        // Each branch keeps its original absolute lateral-movement grant.
        if delta[0].hypot(delta[1]) > maximum_displacement {
            continue;
        }
        let Some(score) = sampler.score(portal, target, width * 0.5)? else {
            continue;
        };
        scored += 1;
        if best.as_ref().is_none_or(|b| score + precision < b.0) {
            best = Some((score, delta, Some(degrees * signed_turn as i32)));
        }
    }
    if cancelled() {
        return Ok(None);
    }
    let (score, delta, turn) = best.ok_or("Portal cross-sections leave terrain bounds")?;
    Ok(Some(Choice {
        delta,
        audit: Audit {
            baseline_sampled_excess: baseline,
            selected_sampled_excess: score,
            turn_degrees: turn,
            candidates_scored: scored,
            height_queries: sampler.queries,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn grid(height: &[i16]) -> path_terrain::Grid<'_> {
        path_terrain::Grid {
            size: 200.,
            resolution: 201,
            origin_y: -500.,
            vertical_span: 1000.,
            height,
        }
    }
    #[test]
    fn contour_guidance_reduces_edge_overlap_without_a_larger_movement_grant() {
        let height = (0usize..201 * 201)
            .map(|i| {
                let x = (i % 201) as i32 - 100;
                (16383 - (x * x).min(8000)) as i16
            })
            .collect::<Vec<_>>();
        let mut work = 0;
        let chosen = choose(
            [0.; 3],
            [-7.5, 0., 0.],
            [0., -10.],
            10.,
            1.,
            10.,
            grid(&height),
            &mut work,
            100000,
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(
            chosen.audit.selected_sampled_excess + 0.001
                < chosen.audit.baseline_sampled_excess.unwrap()
        );
        assert!(matches!(chosen.audit.turn_degrees, Some(1..=15)));
        assert!(chosen.delta[0].hypot(chosen.delta[1]) <= 10.);
        assert_eq!(work, chosen.audit.height_queries * 8);
        assert!(work < 5000);
        let mut replay_work = 0;
        let replay = choose(
            [0.; 3],
            [-7.5, 0., 0.],
            [0., -10.],
            10.,
            1.,
            10.,
            grid(&height),
            &mut replay_work,
            work,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(chosen.delta, replay.delta);
        assert_eq!(work, replay_work);
        assert!(
            choose(
                [0.; 3],
                [-7.5, 0., 0.],
                [0., -10.],
                10.,
                1.,
                10.,
                grid(&height),
                &mut 0,
                work - 1,
                || false
            )
            .is_err()
        );
    }
    #[test]
    fn flat_terrain_keeps_baseline_and_cancellation_returns_no_choice() {
        let height = vec![16383; 201 * 201];
        let c = choose(
            [0.; 3],
            [-7.5, 0., 0.],
            [0., -5.],
            10.,
            1.,
            5.,
            grid(&height),
            &mut 0,
            100000,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(c.delta, [0., -5.]);
        assert_eq!(c.audit.turn_degrees, None);
        let calls = std::cell::Cell::new(0);
        let mut work = 0;
        assert!(
            choose(
                [0.; 3],
                [-7.5, 0., 0.],
                [0., -5.],
                10.,
                1.,
                5.,
                grid(&height),
                &mut work,
                100000,
                || {
                    calls.set(calls.get() + 1);
                    calls.get() > 10
                }
            )
            .unwrap()
            .is_none()
        );
        assert!(work < c.audit.height_queries * 8);
    }
}
