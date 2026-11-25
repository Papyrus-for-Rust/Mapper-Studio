//! Source-bound upright coastal fitting. Ocean-plane water only; no runtime physics.
use crate::{installation::SourceFile, monument_attributes::Attributes, prefab_assets::Catalog};
use papyrus_terrain::{boat_navigation::Navigation, native_layers::Layers, spawn};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};
pub const VERSION: &str = "papyrus-placement-coastal-source-6";
pub const BUDGET_VERSION: &str = "papyrus-placement-sized-source-7";
pub fn supported(version: &str) -> bool {
    matches!(version, VERSION | BUDGET_VERSION)
}
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    format: String,
    schema: u32,
    pub id: String,
    native_sha256: String,
    scene_sha256: String,
    code_sources: Vec<SourceFile>,
    map_scene: SourceFile,
    pub sea_level: f32,
    pub boat_type: i32,
    pub boat_cell_size: f32,
    pub minimum_boat_depth: f32,
    pub minimum_tier_corners: usize,
    pub maximum_cut_fill: f32,
    pub candidate_samples: usize,
    stage_filters: BTreeMap<String, spawn::Filter>,
}
impl Profile {
    pub fn validate(&self) -> Result<(), String> {
        let hash = |s: &str| {
            s.len() == 64
                && s.bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        };
        let mut paths = std::collections::BTreeSet::new();
        if self.format != "papyrus-coastal-placement-profile"
            || self.schema != 1
            || self.id.is_empty()
            || self.id.len() > 128
            || !hash(&self.native_sha256)
            || !hash(&self.scene_sha256)
            || self.code_sources.is_empty()
            || self.code_sources.len() > 2
            || self.code_sources.iter().any(|s| {
                !matches!(
                    s.relative_path.as_str(),
                    "GameAssembly.dll"
                        | "RustClient_Data/il2cpp_data/Metadata/global-metadata.dat"
                        | "RustClient_Data/Managed/Assembly-CSharp.dll"
                        | "RustDedicated_Data/Managed/Assembly-CSharp.dll"
                ) || !hash(&s.sha256)
                    || s.bytes == 0
                    || s.bytes > 1024 * 1024 * 1024
                    || !paths.insert(&s.relative_path)
            })
            || self.map_scene.relative_path != "Bundles/maps/maps.bundle"
            || !hash(&self.map_scene.sha256)
            || self.map_scene.bytes == 0
            || self.map_scene.bytes > 512 * 1024 * 1024
            || !self.sea_level.is_finite()
            || self.sea_level != 0.
            || !(0..=64).contains(&self.boat_type)
            || !self.boat_cell_size.is_finite()
            || !(7.5..=64.).contains(&self.boat_cell_size)
            || !self.minimum_boat_depth.is_finite()
            || !(0.1..=100.).contains(&self.minimum_boat_depth)
            || !(1..=4).contains(&self.minimum_tier_corners)
            || !self.maximum_cut_fill.is_finite()
            || !(24. ..=500.).contains(&self.maximum_cut_fill)
            || !(160..=4096).contains(&self.candidate_samples)
            || self.stage_filters.is_empty()
            || self.stage_filters.len() > 16
        {
            return Err("Unsupported coastal placement profile".into());
        }
        for (path, f) in &self.stage_filters {
            if !path.starts_with("assets/bundled/prefabs/autospawn/monument/")
                || !path.ends_with('/')
                || path.contains("..")
                || path.contains('\\')
            {
                return Err("Invalid coastal stage folder".into());
            }
            f.validate()?;
        }
        Ok(())
    }
    pub fn verify(
        &self,
        root: &Path,
        catalog: &Catalog,
        cancelled: impl Fn() -> bool,
    ) -> Result<(), String> {
        self.validate()?;
        if self.native_sha256 != catalog.installation.native.sha256
            || catalog
                .sources
                .iter()
                .find(|s| s.relative_path == "Bundles/shared/assetscenes.bundle")
                .is_none_or(|s| s.sha256 != self.scene_sha256)
        {
            return Err("Coastal profile belongs to another installation".into());
        }
        for r in self
            .code_sources
            .iter()
            .chain(std::iter::once(&self.map_scene))
        {
            if crate::installation::hash_file(
                root,
                &r.relative_path,
                1024 * 1024 * 1024,
                &cancelled,
            )? != *r
            {
                return Err(
                    "Coastal gameplay code or scene differs from selected installation".into(),
                );
            }
        }
        Ok(())
    }
    pub fn available(
        root: &Path,
        catalog: &Catalog,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        if catalog.installation.kind != crate::installation::InstallationKind::Client {
            return Ok(None);
        }
        for raw in [
            include_str!("../../../compatibility/monuments/client-0f703d43-coastal.json"),
            include_str!("../../../compatibility/monuments/client-8dd4e235-coastal.json"),
            include_str!("../../../compatibility/monuments/client-58f3263f-coastal.json"),
        ] {
            let profile: Self = serde_json::from_str(raw).map_err(|error| error.to_string())?;
            if profile.native_sha256 == catalog.installation.native.sha256 {
                profile.verify(root, catalog, &cancelled)?;
                return Ok(Some(profile));
            }
        }
        Ok(None)
    }
    pub fn filter(&self, path: &str) -> Option<&spawn::Filter> {
        self.stage_filters
            .iter()
            .filter(|(prefix, _)| path.starts_with(prefix.as_str()))
            .max_by_key(|(prefix, _)| prefix.len())
            .map(|(_, f)| f)
    }
    pub fn navigation(
        &self,
        layers: &Layers,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Navigation>, String> {
        self.validate()?;
        Navigation::build(
            layers,
            self.boat_cell_size,
            self.minimum_boat_depth,
            |_, _| Some(self.sea_level),
            cancelled,
        )
    }
    pub fn navigation_source(
        &self,
        layers: &Layers,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Navigation>, String> {
        self.validate()?;
        Navigation::build_source(
            layers,
            self.boat_cell_size,
            self.minimum_boat_depth,
            |_, _| Some(self.sea_level),
            cancelled,
        )
    }
}
pub(crate) fn world(local: [f32; 3], origin: [f32; 3], yaw: f32) -> [f32; 3] {
    let (s, c) = yaw.to_radians().sin_cos();
    [
        origin[0] + c * local[0] - s * local[2],
        origin[1] + local[1],
        origin[2] + s * local[0] + c * local[2],
    ]
}
fn height(l: &Layers, p: [f32; 3]) -> Option<f32> {
    let u = p[0] / l.size as f32 + 0.5;
    let v = p[2] / l.size as f32 + 0.5;
    ((0. ..=1.).contains(&u) && (0. ..=1.).contains(&v))
        .then(|| spawn::sample_height(l, u, v).ok())
        .flatten()
}
pub fn swims(a: &Attributes) -> bool {
    a.root_decor.iter().any(|d| d.kind == "DecorSwim")
}
/// Runs only representable unit-scale, upright source programs. Unsupported
/// pitch/roll, scale or offsets are errors rather than silently dropped settings.
pub fn pose(
    l: &Layers,
    a: &Attributes,
    p: [f32; 3],
    proposal_yaw: f32,
    sea: f32,
) -> Result<([f32; 3], f32), String> {
    pose_impl(l, a, p, proposal_yaw, sea, false)
}
/// Inspected source normal sampling; legacy pose remains for saved recipes.
pub fn pose_source_normal(
    l: &Layers,
    a: &Attributes,
    p: [f32; 3],
    proposal_yaw: f32,
    sea: f32,
) -> Result<([f32; 3], f32), String> {
    pose_impl(l, a, p, proposal_yaw, sea, true)
}
pub(crate) struct PreparedPose<'a> {
    layers: &'a Layers,
    attributes: &'a Attributes,
}
impl<'a> PreparedPose<'a> {
    pub fn new(layers: &'a Layers, attributes: &'a Attributes) -> Result<Self, String> {
        attributes.validate_coastal_inputs()?;
        Ok(Self { layers, attributes })
    }
    pub fn source_normal(
        &self,
        position: [f32; 3],
        yaw: f32,
        sea: f32,
    ) -> Result<([f32; 3], f32), String> {
        pose_validated(self.layers, self.attributes, position, yaw, sea, true)
    }
}
fn pose_impl(
    l: &Layers,
    a: &Attributes,
    p: [f32; 3],
    proposal_yaw: f32,
    sea: f32,
    source_normal: bool,
) -> Result<([f32; 3], f32), String> {
    a.validate_coastal_inputs()?;
    pose_validated(l, a, p, proposal_yaw, sea, source_normal)
}
fn pose_validated(
    l: &Layers,
    a: &Attributes,
    mut p: [f32; 3],
    proposal_yaw: f32,
    sea: f32,
    source_normal: bool,
) -> Result<([f32; 3], f32), String> {
    let (hr, _) = papyrus_terrain::native_layers::dimensions(l.size)?;
    if l.height_resolution != hr || l.height.len() != hr * hr {
        return Err("Invalid coastal root terrain".into());
    }
    if !a.issues.is_empty()
        || !proposal_yaw.is_finite()
        || !sea.is_finite()
        || p.iter().any(|v| !v.is_finite())
    {
        return Err("Unsupported coastal root program".into());
    }
    let rotations = a
        .root_decor
        .iter()
        .filter(|d| d.kind == "DecorRotate")
        .count();
    if rotations > 1 {
        return Err("Multiple coastal random rotations require another adapter".into());
    }
    let mut yaw = if rotations == 0 { proposal_yaw } else { 0. };
    for d in &a.root_decor {
        let v = &d.values;
        match d.kind.as_str() {
            "DecorSwim" => p[1] = sea,
            "DecorRotate" => {
                if v[0] != 0. || v[2] != 0. || v[3] != 0. || v[5] != 0. {
                    return Err("Coastal root rotation is not upright".into());
                }
                yaw += proposal_yaw;
            }
            "DecorScale" => {
                if v.iter().any(|v| *v != 1.) {
                    return Err("Coastal scale is not unit".into());
                }
            }
            "DecorTransform" => {
                if v[3] != 0. || v[5] != 0. || v[6..] != [1., 1., 1.] {
                    return Err("Coastal fixed transform is not upright/unit scale".into());
                }
                p = world([v[0], v[1], v[2]], p, yaw);
                yaw -= v[4];
            }
            "DecorAlign" => {
                if v[0] != 0.
                    || !(0. ..=1.).contains(&v[1])
                    || v[2..5] != [0., 0., 0.]
                    || v[5..] != [1., 1., 1.]
                {
                    return Err("Coastal alignment requires pitch/roll or scale".into());
                }
                let n = if source_normal {
                    papyrus_terrain::terrain_normal::sample(
                        l.size,
                        l.height_resolution,
                        &l.height,
                        [p[0] / l.size as f32 + 0.5, p[2] / l.size as f32 + 0.5],
                    )?
                } else {
                    let step = l.size as f32 / (l.height_resolution - 1) as f32;
                    let mut left = p;
                    let mut right = p;
                    left[0] -= step;
                    right[0] += step;
                    let dx = (height(l, right).ok_or("Coastal normal lies outside terrain")?
                        - height(l, left).ok_or("Coastal normal lies outside terrain")?)
                        / (2. * step);
                    left = p;
                    right = p;
                    left[2] -= step;
                    right[2] += step;
                    let dz = (height(l, right).ok_or("Coastal normal lies outside terrain")?
                        - height(l, left).ok_or("Coastal normal lies outside terrain")?)
                        / (2. * step);
                    let magnitude = (dx * dx + dz * dz + 1.).sqrt();
                    [-dx / magnitude, 1. / magnitude, -dz / magnitude]
                };
                let direction = if n[0] * n[0] + n[2] * n[2] < 1e-10 {
                    [1., 0.]
                } else {
                    [n[0] * n[1], n[2] * n[1]]
                };
                let (s, c) = yaw.to_radians().sin_cos();
                let g = v[1];
                let f = [
                    -s * (1. - g) + direction[0] * g,
                    c * (1. - g) + direction[1] * g,
                ];
                if f[0].hypot(f[1]) < 1e-8 {
                    return Err("Degenerate coastal forward direction".into());
                }
                yaw = -f[0].atan2(f[1]).to_degrees();
            }
            _ => return Err("Unsupported coastal decorator".into()),
        }
    }
    if p.iter().any(|v| !v.is_finite() || v.abs() > 10000.) || !yaw.is_finite() {
        return Err("Invalid fitted coastal pose".into());
    }
    Ok((p, yaw.rem_euclid(360.)))
}
pub fn checks(
    l: &Layers,
    a: &Attributes,
    p: [f32; 3],
    yaw: f32,
    profile: &Profile,
    filter: &spawn::Filter,
    nav: &Navigation,
) -> Result<(), String> {
    a.validate_coastal_inputs()?;
    profile.validate()?;
    filter.validate()?;
    if nav.size() != l.size || !yaw.is_finite() || p.iter().any(|v| !v.is_finite()) {
        return Err("Coastal check context mismatch".into());
    }
    let factor = |p: [f32; 3]| {
        let u = p[0] / l.size as f32 + 0.5;
        let v = p[2] / l.size as f32 + 0.5;
        (0. ..=1.).contains(&u)
            && (0. ..=1.).contains(&v)
            && spawn::sample_factor(l, filter, u, v).is_ok_and(|f| f > 0.)
    };
    for c in &a.terrain_checks {
        let q = world(c.position, p, if c.rotate { yaw } else { 0. });
        if !factor(q) || height(l, q).is_none_or(|h| h < q[1] - c.extents || h > q[1] + c.extents) {
            return Err("Authored coastal terrain check failed".into());
        }
    }
    for c in &a.water_checks {
        if world(c.position, p, if c.rotate { yaw } else { 0. })[1] > profile.sea_level {
            return Err("Authored coastal water check failed".into());
        }
    }
    for c in &a.path_connectors {
        if c.infrastructure_type == profile.boat_type
            && !nav.reaches_edge(world(c.position, p, yaw))
        {
            return Err("Coastal boat connector cannot reach deep map-edge water".into());
        }
    }
    Ok(())
}

/// Recheck navigation against committed terrain, including all monument and decor
/// effects. A provisional base-terrain path cannot certify final harbor access.
pub fn validate_final(
    project: &crate::Project,
    solution: &papyrus_terrain::placement::Solution,
    catalog: &Catalog,
    bindings: &crate::prefab_textures::Bindings,
    maps: &papyrus_terrain::monument_composition::Composed,
    cancelled: impl Fn() -> bool,
) -> Result<bool, String> {
    validate_final_with_source(
        project,
        solution,
        &maps.layers,
        || {
            crate::monument_source::Source::cached_shared(
                project
                    .monument_rules
                    .as_ref()
                    .ok_or("Missing coastal recipe")?,
                catalog,
                bindings,
                &cancelled,
            )
        },
        &cancelled,
    )
}

/// Keep one accepted navigation corridor per boat connector through later
/// object/decor height transactions, sharing the existing dependency bitset.
pub(crate) fn reserve_boat_routes(
    project: &crate::Project,
    solution: &papyrus_terrain::placement::Solution,
    catalog: &Catalog,
    bindings: &crate::prefab_textures::Bindings,
    maps: &mut papyrus_terrain::monument_composition::Composed,
    reservations: &mut Option<papyrus_terrain::height_reservations::HeightReservations>,
    cancelled: impl Fn() -> bool,
) -> Result<bool, String> {
    if !supported(&project.placement_engine)
        || (project.objects.is_none() && project.decoration.is_none())
    {
        return Ok(!cancelled());
    }
    let recipe = project
        .monument_rules
        .as_ref()
        .ok_or("Missing coastal recipe")?;
    let source = crate::monument_source::Source::cached_shared(recipe, catalog, bindings, &cancelled)?;
    let profile = source
        .coastal_profile
        .as_ref()
        .ok_or("Missing coastal profile")?;
    let mut points = Vec::new();
    for outcome in &solution.outcomes {
        let Some(pose) = &outcome.position else {
            continue;
        };
        let target = project
            .targets
            .iter()
            .find(|target| target.instance_id == outcome.id)
            .ok_or("Unknown coastal target")?;
        let attributes = target
            .prefab_path
            .as_ref()
            .and_then(|path| source.prefabs.get(path))
            .and_then(|prefab| prefab.placement_attributes.as_ref())
            .ok_or("Missing captured coastal attributes")?;
        if swims(attributes) {
            for connector in &attributes.path_connectors {
                if connector.infrastructure_type == profile.boat_type {
                    points.push(world(
                        connector.position,
                        [pose.x, pose.y, pose.z],
                        pose.yaw,
                    ));
                }
            }
        }
    }
    if points.is_empty() {
        return Ok(!cancelled());
    }
    let source_navigation =
        recipe.coastal_navigation.as_deref() == Some(papyrus_terrain::boat_navigation::SOURCE_VERSION);
    let navigation = if source_navigation {
        profile.navigation_source(&maps.layers, &cancelled)
    } else {
        profile.navigation(&maps.layers, &cancelled)
    };
    let Some(navigation) = navigation? else {
        return Ok(false);
    };
    if reservations.is_none() {
        *reservations = papyrus_terrain::height_reservations::HeightReservations::new(
            papyrus_terrain::path_terrain::Grid {
                size: maps.layers.size as f32,
                resolution: maps.layers.height_resolution,
                origin_y: -500.,
                vertical_span: 1000.,
                height: &maps.layers.height,
            },
            &cancelled,
        )?;
    }
    let Some(reservations) = reservations else {
        return Ok(false);
    };
    if !reservations.validate_binding(&maps.layers, &cancelled)? {
        return Ok(false);
    }
    let before = reservations.samples();
    let mut work = 0;
    let maximum_work = 128_000_000;
    for &point in &points {
        let Some(route) = navigation.route_support(point, maximum_work - work, &cancelled)? else {
            return Ok(false);
        };
        work += route.work;
        for uv in route.samples {
            if cancelled() {
                return Ok(false);
            }
            if source_navigation {
                let point =
                    uv.map(|v| -(maps.layers.size as f32) * 0.5 + v * maps.layers.size as f32);
                reservations.reserve_query(point)?;
            } else {
                let n = maps.layers.height_resolution;
                let [x, z] = uv.map(|v| (v * (n - 1) as f32).floor() as usize);
                for z in [z, (z + 1).min(n - 1)] {
                    for x in [x, (x + 1).min(n - 1)] {
                        reservations.reserve(z * n + x)?;
                    }
                }
            }
        }
    }
    if let Some(surface) = maps
        .roads
        .as_mut()
        .and_then(|roads| roads.surfaces.as_mut())
    {
        surface.report["staged_pipeline"]["coastal_navigation_support"] = serde_json::json!({
            "engine":"papyrus-coastal-height-support-1", "connectors":points.len(),
            "samples":reservations.samples()-before, "work":work,
            "scope":"boat-depth raster paths and diagonal clearance",
            "runtime_navigation_validated":false,
        });
    }
    Ok(!cancelled())
}

/// Opt-in snapshots locate the stage that first changes harbor navigation.
#[cfg(feature = "road-diagnostics")]
pub(crate) fn capture_stage(
    stage: &str,
    project: &crate::Project,
    solution: &papyrus_terrain::placement::Solution,
    catalog: &Catalog,
    bindings: &crate::prefab_textures::Bindings,
    maps: &papyrus_terrain::monument_composition::Composed,
    cancelled: impl Fn() -> bool,
) -> Result<(), String> {
    use std::{fs, io::Write};
    let Some(root) = std::env::var_os("MAPPER_COASTAL_STAGE_CAPTURE") else {
        return Ok(());
    };
    let root = std::path::Path::new(&root);
    if stage == "monuments" {
        fs::create_dir(root).map_err(|e| format!("New coastal capture directory: {e}"))?;
    }
    let validation = validate_final(project, solution, catalog, bindings, maps, &cancelled);
    let outcomes: Vec<_> = solution
        .outcomes
        .iter()
        .map(|outcome| {
            serde_json::json!({
                "id":outcome.id,
                "position":outcome.position.as_ref().map(|p| [p.x,p.y,p.z,p.yaw]),
                "reason":outcome.reason,
            })
        })
        .collect();
    let data = serde_json::to_vec(&serde_json::json!({
        "format":"mapper-private-coastal-stage-1", "stage":stage,
        "project":project, "outcomes":outcomes, "size":maps.layers.size,
        "height_resolution":maps.layers.height_resolution,
        "height":maps.layers.height, "validation":validation,
    }))
    .map_err(|e| e.to_string())?;
    if data.len() > 128 * 1024 * 1024 {
        return Err("Coastal stage capture exceeds allowance".into());
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join(format!("{stage}.json")))
        .map_err(|e| e.to_string())?;
    file.write_all(&data)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    validation.map_err(|error| format!("Coastal {stage} stage rejected: {error}"))?;
    Ok(())
}

fn validate_final_with_source(
    project: &crate::Project,
    solution: &papyrus_terrain::placement::Solution,
    layers: &Layers,
    load: impl FnOnce() -> Result<std::sync::Arc<crate::monument_source::Source>, String>,
    cancelled: impl Fn() -> bool,
) -> Result<bool, String> {
    if !supported(&project.placement_engine) {
        return Ok(!cancelled());
    }
    if cancelled() {
        return Ok(false);
    }
    let source = load()?;
    let profile = source
        .coastal_profile
        .as_ref()
        .ok_or("Missing coastal profile")?;
    let source_navigation = project.monument_rules.as_ref().is_some_and(|r| {
        r.coastal_navigation.as_deref() == Some(papyrus_terrain::boat_navigation::SOURCE_VERSION)
    });
    let navigation = if source_navigation {
        profile.navigation_source(layers, &cancelled)
    } else {
        profile.navigation(layers, &cancelled)
    };
    let Some(nav) = navigation? else {
        return Ok(false);
    };
    for outcome in &solution.outcomes {
        if cancelled() {
            return Ok(false);
        }
        let Some(p) = &outcome.position else {
            continue;
        };
        let target = project
            .targets
            .iter()
            .find(|t| t.instance_id == outcome.id)
            .ok_or("Unknown coastal target")?;
        let path = target
            .prefab_path
            .as_deref()
            .ok_or("Missing coastal prefab identity")?;
        let attributes = source
            .prefabs
            .get(path)
            .and_then(|p| p.placement_attributes.as_ref())
            .ok_or("Missing captured coastal attributes")?;
        if !swims(attributes) {
            continue;
        }
        for connector in &attributes.path_connectors {
            if connector.infrastructure_type == profile.boat_type
                && !nav.reaches_edge(world(connector.position, [p.x, p.y, p.z], p.yaw))
            {
                return Err(format!(
                    "Final terrain blocks boat access for {} (connector {})",
                    target.name, connector.object_id
                ));
            }
        }
    }
    Ok(!cancelled())
}

pub fn boat_access(
    a: &Attributes,
    p: [f32; 3],
    yaw: f32,
    profile: &Profile,
    nav: &Navigation,
) -> Result<(), String> {
    a.validate_coastal_inputs()?;
    profile.validate()?;
    if !yaw.is_finite() || p.iter().any(|v| !v.is_finite()) {
        return Err("Invalid coastal boat pose".into());
    }
    for c in &a.path_connectors {
        if c.infrastructure_type == profile.boat_type
            && !nav.reaches_edge(world(c.position, p, yaw))
        {
            return Err("Coastal stamp blocks a boat connector's deep-water route".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn terrain() -> Layers {
        let (hr, n) = papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let height = (0..hr * hr)
            .map(|i| {
                let x = (i % hr) as f32 / (hr - 1) as f32 * 1000. - 500.;
                ((x * 0.02 + 500.) / 1000. * 32766. + 0.5) as i16
            })
            .collect();
        Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: n,
            height,
            biome: vec![255; n * n * 5],
            splat: vec![255; n * n * 8],
            topology: vec![1; n * n],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        }
    }
    fn attributes(turn: bool) -> Attributes {
        let mut d = serde_json::json!({"version":1,"root_decor":[{"object_id":1,"active":true,"kind":"DecorSwim","values":[]},{"object_id":2,"active":true,"kind":"DecorAlign","values":[0,1,0,0,0,1,1,1]}],"terrain_checks":[{"object_id":4,"active":true,"position":[0,-1,-20],"rotate":true,"extents":2}],"water_checks":[],"path_connectors":[{"object_id":5,"active":false,"position":[0,-1,250],"infrastructure_type":5}],"issues":[]});
        if turn {
            d["root_decor"].as_array_mut().unwrap().push(serde_json::json!({"object_id":3,"active":true,"kind":"DecorTransform","values":[0,0,0,0,-90,0,1,1,1]}));
            d["terrain_checks"][0]["position"] = serde_json::json!([-20, -1, 0]);
            d["path_connectors"][0]["position"] = serde_json::json!([250, -1, 0]);
        }
        serde_json::from_value(d).unwrap()
    }
    fn profile() -> Profile {
        serde_json::from_str(include_str!(
            "../../../compatibility/monuments/client-0f703d43-coastal.json"
        ))
        .unwrap()
    }
    #[test]
    fn prepared_pose_replays_source_operations_and_rejects_invalid_attributes() {
        let layers = terrain();
        for turn in [false, true] {
            let attributes = attributes(turn);
            let prepared = PreparedPose::new(&layers, &attributes).unwrap();
            for position in [[0.; 3], [12., 37., 56.], [-350., 15., 320.]] {
                for yaw in [0., 17., 90., 217., 359.] {
                    assert_eq!(
                        prepared.source_normal(position, yaw, 0.),
                        pose_source_normal(&layers, &attributes, position, yaw, 0.)
                    );
                }
            }
            assert!(prepared.source_normal([f32::NAN, 0., 0.], 0., 0.).is_err());
        }
        let mut invalid = attributes(false);
        invalid.root_decor[1].values[0] = f32::NAN;
        assert!(PreparedPose::new(&layers, &invalid).is_err());
    }
    #[test]
    fn current_coastal_profile_uses_the_current_numeric_and_stage_sources() {
        let profile: Profile = serde_json::from_str(include_str!(
            "../../../compatibility/monuments/client-8dd4e235-coastal.json"
        ))
        .unwrap();
        profile.validate().unwrap();
        let numeric: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/monuments/client-8dd4e235.json"
        ))
        .unwrap();
        let stages: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/monuments/client-8dd4e235-stages.json"
        ))
        .unwrap();
        assert_eq!(
            profile.native_sha256,
            numeric["native_sha256"].as_str().unwrap()
        );
        assert_eq!(
            profile.scene_sha256,
            numeric["scene_sha256"].as_str().unwrap()
        );
        assert_eq!(
            serde_json::to_value(&profile.code_sources).unwrap(),
            numeric["code_sources"]
        );
        assert_eq!(
            serde_json::to_value(&profile.code_sources).unwrap(),
            stages["code_sources"]
        );
        assert_eq!(
            serde_json::to_value(&profile.map_scene).unwrap(),
            stages["map_scene"]
        );
        assert_eq!(profile.boat_type, 5);
        assert_eq!(profile.boat_cell_size, 7.5);
        assert_eq!(profile.minimum_boat_depth, 4.);
        assert_eq!(profile.minimum_tier_corners, 1);
        assert_eq!(profile.stage_filters.len(), 3);
        assert!(
            profile
                .filter("assets/bundled/prefabs/autospawn/monument/harbor/harbor_1.prefab")
                .is_some()
        );
        assert!(
            profile
                .filter("assets/bundled/prefabs/autospawn/monument/oilrig/oilrig_1.prefab")
                .is_none()
        );
    }
    #[test]
    fn captured_environment_metadata_preserves_the_existing_coastal_pose_program() {
        let terrain = terrain();
        let legacy = attributes(true);
        let mut document = serde_json::to_value(&legacy).unwrap();
        document["version"] = 3.into();
        document["placement_blockers"] = serde_json::json!([]);
        document["environment"] = serde_json::json!({"root_transform":{"rotation":[0,0,0,1],"scale":[1,1,1]},"volumes":[{"object_id":100,"hierarchy_active":false,"component_enabled":true,"type_mask":17,"center":[50,20,-80],"size":[200,50,100],"local_to_root":[[1,0,0,12],[0,1,0,4],[0,0,1,5],[0,0,0,1]],"collider_types":[64],"collider_ids":[101],"rotation_chain":[[0,0,0,1]],"field_names":["Center","Size","Type"]}],"checks":[],"issues":[]});
        let current: Attributes = serde_json::from_value(document).unwrap();
        current.validate().unwrap();
        for yaw in [-180., -73.4, 0., 90., 255.74] {
            assert_eq!(
                pose_source_normal(&terrain, &legacy, [12., 37., 56.], yaw, 0.),
                pose_source_normal(&terrain, &current, [12., 37., 56.], yaw, 0.)
            );
        }
        assert_eq!(current.audit()["environment"]["executed"], false);
    }
    #[test]
    fn partial_alignment_must_replay_the_proposal_instead_of_its_own_output() {
        let terrain = terrain();
        let mut attributes = attributes(false);
        attributes
            .root_decor
            .iter_mut()
            .find(|d| d.kind == "DecorAlign")
            .unwrap()
            .values[1] = 0.5;
        let raw_position = [0., 37., 0.];
        let raw_yaw = 0.;
        let first = pose_source_normal(&terrain, &attributes, raw_position, raw_yaw, 0.).unwrap();
        let replay = pose_source_normal(&terrain, &attributes, raw_position, raw_yaw, 0.).unwrap();
        assert_eq!(first, replay);
        let twice = pose_source_normal(&terrain, &attributes, first.0, first.1, 0.).unwrap();
        assert_ne!(
            first.1.to_bits(),
            twice.1.to_bits(),
            "Alignment is not an idempotent pose validator"
        );
    }
    #[test]
    fn final_navigation_uses_the_saved_height_sampling_policy_at_the_depth_threshold() {
        use papyrus_terrain::{
            path_terrain::{Grid, Layout},
            placement::{Outcome, Position, Solution},
        };
        let mut layers = terrain();
        layers.height.fill(15000);
        let u = (108_f32 + 0.5) / 133.;
        let v = (127_f32 + 0.5) / 133.;
        let point = [-500. + u * 1000., 0., -500. + v * 1000.];
        let support = papyrus_terrain::path_terrain::height_support(
            Layout {
                size: 1000.,
                resolution: layers.height_resolution,
                origin_y: -500.,
                vertical_span: 1000.,
            },
            point,
        )
        .unwrap();
        for (i, h) in support
            .indices
            .into_iter()
            .zip([16266, 16271, 16245, 16241])
        {
            layers.height[i] = h;
        }
        assert_eq!(
            papyrus_terrain::path_terrain::sample_height(
                Grid {
                    size: 1000.,
                    resolution: layers.height_resolution,
                    origin_y: -500.,
                    vertical_span: 1000.,
                    height: &layers.height
                },
                point
            )
            .unwrap(),
            -4.
        );
        let source = || {
            let mut prefab:crate::monument_source::Prefab=serde_json::from_value(serde_json::json!({"rules":{"tier_mask":1,"min_corners":3,"anchors":[],"filters":[],"unsupported":[],"pending":[]},"model":{"vertices":[],"triangles":[],"issues":[],"complete":false}})).unwrap();
            let mut a = attributes(false);
            a.path_connectors[0].position = [0.; 3];
            prefab.placement_attributes = Some(a);
            Ok(std::sync::Arc::new(crate::monument_source::Source {
                sha256: "a".repeat(64),
                prefabs: [("test.prefab".into(), prefab)].into(),
                models_requested: false,
                coastal_profile: Some(profile()),
                stage_source: None,
                dungeon_source: None, offshore_source: None,
                code_sources: Vec::new(),
                extractor_sha256: "a".repeat(64),
                profile_sha256: "a".repeat(64),
            }))
        };
        let mut project = crate::Project {
            size: 1000,
            placement_engine: BUDGET_VERSION.into(),
            ..Default::default()
        };
        project.targets.truncate(1);
        project.targets[0].prefab_path = Some("test.prefab".into());
        let solution = Solution {
            outcomes: vec![Outcome {
                id: project.targets[0].instance_id,
                position: Some(Position {
                    x: point[0],
                    y: 0.,
                    z: point[2],
                    yaw: 0.,
                    clearance: 1.,
                }),
                reason: "test".into(),
                distance: Some(0.),
                within_radius: true,
            }],
            required_missing: 0,
            attempts: 0,
        };
        // Existing recipes omit the optional policy and retain their old result.
        assert!(
            validate_final_with_source(&project, &solution, &layers, source, || false).is_err()
        );
        project.monument_rules = Some(source().unwrap().recipe());
        assert_eq!(
            project
                .monument_rules
                .as_ref()
                .unwrap()
                .coastal_navigation
                .as_deref(),
            Some(papyrus_terrain::boat_navigation::SOURCE_VERSION)
        );
        assert!(
            validate_final_with_source(&project, &solution, &layers, source, || false).unwrap()
        );
        project.monument_rules.as_mut().unwrap().coastal_navigation = None;
        assert!(
            validate_final_with_source(&project, &solution, &layers, source, || false).is_err()
        );
    }
    #[test]
    fn every_coastal_revision_rechecks_boat_access_after_final_terrain_changes() {
        use papyrus_terrain::placement::{Outcome, Position, Solution};
        let source = || {
            let mut prefab: crate::monument_source::Prefab = serde_json::from_value(serde_json::json!({
                "rules": {"tier_mask":1,"min_corners":3,"anchors":[],"filters":[],"unsupported":[],"pending":[]},
                "model": {"vertices":[],"triangles":[],"issues":[],"complete":false},
            })).unwrap();
            prefab.placement_attributes = Some(attributes(false));
            Ok(std::sync::Arc::new(crate::monument_source::Source {
                sha256: "a".repeat(64),
                prefabs: [("test.prefab".into(), prefab)].into(),
                models_requested: false,
                coastal_profile: Some(profile()),
                stage_source: None,
                dungeon_source: None, offshore_source: None,
                code_sources: Vec::new(),
                extractor_sha256: "a".repeat(64),
                profile_sha256: "a".repeat(64),
            }))
        };
        for engine in [VERSION, BUDGET_VERSION] {
            let mut project = crate::Project {
                placement_engine: engine.into(),
                ..crate::Project::default()
            };
            project.targets.truncate(1);
            project.targets[0].prefab_path = Some("test.prefab".into());
            let solution = Solution {
                outcomes: vec![Outcome {
                    id: project.targets[0].instance_id,
                    position: Some(Position {
                        x: 0.,
                        y: 0.,
                        z: 0.,
                        yaw: 0.,
                        clearance: 0.,
                    }),
                    distance: Some(0.),
                    within_radius: true,
                    reason: String::new(),
                }],
                required_missing: 0,
                attempts: 1,
            };
            let mut final_layers = terrain();
            // Open ocean: even the captured inactive boat connector must be checked.
            final_layers.height.fill(15000);
            assert!(
                validate_final_with_source(&project, &solution, &final_layers, source, || false)
                    .unwrap()
            );
            // Later earthworks can close the connector's route after candidate acceptance.
            final_layers.height.fill(18000);
            let result =
                validate_final_with_source(&project, &solution, &final_layers, source, || false);
            assert!(
                result.is_err(),
                "{engine} published a blocked final boat route"
            );
            let error = result.unwrap_err();
            assert!(error.contains("Final terrain blocks boat access"));
            assert!(error.contains("connector 5"));
            assert!(
                !validate_final_with_source(
                    &project,
                    &solution,
                    &final_layers,
                    || panic!("Cancelled validation loaded source"),
                    || true
                )
                .unwrap()
            );
        }
        let project = crate::Project::default();
        let solution = Solution {
            outcomes: vec![],
            required_missing: 0,
            attempts: 0,
        };
        assert!(
            validate_final_with_source(
                &project,
                &solution,
                &terrain(),
                || panic!("Legacy placement loaded coastal source"),
                || false
            )
            .unwrap()
        );
    }
    #[test]
    fn harbor_programs_use_water_height_downhill_heading_and_fixed_yaw_before_checks() {
        let l = terrain();
        let profile = profile();
        let filter = profile
            .filter("assets/bundled/prefabs/autospawn/monument/harbor/harbor_1.prefab")
            .unwrap();
        let nav = profile.navigation(&l, || false).unwrap().unwrap();
        for turn in [false, true] {
            let a = attributes(turn);
            let (p, yaw) = pose(&l, &a, [0., 80., 0.], 23., profile.sea_level).unwrap();
            assert_eq!(p[1], 0.);
            assert!((yaw - if turn { 180. } else { 90. }).abs() < 0.001);
            assert!(world(a.terrain_checks[0].position, p, yaw)[0] > 19.9);
            checks(&l, &a, p, yaw, &profile, filter, &nav).unwrap();
            assert!(checks(&l, &a, [300., 0., 0.], yaw, &profile, filter, &nav).is_err());
            assert!(checks(&l, &a, [0., 20., 0.], yaw, &profile, filter, &nav).is_err());
        }
        let mut bad = attributes(false);
        bad.root_decor[1].values[0] = 1.;
        assert!(pose(&l, &bad, [0.; 3], 0., 0.).is_err());
        let mut bad_layers = l;
        bad_layers.height_resolution = 0;
        assert!(pose(&bad_layers, &attributes(false), [0.; 3], 0., 0.).is_err());
    }
    #[test]
    fn source_normal_policy_uses_diagonal_heights_and_retains_legacy_pose() {
        let mut l = terrain();
        l.height.fill(16000);
        let n = l.height_resolution;
        let c = n / 2;
        l.height[(c - 1) * n + c + 1] = 18000;
        l.height[(c + 1) * n + c - 1] = 17000;
        let attributes = attributes(false);
        let legacy = pose(&l, &attributes, [0.; 3], 0., 0.).unwrap();
        let source = pose_source_normal(&l, &attributes, [0.; 3], 0., 0.).unwrap();
        assert!((legacy.1 - 270.).abs() < 1e-5);
        let normal = papyrus_terrain::terrain_normal::sample(l.size, n, &l.height, [0.5, 0.5]).unwrap();
        let expected = (-normal[0].atan2(normal[2]).to_degrees()).rem_euclid(360.);
        assert!((source.1 - expected).abs() < 1e-4);
        assert!((source.1 - legacy.1).abs() > 90.);
        assert_eq!(source.0, [0.; 3]);
        assert_eq!(pose(&l, &attributes, [0.; 3], 0., 0.).unwrap(), legacy);
    }
    #[test]
    fn profile_receipts_recipe_roundtrip_and_engine_coherence_are_required() {
        let root = tempfile::tempdir().unwrap();
        let (catalog, _) = crate::infrastructure::tests::fixture();
        let mut p = profile();
        p.native_sha256 = catalog.installation.native.sha256.clone();
        p.scene_sha256 = "c".repeat(64);
        std::fs::create_dir_all(root.path().join("Bundles/maps")).unwrap();
        std::fs::write(root.path().join("GameAssembly.dll"), b"code").unwrap();
        std::fs::write(root.path().join("Bundles/maps/maps.bundle"), b"scene").unwrap();
        p.code_sources = vec![
            crate::installation::hash_file(root.path(), "GameAssembly.dll", 1024, &|| false)
                .unwrap(),
        ];
        p.map_scene =
            crate::installation::hash_file(root.path(), "Bundles/maps/maps.bundle", 1024, &|| {
                false
            })
            .unwrap();
        p.verify(root.path(), &catalog, || false).unwrap();
        let mut project = crate::Project {
            preview_engine: crate::native_preview::VERSION.into(),
            installation: Some(catalog.installation.clone()),
            monument_assets: Some(crate::AssetIdentity {
                catalog_sha256: catalog.sha256.clone(),
                bindings_sha256: "f".repeat(64),
            }),
            ..crate::Project::default()
        };
        let key = project.terrain_key();
        project.placement_engine = VERSION.into();
        project.monument_rules = Some(crate::monument_source::Recipe {
            engine: VERSION.into(),
            source_sha256: "e".repeat(64),
            coastal_profile: Some(p.clone()),
            tier_search: None,
            earthwork: None,
            coastal_normals: None,
            coastal_navigation: None,
            coastal_search: None,
            canonical_yaw: None,
            land_alignment: None,
            surface_modifiers: None,
            stage_rules: None,
            dungeon_source: None, offshore_source: None,
        });
        project.validate().unwrap();
        assert_eq!(project.terrain_key(), key);
        let restored: crate::Project =
            serde_json::from_slice(&serde_json::to_vec(&project).unwrap()).unwrap();
        assert_eq!(project, restored);
        project.placement_engine = BUDGET_VERSION.into();
        project.monument_rules.as_mut().unwrap().engine = BUDGET_VERSION.into();
        project.monument_rules.as_mut().unwrap().tier_search =
            Some(crate::monument_source::TIER_RETRY_VERSION.into());
        project.monument_rules.as_mut().unwrap().earthwork =
            Some(crate::monument_source::AUTHORED_EARTHWORK_VERSION.into());
        assert!(
            !serde_json::to_string(&project)
                .unwrap()
                .contains("coastal_normals")
        );
        project.monument_rules.as_mut().unwrap().coastal_normals =
            Some(papyrus_terrain::terrain_normal::VERSION.into());
        assert!(
            !serde_json::to_string(&project)
                .unwrap()
                .contains("coastal_navigation")
        );
        project.monument_rules.as_mut().unwrap().coastal_navigation =
            Some(papyrus_terrain::boat_navigation::SOURCE_VERSION.into());
        project.validate().unwrap();
        assert_eq!(project.terrain_key(), key);
        let restored: crate::Project =
            serde_json::from_slice(&serde_json::to_vec(&project).unwrap()).unwrap();
        assert_eq!(project, restored);
        project.monument_rules.as_mut().unwrap().coastal_navigation = Some("unknown".into());
        assert!(project.validate().is_err());
        project.monument_rules.as_mut().unwrap().coastal_navigation =
            Some(papyrus_terrain::boat_navigation::SOURCE_VERSION.into());
        project.monument_rules.as_mut().unwrap().coastal_normals = Some("unknown".into());
        assert!(project.validate().is_err());
        project.monument_rules.as_mut().unwrap().coastal_normals =
            Some(papyrus_terrain::terrain_normal::VERSION.into());
        project.monument_rules.as_mut().unwrap().earthwork = Some("unknown".into());
        assert!(project.validate().is_err());
        project.monument_rules.as_mut().unwrap().earthwork =
            Some(crate::monument_source::AUTHORED_EARTHWORK_VERSION.into());
        project.monument_rules.as_mut().unwrap().tier_search = Some("unknown".into());
        assert!(project.validate().is_err());
        project.monument_rules.as_mut().unwrap().tier_search =
            Some(crate::monument_source::TIER_RETRY_VERSION.into());
        project.placement_engine = crate::monument_source::VERSION.into();
        assert!(project.validate().is_err());
        std::fs::write(root.path().join("GameAssembly.dll"), b"changed").unwrap();
        assert!(p.verify(root.path(), &catalog, || false).is_err());
        assert!(p.verify(root.path(), &catalog, || true).is_err());
        p.candidate_samples = 0;
        assert!(p.validate().is_err());
    }
}
