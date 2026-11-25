//! Packaged road-metadata and mesh-template preparation from the selected installation.
use crate::{
    infrastructure::Source,
    metadata_runtime::{self, Script},
    prefab_assets::Catalog,
    road_surface_source,
};
use std::path::Path;
const PROFILES: [&str; 3] = [
    include_str!("../../../compatibility/infrastructure/client-0f703d43-road-topology.json"),
    include_str!("../../../compatibility/infrastructure/client-8dd4e235-road-topology.json"),
    include_str!("../../../compatibility/infrastructure/client-58f3263f-road-topology.json"),
];
fn preparation_profile(catalog: &Catalog) -> Result<&'static str, String> {
    for raw in PROFILES {
        let profile: serde_json::Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
        if profile["native_sha256"].as_str() == Some(&catalog.installation.native.sha256)
            && catalog.sources.iter().any(|s| {
                s.relative_path == "Bundles/shared/assetscenes.bundle"
                    && profile["scene_sha256"] == s.sha256
            })
        {
            return Ok(raw);
        }
    }
    Err("No matching road preparation adapter for this installation".into())
}
pub struct OrderedInputs {
    pub assets: std::sync::Arc<crate::source_road_assets::Assets>,
    pub roles: crate::source_road_inputs::Roles,
}

pub(crate) struct CapturedInputs {
    surfaces: road_surface_source::Source,
    ordered: Option<crate::source_road_assets::Assets>,
}

