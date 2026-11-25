//! Atomic source-ordered height raises and topology modifiers for generated prefabs.
use crate::{
    dungeon_base::Pose,
    environment::Transform,
    native_layers::{Layers, dimensions},
    path_terrain::{SHORT_TO_FLOAT, smooth},
    source_stamp_raster::Circle,
    tunnel_network::{Failure, Meter},
};

#[derive(Clone, Debug)]
pub enum Effect {
    RaiseHeight {
        position: [f32; 3],
        opacity: f32,
        radius: f32,
        fade: f32,
    },
    SetTopology {
        position: [f32; 3],
        mask: u32,
        radius: f32,
        fade: f32,
    },
    AddTopology {
        position: [f32; 3],
        mask: u32,
        radius: f32,
        fade: f32,
    },
}
#[derive(Clone, Debug, Default)]
pub struct Prepared(Vec<Effect>);
impl Prepared {
    pub fn new(effects: Vec<Effect>) -> Result<Self, String> {
        if effects.len() > 4096 {
            return Err("Too many prefab terrain effects".into());
        }
        for effect in &effects {
            let (position, radius, fade) = match effect {
                Effect::RaiseHeight {
                    position,
                    opacity,
                    radius,
                    fade,
                } => {
                    if !opacity.is_finite() || !(0. ..=1.).contains(opacity) {
                        return Err("Invalid height-raise opacity".into());
                    }
                    (position, radius, fade)
                }
                Effect::SetTopology {
                    position,
                    radius,
                    fade,
                    ..
                }
                | Effect::AddTopology {
                    position,
                    radius,
                    fade,
                    ..
                } => (position, radius, fade),
            };
            if position.iter().any(|v| !v.is_finite() || v.abs() > 10000.)
                || !radius.is_finite()
                || !(0. ..=4096.).contains(radius)
                || !fade.is_finite()
                || !(0. ..=4096.).contains(fade)
            {
                return Err("Invalid prefab terrain effect".into());
            }
        }
        Ok(Self(effects))
    }
    pub fn effects(&self) -> &[Effect] {
        &self.0
    }
}
pub struct Instance<'a> {
    pub prefab_id: u32,
    pub pose: Pose,
    pub effects: &'a Prepared,
}
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Report {
    pub instances: usize,
    pub operations: usize,
    pub height_writes: usize,
    pub topology_writes: usize,
    pub copied_cells: usize,
    pub work: usize,
}
struct Patch {
    height: Option<Vec<i16>>,
    topology: Option<Vec<i32>>,
    report: Report,
}
/// Exclusive terrain edit held until the owning stage accepts its other outputs.
/// Dropping an edit discards its private channel copies. The borrowed target
/// cannot change or be replaced between preparation and commit.
pub struct Edit<'a> {
    base: &'a mut Layers,
    patch: Patch,
}

/// Original channels moved out by a sequence of exclusive prefab commits.
/// The owner may restore them on the same candidate before any other terrain
/// stage publishes. Channels with no writes need no snapshot.
#[derive(Default)]
pub struct Undo {
    height: Option<Vec<i16>>,
    topology: Option<Vec<i32>>,
}

impl Undo {
    pub fn restore(self, base: &mut Layers) {
        if let Some(height) = self.height {
            base.height = height;
        }
        if let Some(topology) = self.topology {
            base.topology = topology;
        }
    }
}

impl Edit<'_> {
    /// The owner checks cancellation immediately before its publication point.
    /// Committing then performs only infallible ownership transfers.
    pub fn commit(self) -> Report {
        self.commit_recording(&mut Undo::default())
    }

    /// Keep the first value of each changed channel across successive commits.
    pub fn commit_recording(self, undo: &mut Undo) -> Report {
        if let Some(height) = self.patch.height {
            let previous = std::mem::replace(&mut self.base.height, height);
            undo.height.get_or_insert(previous);
        }
        if let Some(topology) = self.patch.topology {
            let previous = std::mem::replace(&mut self.base.topology, topology);
            undo.topology.get_or_insert(previous);
        }
        self.patch.report
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid(String),
    Work(Failure),
}
impl From<Failure> for Error {
    fn from(value: Failure) -> Self {
        Self::Work(value)
    }
}

