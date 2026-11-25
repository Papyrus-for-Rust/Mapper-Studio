//! Utility layout over the live source-ordered object terrain.
use crate::{
    Project, infrastructure, powerline_source,
    prefab_assets::Catalog,
    road_object_placement::{Emitter, Road},
    road_object_source, source_road_cache, source_road_inputs,
};
use papyrus_terrain::{
    decor_fit::rotate, path_terrain, powerline_costmap, powerline_layout, rail_costmap::Terrain,
    source_road_route,
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
pub const VERSION: &str = "mapper-powerline-generation-1";

fn routing_work_allowance(cells: usize, connectors: usize) -> Result<usize, String> {
    // Each connection can search the lattice again. Keep the per-search bounds
    // while accounting for the complete network's area and socket roster.
    cells
        .checked_mul(
            connectors
                .checked_add(1)
                .ok_or("Utility work allowance overflow")?,
        )
        .and_then(|work| work.checked_mul(32))
        .map(|work| work.clamp(128_000_000, powerline_layout::MAXIMUM_WORK))
        .ok_or_else(|| "Utility work allowance overflow".into())
}

pub(crate) struct Context<'a> {
    pub project: &'a Project,
    pub solution: &'a papyrus_terrain::placement::Solution,
    pub root: &'a Path,
    pub catalog: &'a Catalog,
    pub infrastructure: &'a infrastructure::Source,
    pub road: &'a road_object_source::Source,
    pub report: &'a serde_json::Value,
}
pub struct Inputs {
    pub objects: road_object_source::Source,
    pub capture_sha256: String,
    pub layout_order: usize,
    pub topology_order: usize,
    pub layout_active: bool,
    pub power_type: i32,
    pub cost: powerline_costmap::Policy,
    pub primary: Vec<Vec<[usize; 2]>>,
    pub connectors: Vec<powerline_layout::Connector>,
    pub actor_ids: BTreeSet<u64>,
}
#[derive(Serialize)]
pub struct NamedPath {
    pub name: String,
    pub points: Vec<[f32; 3]>,
}
#[derive(Serialize)]
pub struct Generated {
    pub engine: &'static str,
    pub executed: bool,
    pub objects_source_sha256: String,
    pub capture_sha256: String,
    pub costmap: serde_json::Value,
    pub road_actors: BTreeMap<usize, u64>,
    pub layout: powerline_layout::Report,
    pub paths: Vec<NamedPath>,
}
impl Inputs {
    pub(crate) fn bind_plan(&self, plan: &mut crate::object_workflow::Plan) -> Result<(), String> {
        use crate::object_workflow::Handler;
        let workflow = self.objects.procedural_order()?;
        let mut bindings = vec![
            (
                self.layout_order,
                "GeneratePowerlineLayout",
                Handler::UtilityLayout,
            ),
            (
                self.topology_order,
                "GeneratePowerlineTopology",
                Handler::UtilityTopology,
            ),
        ];
        for (index, stage) in self.objects.stages.iter().enumerate() {
            bindings.push((
                stage.scene_order.ok_or("Missing utility scene order")?,
                "PlacePowerlineObjects",
                Handler::UtilityObjects {
                    source_index: index,
                },
            ));
        }
        for (order, class, handler) in bindings {
            let expected = workflow
                .iter()
                .find(|stage| stage.order == order)
                .ok_or("Utility stage is absent from its source workflow")?;
            let step = plan
                .steps
                .iter_mut()
                .find(|step| step.scene_order == order)
                .ok_or("Utility stage is absent from the object workflow")?;
            if step.class != class
                || expected.kind != class
                || step.object_id != expected.object_id
                || step.mode != expected.mode
                || step.description != expected.description
                || step.disabled != expected.disabled
                || step.component_enabled != expected.component_enabled
                || step.hierarchy_active != expected.hierarchy_active
                || step.handler != Handler::External
            {
                return Err("Utility stage differs from its source workflow".into());
            }
            step.handler = handler;
        }
        Ok(())
    }
    pub(crate) fn prepare(
        context: Context<'_>,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        let Context {
            project,
            solution,
            root,
            catalog,
            infrastructure,
            road,
            report,
        } = context;
        let Some(layout_policy) = project
            .roads
            .as_ref()
            .and_then(|recipe| recipe.staged.as_ref())
            .and_then(|stage| stage.layout.as_ref())
        else {
            return Ok(None);
        };
        let Some(roles) = source_road_inputs::Roles::for_source(infrastructure)? else {
            return Err("Selected utility connector roles are unavailable".into());
        };
        let power_type = roles
            .power()
            .ok_or("Missing selected utility connector role")?;
        let Some(assets) = source_road_cache::cached(
            source_road_cache::Context {
                root,
                catalog,
                infrastructure,
            },
            &layout_policy.assets_sha256,
            cancelled,
        )?
        else {
            return Ok(None);
        };
        let flags = infrastructure
            .profile
            .topology_flags
            .as_ref()
            .ok_or("Missing utility topology roles")?;
        let captured = powerline_source::available(
            root,
            catalog,
            road,
            flags,
            infrastructure.profile.protected_topology as u32,
            cancelled,
        )?;
        let objects =
            captured.bind_objects(road, &powerline_source::ResourceRules::default(), cancelled)?;
        let layout = captured
            .programs()
            .iter()
            .find(|stage| stage.kind == "GeneratePowerlineLayout")
            .ok_or("Missing utility layout stage")?;
        let topology = captured
            .programs()
            .iter()
            .find(|stage| stage.kind == "GeneratePowerlineTopology")
            .ok_or("Missing utility topology stage")?;
        if captured
            .programs()
            .iter()
            .filter(|stage| stage.kind == "GeneratePowerlineLayout")
            .count()
            != 1
            || captured
                .programs()
                .iter()
                .filter(|stage| stage.kind == "GeneratePowerlineTopology")
                .count()
                != 1
        {
            return Err("Unsupported repeated utility layout or topology stages".into());
        }
        let roadside: Vec<crate::roadside_stamps::Placement> = serde_json::from_value(
            report["surfaces"]["staged_pipeline"]["roadside"]["placements"].clone(),
        )
        .map_err(|error| error.to_string())?;
        let placements = crate::water_treatment_stage::placements(project, solution, &roadside)?;
        let mut actor_ids: BTreeSet<_> = project
            .targets
            .iter()
            .map(|target| target.instance_id)
            .collect();
        actor_ids.extend(roadside.iter().map(|placement| placement.request));
        let mut connectors = Vec::new();
        for placement in placements {
            if cancelled() {
                return Ok(None);
            }
            let prefab = assets.prefab(placement.prefab)?;
            if prefab.monument.is_none() {
                continue;
            }
            for connector in &prefab.connectors {
                if connector.infrastructure_type != power_type {
                    continue;
                }
                if connectors.len() >= 4096 {
                    return Err("Utility monument connector allowance exhausted".into());
                }
                let offset = rotate(
                    placement.pose.rotation,
                    std::array::from_fn(|index| {
                        connector.position[index] * placement.pose.scale[index]
                    }),
                );
                connectors.push(powerline_layout::Connector {
                    monument: placement.instance,
                    object_id: connector.object_id,
                    position: std::array::from_fn(|index| {
                        placement.pose.position[index] + offset[index]
                    }),
                });
            }
        }
        let mut primary = Vec::new();
        let raw = &report["source_layout"]["primary_routing"];
        if !raw.is_null() {
            let cells = raw["cells"]
                .as_array()
                .filter(|cells| cells.len() <= 1_000_000)
                .ok_or("Missing bounded primary utility anchor chain")?;
            let lattice =
                papyrus_terrain::road_costmap::CellLattice::new(project.size as f32, 7.5)?;
            if raw["lattice"]["resolution"].as_u64() != Some(lattice.resolution() as u64)
                || raw["lattice"]["size"].as_f64() != Some(project.size as f64)
            {
                return Err("Utility anchors use a different source lattice".into());
            }
            primary.push(
                serde_json::from_value(serde_json::Value::Array(cells.clone()))
                    .map_err(|error| error.to_string())?,
            );
        }
        let mask = |names: &[&str]| -> Result<u32, String> {
            names.iter().try_fold(0, |value, name| {
                flags
                    .get(*name)
                    .map(|flag| value | flag)
                    .ok_or_else(|| format!("Missing utility topology role {name}"))
            })
        };
        Ok(Some(Self {
            objects,
            capture_sha256: captured.sha256().into(),
            layout_order: layout.order,
            topology_order: topology.order,
            layout_active: !project.options.remove_large_powerlines.unwrap_or_default() && layout.hierarchy_active
                && layout.component_enabled
                && !layout.disabled.unwrap_or(false)
                && layout.mode as u32
                    & project
                        .objects
                        .as_ref()
                        .ok_or("Missing object realm")?
                        .realm_mask
                    != 0,
            power_type,
            primary,
            connectors,
            actor_ids,
            cost: powerline_costmap::Policy {
                spacing: 7.5,
                radius: 5.,
                blocked_topology: mask(&[
                    "Summit",
                    "Ocean",
                    "Oceanside",
                    "Monument",
                    "Lake",
                    "Lakeside",
                    "Building",
                    "Mountain",
                ])?,
                expensive_topology: mask(&[
                    "Road",
                    "Roadside",
                    "River",
                    "Riverside",
                    "Rail",
                    "Railside",
                ])?,
                moderate_topology: mask(&["Cliff", "Decor"])?,
                expensive_cost: 2500,
                moderate_cost: 1000,
                normal_base: 1,
                slope_scale: 10.,
            },
        }))
    }
    pub(crate) fn generate<F: Fn() -> bool>(
        &self,
        emitter: &Emitter<'_, F>,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Option<(Generated, Vec<Road>)>, String> {
        let view = emitter.view();
        let mut connectors = self.connectors.clone();
        let mut actors = self.actor_ids.clone();
        let mut road_actors = BTreeMap::new();
        let mut next = 1u64;
        for (admission, connector) in emitter.live_road_connectors(self.power_type)? {
            if connectors.len() >= 4096 {
                return Err("Combined utility connector allowance exhausted".into());
            }
            let actor = if let Some(&actor) = road_actors.get(&admission) {
                actor
            } else {
                while actors.contains(&next) {
                    next = next
                        .checked_add(1)
                        .ok_or("Utility actor identity overflow")?;
                }
                actors.insert(next);
                road_actors.insert(admission, next);
                next
            };
            connectors.push(powerline_layout::Connector {
                monument: actor,
                object_id: (connector.attribute + 1) as i64,
                position: connector.position,
            });
        }
        let blockers = view
            .placement_blockers
            .ok_or("Missing live utility placement map")?;
        let Some(grid) = powerline_costmap::build(
            Terrain {
                size: view.size as f32,
                height_resolution: view.height_resolution,
                height: view.height,
                topology_resolution: view.surface_resolution,
                topology: view.topology,
                placement_resolution: view.surface_resolution,
                placement: blockers,
            },
            &self.cost,
            128_000_000,
            cancelled,
        )?
        else {
            return Ok(None);
        };
        let chains: Vec<_> = self.primary.iter().map(Vec::as_slice).collect();
        let maximum_work = routing_work_allowance(grid.costs.len(), connectors.len())?;
        let Some(layout) = powerline_layout::generate(
            &grid,
            &connectors,
            &chains,
            powerline_layout::Settings {
                search: source_road_route::Settings {
                    maximum_expansions: 100_000,
                    maximum_work: maximum_work.min(powerline_layout::MAXIMUM_SEARCH_WORK),
                    directional_penalty: 0,
                    directional_radius: 1,
                },
                sample_stride: 8,
                minimum_path_cells: 8,
                maximum_path_cells: 262144,
                maximum_network_samples: 4096,
                maximum_routes: 4096,
                maximum_work,
            },
            cancelled,
        )?
        else {
            return Ok(None);
        };
        let mut paths = Vec::new();
        let mut roads = Vec::new();
        let mut total_points = 0usize;
        for link in &layout.links {
            if !link.retained {
                continue;
            }
            total_points = total_points
                .checked_add(link.route.cells.len())
                .filter(|count| *count <= 1_000_000)
                .ok_or("Utility path control allowance exhausted")?;
            let mut points = Vec::with_capacity(link.route.cells.len());
            for &[x, z] in &link.route.cells {
                if cancelled() {
                    return Ok(None);
                }
                let uv = [
                    (x as f32 + 0.5) / grid.lattice.resolution() as f32,
                    (z as f32 + 0.5) / grid.lattice.resolution() as f32,
                ];
                let y = path_terrain::sample_height_uv(
                    path_terrain::Grid {
                        size: view.size as f32,
                        resolution: view.height_resolution,
                        origin_y: -500.,
                        vertical_span: 1000.,
                        height: view.height,
                    },
                    uv,
                )?;
                points.push([
                    (uv[0] - 0.5) * view.size as f32,
                    y,
                    (uv[1] - 0.5) * view.size as f32,
                ]);
            }
            roads.push(Road {
                hierarchy: 0,
                width: 0.,
                start: true,
                end: true,
                points: points.clone(),
            });
            paths.push(NamedPath {
                name: format!("Powerline {}", paths.len()),
                points,
            });
        }
        let costmap = serde_json::json!({"engine":grid.engine,"lattice":grid.lattice,"counts":grid.counts,"work":grid.work,"policy":self.cost});
        Ok(Some((
            Generated {
                engine: VERSION,
                executed: true,
                objects_source_sha256: self.objects.sha256.clone(),
                capture_sha256: self.capture_sha256.clone(),
                costmap,
                road_actors,
                layout,
                paths,
            },
            roads,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complete_network_work_scales_with_area_and_connectors_with_a_fixed_ceiling() {
        assert_eq!(routing_work_allowance(17 * 17, 2).unwrap(), 128_000_000);
        let larger = routing_work_allowance(667 * 667, 20).unwrap();
        assert!(larger > 128_000_000 && larger < powerline_layout::MAXIMUM_WORK);
        assert_eq!(
            routing_work_allowance(1067 * 1067, 4096).unwrap(),
            powerline_layout::MAXIMUM_WORK
        );
        assert!(routing_work_allowance(usize::MAX, 1).is_err());
        assert!(routing_work_allowance(1, usize::MAX).is_err());
    }
}
