//! Source-configured PathList reservations for later road-family cost maps.
//! Reuses the shared PathList sweep; it never changes terrain or published paths.
use crate::{path_geometry::Path, rail_reservations, road_network::Road};
use serde::{Deserialize, Serialize};
pub const VERSION: &str = "papyrus-road-placement-sweep-1";
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub engine: String,
    pub width: f32,
    pub main_road_width: f32,
    pub sample_spacing: f32,
    pub terrain_origin_y: f32,
    pub terrain_height: f32,
    pub spline: bool,
    pub maximum_work: usize,
}
impl Policy {
    fn shared(&self, width: f32) -> rail_reservations::Policy {
        rail_reservations::Policy {
            engine: rail_reservations::VERSION.into(),
            width,
            sample_spacing: self.sample_spacing,
            terrain_origin_y: self.terrain_origin_y,
            terrain_height: self.terrain_height,
            spline: self.spline,
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.engine != VERSION || !(1..=128_000_000).contains(&self.maximum_work) {
            return Err("Unsupported road placement sweep policy".into());
        }
        self.shared(self.main_road_width).validate()?;
        self.shared(self.width).validate()
    }
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub roads: usize,
    pub main_roads: usize,
    pub control_work: usize,
    pub sweep: rail_reservations::Report,
    pub work: usize,
}
pub struct Output {
    pub raster: Vec<u8>,
    pub report: Report,
}
pub fn generate(
    size: f32,
    resolution: usize,
    initial: &[u8],
    roads: (&[Road], usize),
    policy: &Policy,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Output>, String> {
    policy.validate()?;
    let (roads, main_roads) = roads;
    if cancelled() {
        return Ok(None);
    }
    if main_roads > roads.len()
        || roads.len() > 257
        || roads.iter().map(|r| r.points.len()).sum::<usize>() > 65_536
    {
        return Err("Road placement control allowance reached".into());
    }
    let mut paths = Vec::with_capacity(roads.len());
    let mut work = 0;
    for road in roads {
        let remaining = policy.maximum_work.saturating_sub(work).min(32_000_000);
        if remaining == 0 {
            return Err("Road placement work allowance reached".into());
        }
        let Some(path) = Path::from_controls(&road.points, remaining, &cancelled)
            .map_err(|e| format!("Road reservation controls failed: {e:?}"))?
        else {
            return Ok(None);
        };
        work += path.work();
        paths.push(path);
    }
    let control_work = work;
    let mut raster: Option<Vec<u8>> = None;
    let mut summary: Option<rail_reservations::Report> = None;
    let groups = if main_roads > 0 && main_roads < paths.len() {
        vec![
            (0, main_roads, policy.main_road_width),
            (main_roads, paths.len(), policy.width),
        ]
    } else {
        vec![(
            0,
            paths.len(),
            if main_roads > 0 {
                policy.main_road_width
            } else {
                policy.width
            },
        )]
    };
    for (start, end, width) in groups {
        let remaining = policy.maximum_work.saturating_sub(work);
        if remaining == 0 {
            return Err("Road placement work allowance reached".into());
        }
        let tracks = paths[start..end]
            .iter()
            .enumerate()
            .map(|(i, path)| rail_reservations::Track {
                id: (start + i) as u64 + 1,
                path,
            })
            .collect::<Vec<_>>();
        let Some(output) = rail_reservations::generate(
            size,
            resolution,
            raster.as_deref().unwrap_or(initial),
            &tracks,
            &policy.shared(width),
            remaining,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        work += output.report.work;
        if let Some(report) = &mut summary {
            report.tracks += output.report.tracks;
            report.sweeps += output.report.sweeps;
            report.newly_blocked += output.report.newly_blocked;
            report.final_blocked = output.report.final_blocked;
            report.cell_visits += output.report.cell_visits;
            report.work += output.report.work;
        } else {
            summary = Some(output.report);
        }
        raster = Some(output.raster);
    }
    Ok(Some(Output {
        raster: raster.unwrap(),
        report: Report {
            engine: VERSION,
            roads: roads.len(),
            main_roads,
            control_work,
            sweep: summary.unwrap(),
            work,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::road_network::Destination;
    fn policy() -> Policy {
        Policy {
            engine: VERSION.into(),
            width: 20.,
            main_road_width: 24.,
            sample_spacing: 1.,
            terrain_origin_y: -500.,
            terrain_height: 1000.,
            spline: true,
            maximum_work: 1_000_000,
        }
    }
    fn road(points: Vec<[f32; 3]>) -> Road {
        Road {
            from: Destination::Monument {
                monument: 1,
                connector: 1,
            },
            to: Destination::Monument {
                monument: 2,
                connector: 2,
            },
            points,
        }
    }
    #[test]
    fn straight_source_sweep_matches_independent_cell_rectangle_and_preserves_initial_cells() {
        let n = 256;
        let mut initial = vec![0; n * n];
        initial[0] = 1;
        let roads = [road(vec![[-100., 20., 0.], [100., 20., 0.]])];
        let before = roads[0].points.clone();
        let output = generate(1000., n, &initial, (&roads, 0), &policy(), || false)
            .unwrap()
            .unwrap();
        let mut expected = initial.clone();
        let index = |x: f32| ((x / 1000. + 0.5) * n as f32) as usize;
        for z in 0..n {
            let wz = ((z as f32 + 0.5) / n as f32 - 0.5) * 1000.;
            if wz.abs() <= 10. {
                for x in index(-100.)..=index(100.) {
                    expected[z * n + x] = 1;
                }
            }
        }
        assert_eq!(output.raster, expected);
        assert_eq!(roads[0].points, before);
        assert_eq!(initial[0], 1);
        assert_eq!(output.report.roads, 1);
        assert!(output.report.sweep.newly_blocked > 0);
        assert!(output.report.work <= policy().maximum_work);
        let again = generate(1000., n, &output.raster, (&roads, 0), &policy(), || false)
            .unwrap()
            .unwrap();
        assert_eq!(again.raster, output.raster);
        assert_eq!(again.report.sweep.newly_blocked, 0);
    }
    #[test]
    fn rotated_and_closed_paths_keep_controls_and_union_without_mutating_input() {
        let initial = vec![0; 256 * 256];
        let roads = [
            road(vec![[-100., 30., -100.], [0., 35., 0.], [100., 40., 100.]]),
            road(vec![
                [-200., 20., -200.],
                [200., 20., -200.],
                [200., 20., 200.],
                [-200., 20., 200.],
                [-200., 20., -200.],
            ]),
        ];
        let before = roads.iter().map(|r| r.points.clone()).collect::<Vec<_>>();
        let output = generate(1000., 256, &initial, (&roads, 0), &policy(), || false)
            .unwrap()
            .unwrap();
        assert!(output.report.sweep.newly_blocked > 1000);
        assert_eq!(
            roads.iter().map(|r| r.points.clone()).collect::<Vec<_>>(),
            before
        );
        assert!(initial.iter().all(|&v| v == 0));
        assert_eq!(
            output.raster,
            generate(1000., 256, &initial, (&roads, 0), &policy(), || false)
                .unwrap()
                .unwrap()
                .raster
        );
    }
    #[test]
    fn main_roads_use_their_wider_source_corridor_and_groups_share_one_budget() {
        let initial = vec![0; 512 * 512];
        let roads = [road(vec![[-100., 20., 0.], [100., 20., 0.]])];
        let narrow = generate(1000., 512, &initial, (&roads, 0), &policy(), || false)
            .unwrap()
            .unwrap();
        let main = generate(1000., 512, &initial, (&roads, 1), &policy(), || false)
            .unwrap()
            .unwrap();
        assert!(main.report.sweep.newly_blocked > narrow.report.sweep.newly_blocked);
        assert!(
            main.raster
                .iter()
                .zip(&narrow.raster)
                .all(|(&a, &b)| a >= b)
        );
        let mixed = [
            road(roads[0].points.clone()),
            road(vec![[-100., 20., 80.], [100., 20., 80.]]),
        ];
        let output = generate(1000., 512, &initial, (&mixed, 1), &policy(), || false)
            .unwrap()
            .unwrap();
        assert_eq!(output.report.main_roads, 1);
        assert_eq!(output.report.sweep.tracks, 2);
        assert!(output.report.work <= policy().maximum_work);
        assert_eq!(
            output.report.sweep.final_blocked,
            output.raster.iter().filter(|&&v| v != 0).count()
        );
        let mut short = policy();
        short.maximum_work = 500_000;
        assert!(generate(1000., 512, &initial, (&mixed, 1), &short, || false).is_err());
        assert!(initial.iter().all(|&v| v == 0));
    }
    #[test]
    fn cancellation_bad_cells_and_shared_budget_never_publish_partial_reservations() {
        let initial = vec![0; 256 * 256];
        let roads = [road(vec![[-100., 20., 0.], [100., 20., 0.]])];
        let calls = std::cell::Cell::new(0);
        assert!(
            generate(1000., 256, &initial, (&roads, 0), &policy(), || {
                calls.set(calls.get() + 1);
                calls.get() > 20
            })
            .unwrap()
            .is_none()
        );
        let mut small = policy();
        small.maximum_work = 100;
        assert!(generate(1000., 256, &initial, (&roads, 0), &small, || false).is_err());
        let mut invalid = initial.clone();
        invalid[123] = 2;
        assert!(generate(1000., 256, &invalid, (&roads, 0), &policy(), || false).is_err());
        assert!(initial.iter().all(|&v| v == 0));
        let empty = generate(1000., 256, &initial, (&[], 0), &policy(), || false)
            .unwrap()
            .unwrap();
        assert_eq!(empty.raster, initial);
        assert_eq!(empty.report.sweep.newly_blocked, 0);
    }
}
