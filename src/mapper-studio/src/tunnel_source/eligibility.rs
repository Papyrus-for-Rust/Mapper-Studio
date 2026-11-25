//! Reusable source-bound portal checks over the owning stage's private terrain/world.
use super::{Prefab, Source};
use papyrus_terrain::{
    dungeon_base::Pose,
    environment::World,
    native_layers::View,
    path_terrain::Grid,
    prefab_terrain,
    tunnel_depth::{self, Policy, Volume},
    tunnel_layout::{Placement, Role},
    tunnel_network::Failure,
};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PortalRejection {
    Environment,
    Terrain(prefab_terrain::Rejection),
    RailAccess,
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct PortalRailAccess {
    pub rail_type: i32,
    pub maximum_cut: f64,
    pub maximum_grade: f32,
    pub extension_spacing: f32,
    pub minimum_extension_steps: usize,
    pub half_width: f32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortalAssessment {
    pub rejection: Option<PortalRejection>,
    pub work: usize,
    pub terrain_samples: usize,
}
#[derive(Clone, Debug, PartialEq)]
pub enum PortalError {
    Invalid(String),
    Work(Failure),
    Environment(tunnel_depth::Error),
}

pub struct PortalChecks<'a> {
    prefabs: BTreeMap<u32, &'a Prefab>,
    geometry: BTreeMap<u32, Vec<Volume>>,
    terrain: prefab_terrain::Terrain<'a>,
    heights: Grid<'a>,
    world: &'a World,
    policy: Policy,
    view: View<'a>,
}
impl<'a> PortalChecks<'a> {
    /// Prepare once for a candidate transaction. The owner supplies the world
    /// registered at the portal stage and the current enabled placement raster.
    pub fn new(
        source: &'a Source,
        view: View<'a>,
        world: &'a World,
        policy: Policy,
    ) -> Result<Self, String> {
        let terrain = prefab_terrain::Terrain::new(view)?;
        let mut prefabs = BTreeMap::new();
        let mut geometry = BTreeMap::new();
        let roster = source
            .selectors
            .get("UpwardsFolder")
            .ok_or("Installed tunnel portal roster is missing")?;
        if roster.prefabs.len() > 4096 {
            return Err("Tunnel portal roster exceeds its bound".into());
        }
        for path in &roster.prefabs {
            let prefab = source
                .prefabs
                .get(path)
                .ok_or("Installed portal prefab is missing")?;
            if prefab.terrain.is_none() {
                return Err(
                    "Tunnel portal terrain attributes are missing; refresh Rust data".into(),
                );
            }
            if prefab.id == 0 || prefabs.insert(prefab.id, prefab).is_some() {
                return Err("Invalid or duplicate installed portal identity".into());
            }
            geometry.insert(prefab.id, prefab.environments.clone());
        }
        let origin_y = papyrus_terrain::erosion::decode(0);
        let heights = Grid {
            size: view.size as f32,
            resolution: view.height_resolution,
            origin_y,
            vertical_span: papyrus_terrain::erosion::decode(32766) - origin_y,
            height: view.height,
        };
        Ok(Self {
            prefabs,
            geometry,
            terrain,
            heights,
            world,
            policy,
            view,
        })
    }
    pub fn check(
        &self,
        prefab_id: u32,
        position: [f32; 3],
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<PortalAssessment, PortalError> {
        let prefab = self.prefabs.get(&prefab_id).ok_or_else(|| {
            PortalError::Invalid("Prefab is outside the installed portal roster".into())
        })?;
        let roots = [Placement {
            prefab_id,
            cell: [0, 0],
            position,
            role: Role::Tunnel,
        }];
        let environment = tunnel_depth::check(
            tunnel_depth::Request {
                placements: &roots,
                geometry: &self.geometry,
                terrain: Some(self.heights),
                world: self.world,
                policy: self.policy,
                maximum_work,
            },
            &cancelled,
        )
        .map_err(|error| match error {
            tunnel_depth::Error::Work(failure) => PortalError::Work(failure),
            tunnel_depth::Error::Invalid(error) => PortalError::Invalid(error),
            error => PortalError::Environment(error),
        })?;
        if !environment.clear {
            return Ok(PortalAssessment {
                rejection: Some(PortalRejection::Environment),
                work: environment.work,
                terrain_samples: environment.terrain_samples,
            });
        }
        let remaining = maximum_work
            .checked_sub(environment.work)
            .filter(|&work| work > 0)
            .ok_or(PortalError::Work(Failure::WorkLimit))?;
        let result = self
            .terrain
            .evaluate(
                prefab.terrain.as_ref().unwrap(),
                Pose {
                    position,
                    rotation: [0., 0., 0., 1.],
                },
                remaining,
                &cancelled,
            )
            .map_err(|error| match error {
                prefab_terrain::Error::Work(error) => PortalError::Work(error),
                prefab_terrain::Error::Invalid(error) => PortalError::Invalid(error),
            })?;
        Ok(PortalAssessment {
            rejection: result.rejection.map(PortalRejection::Terrain),
            work: environment.work + result.work,
            terrain_samples: environment.terrain_samples,
        })
    }

    pub fn check_rail_access(
        &self,
        prefab_id: u32,
        position: [f32; 3],
        policy: PortalRailAccess,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<PortalAssessment, PortalError> {
        let prefab = self.prefabs.get(&prefab_id).ok_or_else(|| {
            PortalError::Invalid("Prefab is outside the installed portal roster".into())
        })?;
        if !(0..=64).contains(&policy.rail_type)
            || !policy.maximum_cut.is_finite()
            || !(0. ..=64.).contains(&policy.maximum_cut)
            || !policy.maximum_grade.is_finite()
            || !(0.001..=1.).contains(&policy.maximum_grade)
            || !policy.extension_spacing.is_finite()
            || !(0.1..=32.).contains(&policy.extension_spacing)
            || !(1..=32).contains(&policy.minimum_extension_steps)
            || !policy.half_width.is_finite()
            || !(0. ..=64.).contains(&policy.half_width)
            || position.iter().any(|v| !v.is_finite() || v.abs() > 10000.)
        {
            return Err(PortalError::Invalid(
                "Invalid tunnel portal rail-access policy or position".into(),
            ));
        }
        let mut rejection = None;
        let mut work = 0;
        let mut terrain_samples = 0;
        // Portal effects raise terrain. Check whether the authored approach can
        // reach the surrounding ground within the requested grade and cutting
        // allowance before committing this underground replacement.
        for connector in &prefab.connectors {
            if cancelled() {
                return Err(PortalError::Work(Failure::Cancelled));
            }
            if work >= maximum_work {
                return Err(PortalError::Work(Failure::WorkLimit));
            }
            work += 1;
            if connector.infrastructure_type != policy.rail_type {
                continue;
            }
            let length = connector.forward[0].hypot(connector.forward[2]);
            if !length.is_finite() || length < 0.00001 {
                return Err(PortalError::Invalid("Invalid portal rail direction".into()));
            }
            let forward = [connector.forward[0] / length, connector.forward[2] / length];
            let distance = policy.extension_spacing * policy.minimum_extension_steps as f32;
            let spacing = self.view.size as f32 / (self.view.height_resolution - 1) as f32;
            let steps = (distance / spacing).ceil().max(1.) as usize;
            'approach: for step in 0..=steps {
                if cancelled() {
                    return Err(PortalError::Work(Failure::Cancelled));
                }
                let along = distance * step as f32 / steps as f32;
                let ceiling = f64::from(position[1] + connector.position[1])
                    + f64::from(policy.maximum_grade) * f64::from(along);
                for across in [-policy.half_width, 0., policy.half_width] {
                    if maximum_work.saturating_sub(work) < 4 {
                        return Err(PortalError::Work(Failure::WorkLimit));
                    }
                    work += 4;
                    let x = position[0] + connector.position[0] + forward[0] * along
                        - forward[1] * across;
                    let z = position[2]
                        + connector.position[2]
                        + forward[1] * along
                        + forward[0] * across;
                    let u = x / self.view.size as f32 + 0.5;
                    let v = z / self.view.size as f32 + 0.5;
                    if !(0. ..=1.).contains(&u) || !(0. ..=1.).contains(&v) {
                        rejection = Some(PortalRejection::RailAccess);
                        break 'approach;
                    }
                    let ground = papyrus_terrain::spawn::sample_height_view(self.view, u, v)
                        .map_err(PortalError::Invalid)?;
                    terrain_samples += 4;
                    if f64::from(ground) - ceiling > policy.maximum_cut {
                        rejection = Some(PortalRejection::RailAccess);
                        break 'approach;
                    }
                }
            }
            if rejection.is_some() {
                break;
            }
        }
        Ok(PortalAssessment {
            rejection,
            work,
            terrain_samples,
        })
    }
}