/// Instances and their effects follow World.AddPrefab order. The immutable base
/// remains available throughout the transaction. Channels copy on their first
/// actual write, and the returned buffers publish together after success.
pub fn apply(
    base: &mut Layers,
    instances: &[Instance<'_>],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Report, Error> {
    Ok(prepare(base, instances, maximum_work, cancelled)?.commit())
}
pub fn prepare<'a>(
    base: &'a mut Layers,
    instances: &[Instance<'_>],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Edit<'a>, Error> {
    let patch = prepare_patch(base, instances, maximum_work, cancelled)?;
    Ok(Edit { base, patch })
}
fn prepare_patch(
    base: &Layers,
    instances: &[Instance<'_>],
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Patch, Error> {
    let (hr, n) = dimensions(base.size).map_err(Error::Invalid)?;
    if base.height_resolution != hr
        || base.surface_resolution != n
        || base.height.len() != hr * hr
        || base.topology.len() != n * n
        || instances.len() > 65536
        || !(1..=512_000_000).contains(&maximum_work)
    {
        return Err(Error::Invalid("Invalid prefab terrain transaction".into()));
    }
    let mut meter = Meter {
        used: 0,
        limit: maximum_work,
        cancelled: &cancelled,
    };
    let mut height: Option<Vec<i16>> = None;
    let mut topology: Option<Vec<i32>> = None;
    let mut report = Report::default();
    for instance in instances {
        meter.spend(1)?;
        if instance.prefab_id == 0 {
            return Err(Error::Invalid("Missing prefab terrain identity".into()));
        }
        Transform {
            position: instance.pose.position,
            rotation: instance.pose.rotation,
            scale: [1.; 3],
        }
        .validate()
        .map_err(Error::Invalid)?;
        report.instances += 1;
        for effect in instance.effects.effects() {
            meter.spend(1)?;
            report.operations += 1;
            let (position, radius, fade, resolution) = match effect {
                Effect::RaiseHeight {
                    position,
                    radius,
                    fade,
                    ..
                } => (position, radius, fade, hr),
                Effect::SetTopology {
                    position,
                    radius,
                    fade,
                    ..
                }
                | Effect::AddTopology {
                    position,
                    radius,
                    fade,
                    ..
                } => (position, radius, fade, n),
            };
            let point = instance.pose.point(*position);
            let circle = Circle::from_world(
                resolution,
                base.size as f32,
                [point[0], point[2]],
                *radius,
                *fade,
            )
            .map_err(Error::Invalid)?;
            let [x0, x1, z0, z1] = circle.bounds();
            for z in z0..z1 {
                for x in x0..x1 {
                    meter.spend(1)?;
                    let weight = circle.weight(x, z);
                    let index = z * resolution + x;
                    match effect {
                        Effect::RaiseHeight { opacity, .. } if weight > 0. && *opacity > 0. => {
                            let old = height.as_ref().unwrap_or(&base.height)[index];
                            if !(0..=32766).contains(&old) {
                                return Err(Error::Invalid("Invalid sampled prefab height".into()));
                            }
                            let source = old as f32 * SHORT_TO_FLOAT;
                            let target = (point[1] - -500.) * (1. / 1000.);
                            let raised = source.max(smooth(source, target, weight * opacity));
                            if !raised.is_finite() || !(0. ..=1.).contains(&raised) {
                                return Err(Error::Invalid(
                                    "Prefab height exceeds the native terrain encoding".into(),
                                ));
                            }
                            let next = (raised * 32766. + 0.5) as i16;
                            if next == old {
                                continue;
                            }
                            if height.is_none() {
                                meter.spend(base.height.len())?;
                                report.copied_cells += base.height.len();
                                height = Some(base.height.clone());
                            }
                            height.as_mut().unwrap()[index] = next;
                            report.height_writes += 1;
                        }
                        Effect::SetTopology { mask, .. } | Effect::AddTopology { mask, .. }
                            if weight > 0.5 =>
                        {
                            let old = topology.as_ref().unwrap_or(&base.topology)[index];
                            let next = match effect {
                                Effect::AddTopology { .. } => old | *mask as i32,
                                _ => *mask as i32,
                            };
                            if next == old {
                                continue;
                            }
                            if topology.is_none() {
                                meter.spend(base.topology.len())?;
                                report.copied_cells += base.topology.len();
                                topology = Some(base.topology.clone());
                            }
                            topology.as_mut().unwrap()[index] = next;
                            report.topology_writes += 1;
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    meter.spend(1)?;
    report.work = meter.used;
    Ok(Patch {
        height,
        topology,
        report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn base() -> Layers {
        let (hr, n) = dimensions(1000).unwrap();
        Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 1,
            height_resolution: hr,
            surface_resolution: n,
            height: vec![16383; hr * hr],
            biome: vec![0; n * n * 5],
            splat: vec![0; n * n * 8],
            topology: vec![i32::MIN; n * n],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        }
    }
    fn pose(y: f32) -> Pose {
        Pose {
            position: [0., y, 0.],
            rotation: [0., 0., 0., 1.],
        }
    }
    fn effects(y: f32, mask: u32) -> Prepared {
        Prepared::new(vec![
            Effect::RaiseHeight {
                position: [0., y, 0.],
                opacity: 0.5,
                radius: 20.,
                fade: 0.,
            },
            Effect::SetTopology {
                position: [0.; 3],
                mask,
                radius: 8.,
                fade: 0.,
            },
        ])
        .unwrap()
    }
    #[test]
    fn ordered_batches_match_separate_source_spawns_and_reuse_unmodified_channels() {
        let mut batch = base();
        let mut sequential = batch.clone();
        let biome = batch.biome.as_ptr();
        let splat = batch.splat.as_ptr();
        let first = effects(40., 8);
        let second = effects(80., 4);
        let instances = [
            Instance {
                prefab_id: 1,
                pose: pose(0.),
                effects: &first,
            },
            Instance {
                prefab_id: 2,
                pose: pose(0.),
                effects: &second,
            },
        ];
        let report = apply(&mut batch, &instances, 10000000, || false).unwrap();
        for instance in &instances {
            apply(
                &mut sequential,
                std::slice::from_ref(instance),
                10000000,
                || false,
            )
            .unwrap();
        }
        assert_eq!(batch.height, sequential.height);
        assert_eq!(batch.topology, sequential.topology);
        assert_eq!(biome, batch.biome.as_ptr());
        assert_eq!(splat, batch.splat.as_ptr());
        assert_eq!(
            report.copied_cells,
            batch.height.len() + batch.topology.len()
        );
        assert!(report.height_writes > 0 && report.topology_writes > 0);
        let center =
            batch.height_resolution / 2 * batch.height_resolution + batch.height_resolution / 2;
        assert!(crate::erosion::decode(batch.height[center]) > 49.9);
        let center =
            batch.surface_resolution / 2 * batch.surface_resolution + batch.surface_resolution / 2;
        assert_eq!(batch.topology[center], 4);
        assert_eq!(batch.topology[0], i32::MIN);
    }

    #[test]
    fn retry_restores_pre_tunnel_channels_across_multiple_publications() {
        let original = base();
        let mut candidate = original.clone();
        let height = candidate.height.as_ptr();
        let topology = candidate.topology.as_ptr();
        let splat = candidate.splat.as_ptr();
        let raise = Prepared::new(vec![Effect::RaiseHeight {
            position: [0., 35., 0.],
            opacity: 1.,
            radius: 16.,
            fade: 0.,
        }])
        .unwrap();
        let portals = effects(80., 4);
        let mut undo = Undo::default();
        for effects in [&raise, &portals] {
            let report = prepare(
                &mut candidate,
                &[Instance { prefab_id: 1, pose: pose(0.), effects }],
                10_000_000,
                || false,
            )
            .unwrap()
            .commit_recording(&mut undo);
            assert!(report.height_writes > 0);
        }
        assert_ne!(candidate.height, original.height);
        assert_ne!(candidate.topology, original.topology);
        undo.restore(&mut candidate);
        assert_eq!(candidate, original);
        assert_eq!(candidate.height.as_ptr(), height);
        assert_eq!(candidate.topology.as_ptr(), topology);
        assert_eq!(candidate.splat.as_ptr(), splat);
    }
    #[test]
    fn topology_add_preserves_existing_bits_and_follows_source_operation_order() {
        let mut base = base();
        let height = base.height.as_ptr();
        let effects = Prepared::new(vec![
            Effect::AddTopology {
                position: [0.; 3],
                mask: 512,
                radius: 20.,
                fade: 1.,
            },
            Effect::SetTopology {
                position: [0.; 3],
                mask: 4,
                radius: 8.,
                fade: 0.,
            },
            Effect::AddTopology {
                position: [0.; 3],
                mask: 512,
                radius: 8.,
                fade: 0.,
            },
        ])
        .unwrap();
        let instance = Instance {
            prefab_id: 1,
            pose: pose(0.),
            effects: &effects,
        };
        let report = apply(&mut base, &[instance], 10_000_000, || false).unwrap();
        let n = base.surface_resolution;
        assert_eq!(base.topology[n / 2 * n + n / 2], 516);
        assert!(base.topology.contains(&(i32::MIN | 512)));
        assert_eq!(base.topology[0], i32::MIN);
        assert_eq!(report.height_writes, 0);
        assert_eq!(height, base.height.as_ptr());
        assert_eq!(report.copied_cells, base.topology.len());
    }
    #[test]
    fn no_effective_writes_retain_allocations_and_skip_grid_copy_work() {
        let mut base = base();
        let height = base.height.as_ptr();
        let topology = base.topology.as_ptr();
        let effects = effects(-1000., i32::MIN as u32);
        let report = apply(
            &mut base,
            &[Instance {
                prefab_id: 1,
                pose: pose(0.),
                effects: &effects,
            }],
            10000,
            || false,
        )
        .unwrap();
        assert_eq!(report.copied_cells, 0);
        assert_eq!(report.height_writes, 0);
        assert_eq!(report.topology_writes, 0);
        assert_eq!(height, base.height.as_ptr());
        assert_eq!(topology, base.topology.as_ptr());
    }
    #[test]
    fn failed_later_instances_cancellation_and_final_budget_do_not_publish() {
        let original = base();
        let effects = effects(40., 8);
        let instance = Instance {
            prefab_id: 1,
            pose: pose(0.),
            effects: &effects,
        };
        let mut changed = original.clone();
        let report = apply(
            &mut changed,
            std::slice::from_ref(&instance),
            10000000,
            || false,
        )
        .unwrap();
        let mut retained = original.clone();
        assert!(matches!(
            apply(
                &mut retained,
                std::slice::from_ref(&instance),
                report.work - 1,
                || false
            ),
            Err(Error::Work(Failure::WorkLimit))
        ));
        assert_eq!(retained.height, original.height);
        assert_eq!(retained.topology, original.topology);
        let calls = std::cell::Cell::new(0);
        assert!(matches!(
            apply(
                &mut retained,
                std::slice::from_ref(&instance),
                10000000,
                || {
                    calls.set(calls.get() + 1);
                    calls.get() > 1000
                }
            ),
            Err(Error::Work(Failure::Cancelled))
        ));
        let invalid = Instance {
            prefab_id: 0,
            pose: pose(0.),
            effects: &effects,
        };
        assert!(matches!(
            apply(&mut retained, &[instance, invalid], 10000000, || false),
            Err(Error::Invalid(_))
        ));
        assert_eq!(retained.height, original.height);
        assert_eq!(retained.topology, original.topology);
    }
    #[test]
    fn brushes_clip_map_edges_and_unit_rotation_moves_the_authored_centers() {
        let mut base = base();
        let effects = Prepared::new(vec![Effect::SetTopology {
            position: [510., 0., 0.],
            mask: 0,
            radius: 20.,
            fade: 0.,
        }])
        .unwrap();
        let half = std::f32::consts::FRAC_PI_4;
        let instance = Instance {
            prefab_id: 1,
            pose: Pose {
                position: [0.; 3],
                rotation: [0., half.sin(), 0., half.cos()],
            },
            effects: &effects,
        };
        let report = apply(&mut base, &[instance], 1000000, || false).unwrap();
        assert!(report.topology_writes > 0);
        assert_eq!(report.height_writes, 0);
        let n = base.surface_resolution;
        assert_eq!(base.topology[n / 2], 0);
        assert_eq!(base.topology[n / 2 * n + n - 1], i32::MIN);
    }
    #[test]
    fn prepared_edits_drop_without_writes_and_commit_to_the_borrowed_target() {
        let mut base = base();
        let original = base.clone();
        let effects = effects(50., 4);
        let instances = [Instance {
            prefab_id: 1,
            pose: pose(0.),
            effects: &effects,
        }];
        drop(prepare(&mut base, &instances, 10000000, || false).unwrap());
        assert_eq!(base.height, original.height);
        assert_eq!(base.topology, original.topology);
        let biome = base.biome.as_ptr();
        let splat = base.splat.as_ptr();
        let report = prepare(&mut base, &instances, 10000000, || false)
            .unwrap()
            .commit();
        assert!(report.height_writes > 0 && report.topology_writes > 0);
        assert_ne!(base.height, original.height);
        assert_ne!(base.topology, original.topology);
        assert_eq!(base.biome.as_ptr(), biome);
        assert_eq!(base.splat.as_ptr(), splat);
    }
}
