//! Numeric, versioned prefab-local water inspection. This does not rasterize water or load meshes.
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "shape",
    rename_all = "lowercase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Geometry {
    Box {
        center: [f64; 3],
        half_axes: [[f64; 3]; 3],
        flat_orthogonal: bool,
    },
    Mesh,
    Sphere,
    Capsule,
    Terrain,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Collider {
    pub object_id: i64,
    pub active: bool,
    pub component_enabled: bool,
    pub hierarchy_active: bool,
    pub hierarchy_active_without_root: bool,
    pub geometry: Geometry,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Body {
    pub object_id: i64,
    pub active: bool,
    pub component_enabled: bool,
    pub hierarchy_active: bool,
    pub hierarchy_active_without_root: bool,
    pub body_type: i32,
    pub is_ocean: bool,
    pub triggers: Vec<Collider>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Contributor {
    pub object_id: i64,
    pub active: bool,
    pub component_enabled: bool,
    pub hierarchy_active: bool,
    pub hierarchy_active_without_root: bool,
    pub body_type: Option<i32>,
    pub add_topology: bool,
    pub collider: Collider,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Issue {
    pub object_id: i64,
    pub reason: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Report {
    pub version: u32,
    pub space: String,
    pub nodes_visited: usize,
    pub water_bodies: Vec<Body>,
    pub contributors: Vec<Contributor>,
    pub issues: Vec<Issue>,
    pub complete: bool,
}
fn valid_type(value: i32) -> bool {
    [1, 2, 4, 8, 16, 32].contains(&value)
}
impl Geometry {
    pub fn flat_box(&self) -> bool {
        matches!(
            self,
            Self::Box {
                flat_orthogonal: true,
                ..
            }
        )
    }
    fn validate(&self) -> Result<(), String> {
        if let Self::Box {
            center,
            half_axes,
            flat_orthogonal,
        } = self
        {
            if center
                .iter()
                .chain(half_axes.iter().flatten())
                .any(|v| !v.is_finite() || v.abs() > 1_000_000.)
            {
                return Err("Invalid water collider coordinates".into());
            }
            let lengths = half_axes.map(|a| a.iter().map(|v| v * v).sum::<f64>().sqrt());
            if lengths.iter().any(|&v| v < 1e-8) {
                return Err("Degenerate water collider".into());
            }
            let [a, b, c] = half_axes;
            let determinant = a[0] * (b[1] * c[2] - b[2] * c[1])
                - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]);
            if determinant.abs() / (lengths[0] * lengths[1] * lengths[2]) < 1e-8 {
                return Err("Singular water collider".into());
            }
            let flat = b[0].abs() + b[2].abs() <= lengths[1] * 1e-6
                && a[1].abs() + c[1].abs() <= lengths[0].max(lengths[2]) * 1e-6
                && a.iter().zip(c).map(|(a, b)| a * b).sum::<f64>().abs()
                    <= lengths[0] * lengths[2] * 1e-6;
            if flat != *flat_orthogonal {
                return Err("Water collider shape contradicts flat-box flag".into());
            }
        }
        Ok(())
    }
}
impl Report {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1
            || self.space != "prefab-root-local-unity"
            || self.nodes_visited > 65536
            || (self.complete && self.nodes_visited == 0)
            || self.complete != self.issues.is_empty()
            || self.water_bodies.len() > 1024
            || self.contributors.len() > 1024
            || self.issues.len() > 256
            || self.water_bodies.len() > self.nodes_visited
            || self.contributors.len() > self.nodes_visited
            || self
                .water_bodies
                .iter()
                .map(|b| b.triggers.len())
                .sum::<usize>()
                > 4096
        {
            return Err("Invalid water geometry report version or limits".into());
        }
        let mut ids = HashSet::new();
        let mut colliders = HashMap::new();
        let mut check = |c: &Collider| -> Result<(), String> {
            if c.object_id == 0 {
                return Err("Invalid water collider identity".into());
            }
            if c.active != (c.component_enabled && c.hierarchy_active)
                || (c.hierarchy_active && !c.hierarchy_active_without_root)
            {
                return Err("Invalid collider activation flags".into());
            }
            c.geometry.validate()?;
            if let Some(old) = colliders.insert(c.object_id, c.clone())
                && old != *c
            {
                return Err("Conflicting water collider records".into());
            }
            Ok(())
        };
        for body in &self.water_bodies {
            if body.object_id == 0 || !ids.insert(body.object_id) || !valid_type(body.body_type) {
                return Err("Invalid or duplicate water body identity/type".into());
            }
            if body.active != (body.component_enabled && body.hierarchy_active)
                || (body.hierarchy_active && !body.hierarchy_active_without_root)
            {
                return Err("Invalid water body activation flags".into());
            }
            let mut triggers = HashSet::new();
            for c in &body.triggers {
                if !triggers.insert(c.object_id) {
                    return Err("Duplicate water trigger".into());
                }
                check(c)?;
            }
        }
        for contributor in &self.contributors {
            if contributor.object_id == 0
                || !ids.insert(contributor.object_id)
                || contributor.body_type.is_some_and(|t| !valid_type(t))
            {
                return Err("Invalid or duplicate water contributor identity/type".into());
            }
            if contributor.active != (contributor.component_enabled && contributor.hierarchy_active)
                || (contributor.hierarchy_active && !contributor.hierarchy_active_without_root)
            {
                return Err("Invalid contributor activation flags".into());
            }
            check(&contributor.collider)?;
        }
        if self.issues.iter().any(|i| {
            i.object_id == 0
                || i.reason.is_empty()
                || i.reason.len() > 1024
                || i.reason.contains('\0')
        }) {
            return Err("Invalid water geometry issue".into());
        }
        Ok(())
    }
    pub fn flat_box_contributors(&self) -> usize {
        self.contributors
            .iter()
            .filter(|c| c.active && c.collider.active && c.collider.geometry.flat_box())
            .count()
    }
}

/// Read-only audit: geometry availability is never a claim of rasterization or game readiness.
pub fn audit(catalog: &crate::prefab_assets::Catalog, details: bool) -> serde_json::Value {
    let mut entries: Vec<_> = catalog.entries.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let mut inspected = 0;
    let mut complete = 0;
    let mut bodies = 0;
    let mut contributors = 0;
    let mut active = 0;
    let mut flat = 0;
    let prefabs:Vec<_>=entries.into_iter().map(|(path,asset)| {
        let report=asset.water_geometry.as_ref();
        if let Some(r)=report {inspected+=1;complete+=usize::from(r.complete);bodies+=r.water_bodies.len();contributors+=r.contributors.len();active+=r.contributors.iter().filter(|c|c.active&&c.collider.active).count();flat+=r.flat_box_contributors();}
        let mut value=serde_json::json!({"prefab":path,"inspected":report.is_some(),"inspection_complete":report.is_some_and(|r|r.complete),"nodes_visited":report.map(|r|r.nodes_visited),"water_bodies":report.map(|r|r.water_bodies.len()),"contributors":report.map(|r|r.contributors.len()),"active_flat_box_contributors":report.map(Report::flat_box_contributors),"issues":report.map(|r|&r.issues)});
        if details {value["water_geometry"]=serde_json::to_value(report).expect("Validated finite geometry serializes");}
        value
    }).collect();
    serde_json::json!({"format":"papyrus-water-geometry-audit","schema":1,"scope":"serialized static hierarchy; runtime-spawned prefab references are not resolved","catalog_sha256":catalog.sha256,"prefab_count":catalog.entries.len(),"inspected_prefabs":inspected,"complete_inspections":complete,"unknown_prefabs":catalog.entries.len()-inspected,"water_bodies":bodies,"contributors":contributors,"active_contributors":active,"active_flat_box_contributors":flat,"active_other_geometry_contributors":active-flat,"activation_scope":"serialized initial states; runtime activation and procedural-object execution are not simulated","rasterized":false,"game_validated":false,"prefabs":prefabs})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn report() -> Report {
        Report {
            version: 1,
            space: "prefab-root-local-unity".into(),
            nodes_visited: 2,
            water_bodies: vec![Body {
                object_id: 10,
                active: true,
                component_enabled: true,
                hierarchy_active: true,
                hierarchy_active_without_root: true,
                body_type: 8,
                is_ocean: false,
                triggers: vec![Collider {
                    object_id: 12,
                    active: true,
                    component_enabled: true,
                    hierarchy_active: true,
                    hierarchy_active_without_root: true,
                    geometry: Geometry::Box {
                        center: [10., 3., 20.],
                        half_axes: [[2., 0., 0.], [0., 1., 0.], [0., 0., 3.]],
                        flat_orthogonal: true,
                    },
                }],
            }],
            contributors: vec![Contributor {
                object_id: 11,
                active: true,
                component_enabled: true,
                hierarchy_active: true,
                hierarchy_active_without_root: true,
                body_type: Some(8),
                add_topology: true,
                collider: Collider {
                    object_id: 12,
                    active: true,
                    component_enabled: true,
                    hierarchy_active: true,
                    hierarchy_active_without_root: true,
                    geometry: Geometry::Box {
                        center: [10., 3., 20.],
                        half_axes: [[2., 0., 0.], [0., 1., 0.], [0., 0., 3.]],
                        flat_orthogonal: true,
                    },
                },
            }],
            issues: vec![],
            complete: true,
        }
    }
    #[test]
    fn typed_report_roundtrips_and_preserves_shared_trigger_geometry() {
        let report = report();
        report.validate().unwrap();
        assert_eq!(report.flat_box_contributors(), 1);
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(
            value["contributors"][0]["collider"]["geometry"]["halfAxes"][2][2],
            3.
        );
        let loaded: Report = serde_json::from_value(value).unwrap();
        assert_eq!(loaded, report);
    }
    #[test]
    fn rejects_mismatched_flags_duplicate_identities_and_incomplete_claims() {
        let mut invalid = report();
        if let Geometry::Box {
            flat_orthogonal, ..
        } = &mut invalid.contributors[0].collider.geometry
        {
            *flat_orthogonal = false;
        }
        assert!(invalid.validate().is_err());
        let mut invalid = report();
        invalid.contributors[0].object_id = 10;
        assert!(invalid.validate().is_err());
        let mut invalid = report();
        invalid.complete = false;
        assert!(invalid.validate().is_err());
        let mut invalid = report();
        invalid.version = 99;
        assert!(invalid.validate().is_err());
        let mut invalid = report();
        invalid.water_bodies[0].triggers[0].active = false;
        assert!(invalid.validate().is_err());
    }
    #[test]
    fn audit_keeps_unknown_geometry_distinct_from_inspected_absence() {
        use crate::{
            installation::{Installation, InstallationKind, SourceFile},
            prefab_assets::{Asset, Catalog},
        };
        let asset = Asset {
            priority: None,
            bounds: None,
            yaw: None,
            modifier_size: None,
            modifier_offset: None,
            texture_guids: Default::default(),
            splat_mask: None,
            modifier: None,
            modifier_absent: false,
            water_geometry: Some(report()),
            no_build_geometry: None,
            display_name: None,
        };
        let mut unknown = asset.clone();
        unknown.water_geometry = None;
        let catalog = Catalog {
            sha256: "a".repeat(64),
            installation: Installation {
                kind: InstallationKind::Client,
                native: SourceFile {
                    relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                    sha256: "b".repeat(64),
                    bytes: 1,
                },
                scene_manifest: None,
            },
            sources: vec![],
            entries: HashMap::from([("known".into(), asset), ("unknown".into(), unknown)]),
        };
        let summary = audit(&catalog, false);
        assert_eq!(summary["inspected_prefabs"], 1);
        assert_eq!(summary["unknown_prefabs"], 1);
        assert_eq!(summary["active_flat_box_contributors"], 1);
        assert_eq!(summary["rasterized"], false);
        assert!(summary["prefabs"][0].get("water_geometry").is_none());
        assert_eq!(
            audit(&catalog, true)["prefabs"][0]["water_geometry"]["contributors"][0]["collider"]["geometry"]
                ["center"][1],
            3.
        );
    }
}
