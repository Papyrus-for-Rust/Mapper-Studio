//! Source-bound static restriction volumes. Footprints are vertical projections,
//! not a claim that every runtime construction test passes outside them.
use serde::{Deserialize, Serialize};
pub const VERSION: u32 = 2;
// The name-only root scan preserves the catalog's prefab metadata exactly.
const PREVIOUS_CATALOG_EXTRACTOR: &str =
    "f8a2b199de0e4614ff3a8518dae0f8d6788b19378c8cc8916ddc6a56fa6bee09";

#[derive(Deserialize)]
struct CacheHeader {
    adapter: String,
    extractor_sha256: String,
    sources: Vec<crate::installation::SourceFile>,
}

impl CacheHeader {
    fn matches(
        &self,
        source: &crate::installation::Installation,
        adapter: &str,
        compatible_extractor: &str,
        native_extractor: Option<&str>,
    ) -> bool {
        self.adapter == adapter
            && (self.extractor_sha256 == compatible_extractor
                || self.extractor_sha256 == PREVIOUS_CATALOG_EXTRACTOR
                || native_extractor == Some(self.extractor_sha256.as_str()))
            && self.sources.contains(&source.native)
            && source
                .scene_manifest
                .as_ref()
                .is_some_and(|manifest| self.sources.contains(manifest))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Report {
    pub version: u32,
    pub space: String,
    pub layer_index: u8,
    pub nodes_visited: usize,
    pub volumes: Vec<Volume>,
    pub issues: Vec<Issue>,
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub should_display_on_map: Option<bool>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Volume {
    pub object_id: i64,
    pub active: bool,
    pub geometry: Geometry,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Issue {
    pub object_id: i64,
    pub reason: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "shape", rename_all = "snake_case", deny_unknown_fields)]
pub enum Geometry {
    Box {
        center: [f32; 3],
        #[serde(rename = "halfAxes")]
        half_axes: [[f32; 3]; 3],
        #[serde(rename = "flatOrthogonal")]
        flat_orthogonal: bool,
    },
    Sphere {
        center: [f32; 3],
        radius: f32,
    },
    Capsule {
        start: [f32; 3],
        end: [f32; 3],
        radius: f32,
    },
    Mesh {
        convex: bool,
        vertices: Vec<[f32; 3]>,
        triangles: Vec<[u32; 3]>,
    },
}
impl Report {
    pub fn validate(&self) -> Result<(), String> {
        let point = |p: &[f32; 3]| p.iter().all(|v| v.is_finite() && v.abs() <= 100000.);
        let radius = |v: f32| v.is_finite() && v > 0. && v <= 20000.;
        if ![1, VERSION].contains(&self.version)
            || self.space != "prefab-root-local-unity"
            || self.layer_index >= 32
            || self.nodes_visited > 65536
            || self.volumes.len() > 1024
            || self.issues.len() > 256
            || self.complete && !self.issues.is_empty()
            || self.issues.iter().any(|i| i.reason.len() > 1024)
        {
            return Err("Unsupported no-build geometry report".into());
        }
        let mut ids = std::collections::HashSet::new();
        for v in &self.volumes {
            if !ids.insert(v.object_id) {
                return Err("Duplicate restriction collider".into());
            }
            let valid = match &v.geometry {
                Geometry::Box {
                    center, half_axes, ..
                } => {
                    point(center)
                        && half_axes
                            .iter()
                            .all(|a| point(a) && a.iter().any(|v| v.abs() > 1e-8))
                }
                Geometry::Sphere { center, radius: r } => point(center) && radius(*r),
                Geometry::Capsule {
                    start,
                    end,
                    radius: r,
                } => point(start) && point(end) && radius(*r),
                Geometry::Mesh {
                    vertices,
                    triangles,
                    ..
                } => {
                    self.version >= 2
                        && (3..=65536).contains(&vertices.len())
                        && (1..=131072).contains(&triangles.len())
                        && vertices.iter().all(point)
                        && triangles
                            .iter()
                            .flatten()
                            .all(|i| (*i as usize) < vertices.len())
                }
            };
            if !valid {
                return Err("Invalid restriction geometry".into());
            }
        }
        Ok(())
    }
}
impl Geometry {
    /// Polygon of the top-down footprint in prefab-local Unity X/Z. Curved
    /// edges use 64 samples; the source radius is never a monument bound guess.
    pub fn footprints(&self) -> Vec<Vec<[f32; 2]>> {
        if let Self::Mesh {
            convex,
            vertices,
            triangles,
        } = self
        {
            let points: Vec<_> = vertices.iter().map(|p| [p[0], p[2]]).collect();
            if *convex {
                return vec![hull(points)];
            }
            let mut polygons: Vec<_> = triangles
                .iter()
                .map(|t| hull(t.map(|i| points[i as usize]).to_vec()))
                .filter(|p| p.len() >= 2)
                .collect();
            polygons.sort_by(|a, b| {
                a.iter()
                    .flatten()
                    .zip(b.iter().flatten())
                    .find_map(|(a, b)| {
                        let c = a.total_cmp(b);
                        (c != std::cmp::Ordering::Equal).then_some(c)
                    })
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.len().cmp(&b.len()))
            });
            polygons.dedup();
            return polygons;
        }
        let mut points = Vec::new();
        match self {
            Self::Box {
                center, half_axes, ..
            } => {
                for x in [-1., 1.] {
                    for y in [-1., 1.] {
                        for z in [-1., 1.] {
                            points.push([0, 2].map(|i| {
                                center[i]
                                    + half_axes[0][i] * x
                                    + half_axes[1][i] * y
                                    + half_axes[2][i] * z
                            }));
                        }
                    }
                }
            }
            Self::Sphere { center, radius } => circle(&mut points, *center, *radius),
            Self::Capsule { start, end, radius } => {
                circle(&mut points, *start, *radius);
                circle(&mut points, *end, *radius);
            }
            Self::Mesh { .. } => unreachable!(),
        }
        vec![hull(points)]
    }
}
fn circle(out: &mut Vec<[f32; 2]>, center: [f32; 3], radius: f32) {
    for i in 0..64 {
        let a = i as f32 * std::f32::consts::TAU / 64.;
        out.push([center[0] + a.cos() * radius, center[2] + a.sin() * radius]);
    }
}
fn hull(mut points: Vec<[f32; 2]>) -> Vec<[f32; 2]> {
    points.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    points.dedup();
    if points.len() < 3 {
        return points;
    }
    let cross = |a: [f32; 2], b: [f32; 2], c: [f32; 2]| {
        (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
    };
    let mut lower = Vec::new();
    let mut upper = Vec::new();
    for p in &points {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], *p) <= 0. {
            lower.pop();
        }
        lower.push(*p);
    }
    for p in points.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], *p) <= 0. {
            upper.pop();
        }
        upper.push(*p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

/// Independently enrich view metadata without replacing saved generation pins
/// or re-extracting textures. Existing catalogs remain valid for their recipes.
pub fn prepare(
    root: &std::path::Path,
    source: &crate::installation::Installation,
    cancelled: impl Fn() -> bool,
) -> Result<crate::prefab_assets::Catalog, String> {
    prepare_catalog(root, source, true, cancelled)
}

// Keep every parser dependency read-leased through capture and publication.
// The child can reuse these receipts for both of its source snapshots.
fn lease_catalog_sources(
    root: &std::path::Path,
    source: &crate::installation::Installation,
    restrictions: bool,
    cancelled: &impl Fn() -> bool,
) -> Result<(), String> {
    use crate::installation::hash_file;
    source.validate()?;
    let manifest = source
        .scene_manifest
        .as_ref()
        .ok_or("Missing source manifest")?;
    if hash_file(
        root,
        &source.native.relative_path,
        64 * 1024 * 1024,
        cancelled,
    )? != source.native
        || hash_file(root, &manifest.relative_path, 32 * 1024 * 1024, cancelled)? != *manifest
    {
        return Err("Selected installation changed before catalog preparation".into());
    }
    hash_file(
        root,
        "Bundles/shared/assetscenes.bundle",
        2 * 1024 * 1024 * 1024,
        cancelled,
    )?;
    let data = source
        .native
        .relative_path
        .split('/')
        .next()
        .ok_or("Missing data directory")?;
    let manager = format!("{data}/globalgamemanagers");
    if restrictions && root.join(&manager).is_file() {
        hash_file(root, &manager, 32 * 1024 * 1024, cancelled)?;
        for name in ["content", "monuments"] {
            let relative = format!("Bundles/shared/{name}.bundle");
            if root.join(&relative).is_file() {
                hash_file(root, &relative, 8 * 1024 * 1024 * 1024, cancelled)?;
            }
        }
    }
    Ok(())
}
/// View-only metadata enrichment; leaves generation catalogs and pins intact.
pub fn prepare_catalog(
    root: &std::path::Path,
    source: &crate::installation::Installation,
    restrictions: bool,
    cancelled: impl Fn() -> bool,
) -> Result<crate::prefab_assets::Catalog, String> {
    use crate::{
        metadata_runtime::{self, Script},
        prefab_assets,
    };
    if cancelled() {
        return Err("Monument metadata loading cancelled".into());
    }
    source.validate()?;
    if restrictions {
        let data = source
            .native
            .relative_path
            .split('/')
            .next()
            .ok_or("Missing data directory")?;
        let manager = root.join(data).join("globalgamemanagers");
        if !manager.is_file() {
            return Err(format!(
                "No-build zones require {}. Select a complete Rust installation.",
                manager.display()
            ));
        }
    }
    let _verification = crate::installation::VerificationScope::default();
    let local = crate::storage::local_data()?;
    let cache = local.join("papyrus-prefab-cache");
    let native_extractor = (!restrictions).then(crate::native_catalog::fingerprint);
    let compatible_extractor = metadata_runtime::catalog_extractor_sha256();
    // Names use the smaller catalog without the restriction mesh receipts.
    let adapter = if restrictions {
        prefab_assets::BUILDING_ADAPTER
    } else {
        prefab_assets::DISCOVERY_ADAPTER
    };
    if cache.is_dir() {
        for entry in std::fs::read_dir(&cache)
            .map_err(|e| e.to_string())?
            .take(256)
        {
            if cancelled() {
                return Err("Monument metadata loading cancelled".into());
            }
            let path = entry
                .map_err(|e| e.to_string())?
                .path()
                .join("catalog.json");
            use std::io::Read;
            let mut bytes = Vec::new();
            let Ok(file) = std::fs::File::open(&path) else {
                continue;
            };
            if file
                .take(16 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .is_err()
                || bytes.len() > 16 * 1024 * 1024
            {
                continue;
            }
            let Ok(document) = serde_json::from_slice::<CacheHeader>(&bytes) else {
                continue;
            };
            if restrictions && document.extractor_sha256 != compatible_extractor {
                continue;
            }
            if !document.matches(
                source,
                adapter,
                &compatible_extractor,
                native_extractor.as_deref(),
            ) {
                continue;
            }
            if let Ok(catalog) = prefab_assets::load_document(root, &bytes, source, &cancelled)
                && (!restrictions
                    || catalog
                        .entries
                        .values()
                        .any(|a| a.no_build_geometry.is_some()))
            {
                return Ok(catalog);
            }
        }
    }
    lease_catalog_sources(root, source, restrictions, &cancelled)?;
    std::fs::create_dir_all(&local).map_err(|e| e.to_string())?;
    let temporary = tempfile::Builder::new()
        .prefix(".restriction-prepare-")
        .tempdir_in(&local)
        .map_err(|e| e.to_string())?;
    let mut arguments = vec![
        "--rust-root".into(),
        root.as_os_str().to_owned(),
        "--cache-root".into(),
        temporary.path().as_os_str().to_owned(),
    ];
    let script = if restrictions {
        arguments.push("--building-restrictions".into());
        Script::RestrictionCatalog
    } else {
        Script::Catalog
    };
    if !metadata_runtime::run(script, &arguments, &cancelled)? {
        return Err("Monument metadata loading cancelled".into());
    }
    let path = metadata_runtime::document(temporary.path(), "catalog.json")?;
    let catalog = prefab_assets::load(root, &path, source, &cancelled)?;
    if cancelled() {
        return Err("Monument metadata loading cancelled".into());
    }
    metadata_runtime::publish(&path, &cache, &catalog.sha256, 16 * 1024 * 1024)?;
    Ok(catalog)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn previous_root_scan_catalog_still_requires_the_matching_installation() {
        use crate::installation::{Installation, InstallationKind, SourceFile};
        let file = |relative_path: &str, digit: &str| SourceFile {
            relative_path: relative_path.into(),
            sha256: digit.repeat(64),
            bytes: 1,
        };
        let mut source = Installation {
            kind: InstallationKind::Client,
            native: file("RustClient_Data/Plugins/x86_64/RustNative.dll", "a"),
            scene_manifest: Some(file("Bundles/AssetSceneManifest.json", "b")),
        };
        let adapter = crate::prefab_assets::BUILDING_ADAPTER;
        let header = CacheHeader {
            adapter: adapter.into(),
            extractor_sha256: PREVIOUS_CATALOG_EXTRACTOR.into(),
            sources: vec![source.native.clone(), source.scene_manifest.clone().unwrap()],
        };
        let current_extractor = crate::metadata_runtime::catalog_extractor_sha256();
        assert_ne!(current_extractor, PREVIOUS_CATALOG_EXTRACTOR);
        assert!(header.matches(&source, adapter, &current_extractor, None));
        source.scene_manifest.as_mut().unwrap().sha256 = "c".repeat(64);
        assert!(!header.matches(&source, adapter, &current_extractor, None));
    }

    #[test]
    fn incomplete_restriction_installations_fail_before_cache_access_or_extraction() {
        use crate::installation::{Installation, InstallationKind, SourceFile};
        let root = tempfile::tempdir().unwrap();
        let source = Installation {
            kind: InstallationKind::Client,
            native: SourceFile {
                relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                sha256: "a".repeat(64),
                bytes: 1,
            },
            scene_manifest: Some(SourceFile {
                relative_path: "Bundles/AssetSceneManifest.json".into(),
                sha256: "b".repeat(64),
                bytes: 1,
            }),
        };
        let error = prepare_catalog(root.path(), &source, true, || false)
            .err()
            .unwrap();
        assert!(error.contains("globalgamemanagers"));
        assert!(error.contains("complete Rust installation"));
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        assert_eq!(
            prepare_catalog(root.path(), &source, true, || true)
                .err()
                .unwrap(),
            "Monument metadata loading cancelled"
        );
    }
    #[test]
    fn catalog_capture_leases_all_dependencies_and_rejects_changed_installations() {
        use crate::installation::{self, Installation, InstallationKind};
        let root = tempfile::tempdir().unwrap();
        let native = "RustClient_Data/Plugins/x86_64/RustNative.dll";
        let manifest = "Bundles/AssetSceneManifest.json";
        let paths = [
            native,
            manifest,
            "Bundles/shared/assetscenes.bundle",
            "RustClient_Data/globalgamemanagers",
            "Bundles/shared/content.bundle",
            "Bundles/shared/monuments.bundle",
        ];
        for path in paths {
            let full = root.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, path.as_bytes()).unwrap();
        }
        let source = Installation {
            kind: InstallationKind::Client,
            native: installation::hash_file(root.path(), native, 1024, &|| false).unwrap(),
            scene_manifest: Some(
                installation::hash_file(root.path(), manifest, 1024, &|| false).unwrap(),
            ),
        };
        {
            let _verification = installation::VerificationScope::default();
            lease_catalog_sources(root.path(), &source, true, &|| false).unwrap();
            #[cfg(windows)]
            {
                assert_eq!(crate::source_lease::receipts().len(), 6);
                for path in paths {
                    assert!(
                        std::fs::OpenOptions::new()
                            .write(true)
                            .open(root.path().join(path))
                            .is_err()
                    );
                }
            }
        }
        {
            let _verification = installation::VerificationScope::default();
            lease_catalog_sources(root.path(), &source, false, &|| false).unwrap();
            #[cfg(windows)]
            assert_eq!(crate::source_lease::receipts().len(), 3);
        }
        std::fs::write(root.path().join(native), b"updated native").unwrap();
        assert!(lease_catalog_sources(root.path(), &source, true, &|| false).is_err());
        assert!(lease_catalog_sources(root.path(), &source, true, &|| true).is_err());
    }
    #[test]
    fn concave_mesh_projection_does_not_fill_its_convex_envelope() {
        let vertices = vec![
            [0., 0., 0.],
            [3., 0., 0.],
            [3., 0., 1.],
            [1., 0., 1.],
            [1., 0., 3.],
            [0., 0., 3.],
        ];
        let triangles = vec![[0, 1, 2], [0, 2, 3], [0, 3, 5], [3, 4, 5]];
        let mesh = Geometry::Mesh {
            convex: false,
            vertices: vertices.clone(),
            triangles: triangles.clone(),
        };
        let polygons = mesh.footprints();
        assert_eq!(polygons.len(), 4);
        let inside = |p: &Vec<[f32; 2]>| {
            p.iter().enumerate().all(|(i, a)| {
                let b = p[(i + 1) % p.len()];
                (b[0] - a[0]) * (2. - a[1]) - (b[1] - a[1]) * (2. - a[0]) >= 0.
            })
        };
        assert!(!polygons.iter().any(inside));
        let convex = Geometry::Mesh {
            convex: true,
            vertices,
            triangles,
        }
        .footprints();
        assert_eq!(convex.len(), 1);
        assert!(inside(&convex[0]));
    }
    #[test]
    #[ignore = "Requires an owned, source-bound restriction catalog and Rust folder"]
    fn owned_catalog_receipts_and_all_restriction_shapes_validate() {
        let root = std::path::PathBuf::from(
            std::env::var_os("MAPPER_NO_BUILD_ROOT").expect("Rust folder"),
        );
        let path = std::path::PathBuf::from(
            std::env::var_os("MAPPER_NO_BUILD_CATALOG").expect("Owned catalog"),
        );
        let _verification = crate::installation::VerificationScope::default();
        let source = crate::installation::inspect(&root, || false).unwrap();
        let catalog = crate::prefab_assets::load(&root, &path, &source, || false).unwrap();
        let mut count = 0;
        let mut meshes = 0;
        for report in catalog
            .entries
            .values()
            .filter_map(|a| a.no_build_geometry.as_ref())
        {
            report.validate().unwrap();
            for volume in &report.volumes {
                count += 1;
                meshes += usize::from(matches!(volume.geometry, Geometry::Mesh { .. }));
                for points in volume.geometry.footprints() {
                    assert!(points.iter().flatten().all(|v| v.is_finite()));
                }
            }
        }
        assert!(count > 0);
        eprintln!("Owned restriction catalog: {count} colliders, {meshes} mesh colliders");
    }
    #[test]
    fn projected_capsules_and_boxes_preserve_local_offsets_and_height_extent() {
        let c = Geometry::Capsule {
            start: [-10., 50., 5.],
            end: [10., -50., 5.],
            radius: 2.,
        }
        .footprints()
        .remove(0);
        let bounds = |p: &[[f32; 2]]| {
            [
                p.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min),
                p.iter().map(|p| p[0]).fold(f32::NEG_INFINITY, f32::max),
                p.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min),
                p.iter().map(|p| p[1]).fold(f32::NEG_INFINITY, f32::max),
            ]
        };
        assert_eq!(bounds(&c), [-12., 12., 3., 7.]);
        let b = Geometry::Box {
            center: [20., 7., 30.],
            half_axes: [[2., 0., 0.], [0., 3., 0.], [0., 0., 4.]],
            flat_orthogonal: true,
        }
        .footprints()
        .remove(0);
        assert_eq!(bounds(&b), [18., 22., 26., 34.]);
        assert_eq!(b.len(), 4);
        let vertical = Geometry::Capsule {
            start: [5., -50., 10.],
            end: [5., 50., 10.],
            radius: 3.,
        }
        .footprints()
        .remove(0);
        assert_eq!(bounds(&vertical), [2., 8., 7., 13.]);
    }
    #[test]
    fn unsupported_or_nonfinite_reports_never_look_like_verified_empty_zones() {
        let mut r = Report {
            version: 1,
            space: "prefab-root-local-unity".into(),
            layer_index: 29,
            nodes_visited: 1,
            volumes: vec![],
            issues: vec![Issue {
                object_id: 1,
                reason: "unsupported mesh".into(),
            }],
            complete: true,
            should_display_on_map: None,
        };
        assert!(r.validate().is_err());
        r.complete = false;
        assert!(r.validate().is_ok());
        r.volumes.push(Volume {
            object_id: 2,
            active: true,
            geometry: Geometry::Sphere {
                center: [0.; 3],
                radius: f32::NAN,
            },
        });
        assert!(r.validate().is_err());
    }
}
