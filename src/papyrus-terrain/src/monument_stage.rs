//! PlaceMonuments' stage-wide eligibility gate, before root decorators/anchors.
//! This is not the grouped candidate search, count/spacing solver or physics.
use crate::{native_layers::Layers, spawn};
use serde::{Deserialize, Serialize};
pub const VERSION: &str = "papyrus-monument-stage-filter-1";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Program {
    pub mode: i32,
    pub disabled: bool,
    pub minimum_world_size: u32,
    pub nexus_only: bool,
    pub filter: spawn::Filter,
}
impl Program {
    pub fn validate(&self) -> Result<(), String> {
        self.filter.validate()?;
        if !(-1..=3).contains(&self.mode) || self.minimum_world_size > 6000 {
            return Err("Unsupported land monument stage program".into());
        }
        Ok(())
    }
}
/// The captured realm mask belongs to the installation adapter, not a universal
/// default. PlaceMonuments inherits the base RunOnCache=false implementation.
#[derive(Clone, Copy, Debug)]
pub struct Context {
    pub realm_mask: i32,
    pub cached: bool,
    pub nexus: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum Check {
    Accepted { factor: f32, draw: f32 },
    FilterRejected { factor: f32, draw: f32 },
    Skipped(&'static str),
}

/// Caller supplies its deterministic random draw in the source Value range.
/// Retains the exact source comparison: equality passes, including factor=draw=0.
/// No Unity enabled/active flags participate in ProceduralComponent.ShouldRun.
pub fn evaluate(
    layers: &Layers,
    program: &Program,
    context: Context,
    point: [f32; 2],
    draw: f32,
) -> Result<Check, String> {
    program.validate()?;
    if !(1..=3).contains(&context.realm_mask)
        || !draw.is_finite()
        || !(0. ..=1.).contains(&draw)
        || point.iter().any(|v| !v.is_finite())
    {
        return Err("Invalid land monument stage query".into());
    }
    if program.disabled || context.cached || program.mode & context.realm_mask == 0 {
        return Ok(Check::Skipped("Procedural monument program does not run"));
    }
    if layers.size < program.minimum_world_size {
        return Ok(Check::Skipped("World is below the monument stage minimum"));
    }
    if program.nexus_only && !context.nexus {
        return Ok(Check::Skipped("Monument stage requires a nexus world"));
    }
    let u = point[0] / layers.size as f32 + 0.5;
    let v = point[1] / layers.size as f32 + 0.5;
    // Search proposals can extend beyond the world before the later footprint
    // check. Reject that candidate rather than aborting the complete search.
    if !(0. ..=1.).contains(&u) || !(0. ..=1.).contains(&v) {
        return Ok(Check::Skipped("Monument stage root lies outside the map"));
    }
    let factor = spawn::sample_factor(layers, &program.filter, u, v)?;
    Ok(if factor * factor < draw {
        Check::FilterRejected { factor, draw }
    } else {
        Check::Accepted { factor, draw }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finite_outside_search_proposals_are_rejected_without_aborting() {
        let (layers, program, context) = fixture();
        for point in [[-501., 0.], [0., 501.], [f32::MAX, 0.]] {
            assert_eq!(
                evaluate(&layers, &program, context, point, 0.5).unwrap(),
                Check::Skipped("Monument stage root lies outside the map")
            );
        }
        assert!(evaluate(&layers, &program, context, [f32::NAN, 0.], 0.5).is_err());
    }
    fn fixture() -> (Layers, Program, Context) {
        let (hr, n) = crate::native_layers::dimensions(1000).unwrap();
        let layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 1,
            height_resolution: hr,
            surface_resolution: n,
            height: vec![18000; hr * hr],
            splat: vec![0; n * n * 8],
            biome: vec![0; n * n * 5],
            topology: vec![16; n * n],
            normalized_splat_cells: 0,
            normalized_biome_cells: 0,
        };
        let program = Program {
            mode: -1,
            disabled: false,
            minimum_world_size: 0,
            nexus_only: false,
            filter: spawn::Filter {
                splat_type: -1,
                biome_type: 8,
                topology_any: -1,
                topology_all: 16,
                topology_not: 2,
            },
        };
        (
            layers,
            program,
            Context {
                realm_mask: 2,
                cached: false,
                nexus: false,
            },
        )
    }
    #[test]
    fn stage_filter_uses_squared_factor_and_exact_source_boundary() {
        let (mut layers, mut p, c) = fixture();
        let n = layers.surface_resolution;
        p.filter.splat_type = 1;
        layers.biome[3 * n * n..4 * n * n].fill(255);
        layers.splat[..n * n].fill(128);
        let factor = 128. / 255.;
        assert_eq!(
            evaluate(&layers, &p, c, [0.; 2], factor * factor).unwrap(),
            Check::Accepted {
                factor,
                draw: factor * factor
            }
        );
        assert!(matches!(
            evaluate(&layers, &p, c, [0.; 2], 0.3).unwrap(),
            Check::FilterRejected { .. }
        ));
        layers.splat.fill(0);
        assert_eq!(
            evaluate(&layers, &p, c, [0.; 2], 0.).unwrap(),
            Check::Accepted {
                factor: 0.,
                draw: 0.
            }
        );
        assert!(matches!(
            evaluate(&layers, &p, c, [0.; 2], f32::MIN_POSITIVE).unwrap(),
            Check::FilterRejected { .. }
        ));
    }
    #[test]
    fn biome_and_topology_are_sampled_at_the_proposal_and_inputs_stay_unchanged() {
        let (mut layers, p, c) = fixture();
        let n = layers.surface_resolution;
        for z in 0..n {
            for x in 0..n / 2 {
                layers.biome[3 * n * n + z * n + x] = 255;
            }
        }
        let original = layers.topology.clone();
        assert!(matches!(
            evaluate(&layers, &p, c, [-250., 0.], 0.5).unwrap(),
            Check::Accepted { .. }
        ));
        assert!(matches!(
            evaluate(&layers, &p, c, [250., 0.], 0.5).unwrap(),
            Check::FilterRejected { .. }
        ));
        assert_eq!(layers.topology, original);
        layers.topology.fill(2 | 16);
        assert!(matches!(
            evaluate(&layers, &p, c, [-250., 0.], 0.5).unwrap(),
            Check::FilterRejected { .. }
        ));
    }
    #[test]
    fn realm_disabled_cache_size_and_nexus_follow_their_separate_source_gates() {
        let (layers, mut p, c) = fixture();
        for mode in [0, 1] {
            p.mode = mode;
            assert!(matches!(
                evaluate(&layers, &p, c, [0.; 2], 0.5).unwrap(),
                Check::Skipped(_)
            ));
        }
        p.mode = 3;
        assert!(matches!(
            evaluate(&layers, &p, c, [0.; 2], 0.5).unwrap(),
            Check::FilterRejected { .. }
        ));
        p.disabled = true;
        assert!(matches!(
            evaluate(&layers, &p, c, [0.; 2], 0.5).unwrap(),
            Check::Skipped(_)
        ));
        p.disabled = false;
        p.minimum_world_size = 2000;
        assert!(matches!(
            evaluate(&layers, &p, c, [0.; 2], 0.5).unwrap(),
            Check::Skipped(_)
        ));
        p.minimum_world_size = 0;
        p.nexus_only = true;
        assert!(matches!(
            evaluate(&layers, &p, c, [0.; 2], 0.5).unwrap(),
            Check::Skipped(_)
        ));
        assert!(matches!(
            evaluate(&layers, &p, Context { nexus: true, ..c }, [0.; 2], 0.5).unwrap(),
            Check::FilterRejected { .. }
        ));
        assert!(matches!(
            evaluate(
                &layers,
                &p,
                Context {
                    nexus: true,
                    cached: true,
                    ..c
                },
                [0.; 2],
                0.5
            )
            .unwrap(),
            Check::Skipped(_)
        ));
    }
    #[test]
    fn malformed_program_queries_and_surface_grids_do_not_certify_acceptance() {
        let (mut layers, mut p, c) = fixture();
        for draw in [-0.1, 1.1, f32::NAN] {
            assert!(evaluate(&layers, &p, c, [0.; 2], draw).is_err());
        }
        assert!(evaluate(&layers, &p, c, [f32::NAN, 0.], 0.5).is_err());
        assert!(matches!(
            evaluate(&layers, &p, c, [501., 0.], 0.5).unwrap(),
            Check::Skipped(_)
        ));
        p.mode = 4;
        assert!(p.validate().is_err());
        p.mode = -1;
        layers.biome.pop();
        assert!(evaluate(&layers, &p, c, [0.; 2], 0.5).is_err());
    }
}