impl CapturedInputs {
    pub(crate) fn into_startup_inputs(
        self,
        source: &Source,
    ) -> Result<(road_surface_source::Source, OrderedInputs), String> {
        let roles = crate::source_road_inputs::Roles::for_source(source)?
            .ok_or("Road connectors unavailable")?;
        let assets = self.ordered.ok_or("Road connectors unavailable")?;
        Ok((
            self.surfaces,
            OrderedInputs {
                assets: std::sync::Arc::new(assets),
                roles,
            },
        ))
    }
}
/// On-demand part of normal road preparation. It neither activates generation
/// nor rewrites asset pins in an existing recipe. Unknown gameplay code stays
/// explicitly unsupported by the ordered-layout adapter.
pub fn prepare_ordered(
    root: &Path,
    catalog: &Catalog,
    source: &Source,
    pin: Option<&str>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<OrderedInputs>, String> {
    if cancelled() {
        return Ok(None);
    }
    let Some(roles) = crate::source_road_inputs::Roles::for_source(source)? else {
        return Ok(None);
    };
    let Some(assets) = crate::source_road_cache::prepare(
        crate::source_road_cache::Context {
            root,
            catalog,
            infrastructure: source,
        },
        pin,
        cancelled,
    )?
    else {
        return Ok(None);
    };
    Ok(Some(OrderedInputs {
        assets: std::sync::Arc::new(assets),
        roles,
    }))
}
pub fn prepare(
    root: &Path,
    catalog: &Catalog,
    pinned: Option<&str>,
    cancelled: impl Fn() -> bool,
) -> Result<Source, String> {
    prepare_source(root, catalog, pinned, false, &cancelled).map(|(source, _)| source)
}

pub(crate) fn prepare_startup(
    root: &Path,
    catalog: &Catalog,
    pinned: Option<&str>,
    cancelled: impl Fn() -> bool,
) -> Result<(Source, Option<CapturedInputs>), String> {
    prepare_source(root, catalog, pinned, true, &cancelled)
}

fn prepare_source(
    root: &Path,
    catalog: &Catalog,
    pinned: Option<&str>,
    capture_ordered: bool,
    cancelled: &impl Fn() -> bool,
) -> Result<(Source, Option<CapturedInputs>), String> {
    match Source::discover(catalog, pinned, cancelled) {
        Ok(source) => {
            source.verify_installation(root, cancelled)?;
            return Ok((source, None));
        }
        Err(error) if pinned.is_some() => return Err(error),
        Err(error) if error.starts_with("No matching extracted infrastructure source") => {}
        Err(error) => return Err(error),
    }
    let local = crate::storage::local_data()?;
    let path =
        crate::asset_discovery::catalog_path(&local.join("papyrus-prefab-cache"), &catalog.sha256)?;
    let (source, captured) =
        prepare_in_with_ordered(root, catalog, &path, &local, capture_ordered, cancelled)?;
    // With other snapshots present, normal discovery still selects the
    // surface and checks the ordered inputs for conflicting captures.
    let can_reuse = captured.ordered.as_ref().is_some_and(|ordered| {
        only_snapshot(&local.join("papyrus-road-layout-cache"), &ordered.sha256)
            && only_snapshot(
                &local.join("papyrus-road-mesh-cache"),
                &captured.surfaces.sha256,
            )
    });
    Ok((source, can_reuse.then_some(captured)))
}

fn only_snapshot(cache: &Path, digest: &str) -> bool {
    let Ok(mut entries) = std::fs::read_dir(cache) else {
        return false;
    };
    matches!(
        entries.next(),
        Some(Ok(entry)) if entry.file_name() == digest
            && entry.file_type().is_ok_and(|kind| kind.is_dir())
            && entries.next().is_none()
    )
}
pub fn prepare_in(
    root: &Path,
    catalog: &Catalog,
    catalog_path: &Path,
    local: &Path,
    cancelled: impl Fn() -> bool,
) -> Result<(Source, road_surface_source::Source), String> {
    let (source, captured) =
        prepare_in_with_ordered(root, catalog, catalog_path, local, false, &cancelled)?;
    Ok((source, captured.surfaces))
}

fn prepare_in_with_ordered(
    root: &Path,
    catalog: &Catalog,
    catalog_path: &Path,
    local: &Path,
    capture_ordered: bool,
    cancelled: &impl Fn() -> bool,
) -> Result<(Source, CapturedInputs), String> {
    if cancelled() {
        return Err("Road preparation cancelled".into());
    }
    let profile = preparation_profile(catalog)?;
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut raw = Vec::new();
    std::fs::File::open(catalog_path)
        .map_err(|e| e.to_string())?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() > 16 * 1024 * 1024 || format!("{:x}", Sha256::digest(&raw)) != catalog.sha256 {
        return Err("Catalog changed before road preparation".into());
    }
    std::fs::create_dir_all(local).map_err(|e| e.to_string())?;
    let temporary = tempfile::Builder::new()
        .prefix(".road-prepare-")
        .tempdir_in(local)
        .map_err(|e| e.to_string())?;
    let profile_path = temporary.path().join("profile.json");
    std::fs::write(&profile_path, profile).map_err(|e| e.to_string())?;
    let output = temporary.path().join("infrastructure");
    let (script, cache_root) = if capture_ordered {
        (Script::RoadConnectors, temporary.path())
    } else {
        (Script::Infrastructure, output.as_path())
    };
    if !metadata_runtime::run(
        script,
        &[
            "--rust-root".into(),
            root.as_os_str().to_owned(),
            "--catalog".into(),
            catalog_path.as_os_str().to_owned(),
            "--profile".into(),
            profile_path.as_os_str().to_owned(),
            "--cache-root".into(),
            cache_root.as_os_str().to_owned(),
        ],
        cancelled,
    )? {
        return Err("Road preparation cancelled".into());
    }
    let document = metadata_runtime::document(&output, "infrastructure.json")?;
    let source = Source::load(&document, catalog, cancelled)?;
    source.verify_installation(root, cancelled)?;
    let surfaces = prepare_surface_in(root, &source, &document, temporary.path(), cancelled)?;
    if cancelled() {
        return Err("Road preparation cancelled".into());
    }
    metadata_runtime::publish(
        &document,
        &local.join("papyrus-infrastructure-cache"),
        &source.sha256,
        16 * 1024 * 1024,
    )?;
    let surface_path = metadata_runtime::document(&temporary.path().join("meshes"), "meshes.json")?;
    metadata_runtime::publish(
        &surface_path,
        &local.join("papyrus-road-mesh-cache"),
        &surfaces.sha256,
        32 * 1024 * 1024,
    )?;
    let ordered = if capture_ordered {
        let context = crate::source_road_cache::Context {
            root,
            catalog,
            infrastructure: &source,
        };
        Some(
            crate::source_road_cache::publish_prepared(
                context,
                local,
                &temporary.path().join("layout"),
                None,
                cancelled,
            )?
            .ok_or("Ordered road preparation cancelled")?,
        )
    } else {
        None
    };
    Ok((source, CapturedInputs { surfaces, ordered }))
}
fn prepare_surface_in(
    root: &Path,
    source: &Source,
    parent_path: &Path,
    temporary: &Path,
    cancelled: impl Fn() -> bool,
) -> Result<road_surface_source::Source, String> {
    let profile = temporary.join("surfaces.json");
    let raw = road_surface_source::extraction_profile(source)?
        .ok_or("No matching road surface adapter for this installation")?;
    std::fs::write(&profile, raw).map_err(|e| e.to_string())?;
    let output = temporary.join("meshes");
    if !metadata_runtime::run(
        Script::RoadMeshes,
        &[
            "--rust-root".into(),
            root.as_os_str().to_owned(),
            "--infrastructure".into(),
            parent_path.as_os_str().to_owned(),
            "--profile".into(),
            profile.as_os_str().to_owned(),
            "--cache-root".into(),
            output.as_os_str().to_owned(),
        ],
        &cancelled,
    )? {
        return Err("Road surface preparation cancelled".into());
    }
    let document = metadata_runtime::document(&output, "meshes.json")?;
    let surfaces = road_surface_source::Source::read(&document, source, &cancelled)?;
    surfaces.verify_installation(root, cancelled)?;
    Ok(surfaces)
}
pub fn prepare_surface(
    root: &Path,
    source: &Source,
    cancelled: impl Fn() -> bool,
) -> Result<Option<road_surface_source::Source>, String> {
    if let Some(surface) = road_surface_source::Source::discover(source, &cancelled)? {
        surface.verify_installation(root, &cancelled)?;
        return Ok(Some(surface));
    }
    if cancelled() {
        return Ok(None);
    }
    let local = crate::storage::local_data()?;
    let temporary = tempfile::Builder::new()
        .prefix(".road-surface-prepare-")
        .tempdir_in(&local)
        .map_err(|e| e.to_string())?;
    let parent = crate::client_cache::pinned_document(
        &local,
        "papyrus-infrastructure-cache",
        &source.sha256,
        "infrastructure.json",
    )?;
    let surface = prepare_surface_in(root, source, &parent, temporary.path(), &cancelled)?;
    if cancelled() {
        return Ok(None);
    }
    let document = metadata_runtime::document(&temporary.path().join("meshes"), "meshes.json")?;
    metadata_runtime::publish(
        &document,
        &local.join("papyrus-road-mesh-cache"),
        &surface.sha256,
        32 * 1024 * 1024,
    )?;
    Ok(Some(surface))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_reuse_requires_one_exact_snapshot() {
        let cache = tempfile::tempdir().unwrap();
        let digest = "a".repeat(64);
        assert!(!only_snapshot(cache.path(), &digest));
        std::fs::create_dir(cache.path().join(&digest)).unwrap();
        assert!(only_snapshot(cache.path(), &digest));
        assert!(!only_snapshot(cache.path(), &"b".repeat(64)));
        std::fs::create_dir(cache.path().join("another-snapshot")).unwrap();
        assert!(!only_snapshot(cache.path(), &digest));
    }
    #[test]
    #[ignore = "requires an owned installation and an isolated metadata runtime/cache"]
    fn owned_complete_preparation_binds_native_routing_and_all_road_consumers() {
        let path = |name| std::path::PathBuf::from(std::env::var_os(name).expect(name));
        let root = path("MAPPER_COMPLETE_ROAD_TEST_ROOT");
        let catalog_path = path("MAPPER_COMPLETE_ROAD_TEST_CATALOG");
        let _leases = crate::source_lease::Scope::default();
        let installation = crate::installation::inspect(&root, || false).unwrap();
        let catalog =
            crate::prefab_assets::load(&root, &catalog_path, &installation, || false).unwrap();
        let local = crate::storage::local_data().unwrap();
        let (source, surfaces) =
            prepare_in(&root, &catalog, &catalog_path, &local, || false).unwrap();
        let reopened = prepare(&root, &catalog, Some(&source.sha256), || false).unwrap();
        assert_eq!(reopened.sha256, source.sha256);
        let ordered = prepare_ordered(&root, &catalog, &source, None, || false)
            .unwrap()
            .unwrap();
        let roadside = crate::roadside_source::prepare(&root, &catalog, || false)
            .unwrap()
            .unwrap();
        let paint = crate::road_paint_source::Profile::for_source(&roadside)
            .unwrap()
            .unwrap();
        let terrain = crate::road_terrain_source::Profile::for_paint(&paint)
            .unwrap()
            .unwrap();
        let recipe = crate::infrastructure::Recipe::for_prepared_source(
            &source,
            surfaces.sha256.clone(),
            roadside.sha256.clone(),
            crate::source_road_planning::Policy::bind(&ordered),
            false,
        )
        .unwrap();
        recipe.validate().unwrap();
        assert_eq!(
            recipe
                .access_costmap
                .as_ref()
                .unwrap()
                .policy
                .sampling
                .as_deref(),
            Some(papyrus_terrain::road_costmap::NATIVE_SAMPLING_VERSION)
        );
        assert_eq!(recipe.ring_costmap, recipe.access_costmap);
        assert!(recipe.include_trails.unwrap_or(true));
        assert!(recipe.staged.as_ref().unwrap().layout.is_some());
        let mut project = crate::Project::load(&path("MAPPER_COMPLETE_ROAD_TEST_PROJECT")).unwrap();
        project.size = 5000;
        project.targets.clear();
        let stages = crate::monument_stages::Source::load(
            &path("MAPPER_COMPLETE_ROAD_TEST_STAGES"),
            &root,
            &catalog,
            || false,
        )
        .unwrap();
        let targets =
            crate::placement_preview::populate_stage_targets(&mut project, &catalog, &stages)
                .unwrap();
        assert!(targets > 6);
        project.roads = Some(recipe.clone());
        project.validate().unwrap();
        project
            .save(&path("MAPPER_COMPLETE_ROAD_TEST_OUTPUT_PROJECT"))
            .unwrap();
        let report = serde_json::json!({
            "source_sha256": source.sha256, "semantic_sha256":source.semantic_sha256().unwrap(),
            "surface_sha256":surfaces.sha256, "ordered_sha256":ordered.assets.sha256,
            "roadside_sha256":roadside.sha256, "paint_sha256":paint.sha256().unwrap(),
            "terrain_sha256":terrain.sha256().unwrap(), "recipe":recipe,
            "pinned_source_reopened":true, "monument_targets":targets, "runtime_validated":false
        });
        std::fs::write(
            path("MAPPER_COMPLETE_ROAD_TEST_REPORT"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
    }
    #[test]
    #[ignore = "requires an owned installation and verified infrastructure/catalog caches"]
    fn owned_ordered_preparation_reopens_source_bound_paved_and_trail_connectors() {
        let path = |name| std::path::PathBuf::from(std::env::var_os(name).expect(name));
        let root = path("MAPPER_ORDERED_TEST_ROOT");
        let _leases = crate::source_lease::Scope::default();
        let installation = crate::installation::inspect(&root, || false).unwrap();
        let catalog = crate::prefab_assets::load(
            &root,
            &path("MAPPER_ORDERED_TEST_CATALOG"),
            &installation,
            || false,
        )
        .unwrap();
        let source = Source::load(&path("MAPPER_ORDERED_TEST_PARENT"), &catalog, || false).unwrap();
        source.verify_installation(&root, || false).unwrap();
        let prepared = prepare_ordered(&root, &catalog, &source, None, || false)
            .unwrap()
            .unwrap();
        let reopened = prepare_ordered(
            &root,
            &catalog,
            &source,
            Some(&prepared.assets.sha256),
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(prepared.assets.sha256, reopened.assets.sha256);
        assert_eq!(
            prepared.roles.reference_sha256(),
            reopened.roles.reference_sha256()
        );
        let count = |kind| {
            prepared
                .assets
                .prefabs()
                .values()
                .flat_map(|prefab| &prefab.connectors)
                .filter(|connector| connector.infrastructure_type == kind)
                .count()
        };
        let paved = count(source.profile.road_type);
        let trails = count(source.profile.trails.as_ref().unwrap().infrastructure_type);
        assert!(paved > 0);
        assert!(trails > 0);
        let report = serde_json::json!({
            "assets_sha256": prepared.assets.sha256,
            "parent_sha256": source.sha256,
            "roles_reference_sha256": prepared.roles.reference_sha256(),
            "prefabs": prepared.assets.prefabs().len(),
            "paved_connectors": paved,
            "trail_connectors": trails,
            "warm_pin_reused": true,
            "runtime_validated": false
        });
        std::fs::write(
            path("MAPPER_ORDERED_TEST_REPORT"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
    }
    #[test]
    #[ignore = "requires an owned installation and verified infrastructure/catalog snapshots"]
    fn owned_surface_preparation_publishes_and_reopens_matching_mesh_buffers() {
        let path = |name| std::path::PathBuf::from(std::env::var_os(name).expect(name));
        let root = path("MAPPER_SURFACE_TEST_ROOT");
        let _leases = crate::source_lease::Scope::default();
        let installation = crate::installation::inspect(&root, || false).unwrap();
        let catalog = crate::prefab_assets::load(
            &root,
            &path("MAPPER_SURFACE_TEST_CATALOG"),
            &installation,
            || false,
        )
        .unwrap();
        let source = Source::load(&path("MAPPER_SURFACE_TEST_PARENT"), &catalog, || false).unwrap();
        source.verify_installation(&root, || false).unwrap();
        let surface = prepare_surface(&root, &source, || false).unwrap().unwrap();
        let reopened = prepare_surface(&root, &source, || false).unwrap().unwrap();
        assert_eq!(surface.sha256, reopened.sha256);
        assert!(!surface.meshes.is_empty());
        let curve = papyrus_terrain::path_geometry::Path::from_controls(
            &[[-100., 12., 0.], [0., 12., 10.], [100., 12., 0.]],
            10000,
            || false,
        )
        .unwrap()
        .unwrap();
        let templates: Vec<_> = surface.meshes.iter().map(|m| &m.buffer).collect();
        let policy = surface.policy();
        let settings = papyrus_terrain::path_mesh::Settings {
            width: 10.,
            chunk_repetitions: policy.chunk_repetitions,
            maximum_work: 32_000_000,
            snap_start: policy.snap_open_endpoints,
            snap_end: policy.snap_open_endpoints,
            mesh_offset: policy.mesh_offset,
            top_aligned: policy.top_aligned,
        };
        let radius = |_| policy.radius(settings.width, 0.);
        let mut deformed_vertices = 0;
        for lod in 0..templates.len() {
            let chunk = papyrus_terrain::path_mesh::deform_surface_chunk(
                &curve,
                &templates,
                lod,
                papyrus_terrain::path_mesh::Surface {
                    settings,
                    snap_to_terrain: policy.snap_to_terrain,
                    radius: &radius,
                },
                0,
                |_, _| Some(12.),
                || false,
            )
            .unwrap()
            .unwrap();
            chunk.mesh.validate().unwrap();
            deformed_vertices += chunk.mesh.vertices.len();
        }
        let report = serde_json::json!({"surface_sha256":surface.sha256,"parent_sha256":source.sha256,"lods":surface.meshes.len(),"vertices":surface.meshes.iter().map(|m|m.buffer.vertices.len()).sum::<usize>(),"deformed_vertices":deformed_vertices,"warm_reused":true,"runtime_validated":false});
        std::fs::write(
            path("MAPPER_SURFACE_TEST_REPORT"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
    }
    #[test]
    fn cancelled_or_mismatched_source_never_creates_an_output_cache() {
        let (catalog, _) = crate::infrastructure::tests::fixture();
        let temporary = tempfile::tempdir().unwrap();
        let output = temporary.path().join("output");
        let missing = temporary.path().join("missing");
        let error = prepare_in(&missing, &catalog, &missing, &output, || true)
            .err()
            .unwrap();
        assert!(error.contains("cancelled"));
        assert!(!output.exists());
        let error = prepare_in(&missing, &catalog, &missing, &output, || false)
            .err()
            .unwrap();
        assert!(error.contains("No matching road preparation adapter"));
        assert!(!output.exists());
    }
}
