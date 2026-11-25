//! Selected-client enum roles and yaw-only, unit-scale Studio pose adaptation.
//! This prepares raw layout inputs, not placement or terrain/mesh acceptance.
use crate::{Project, infrastructure, installation::SourceFile, source_road_assets::Assets};
use papyrus_terrain::{placement::Solution, road_costmap::CellLattice, source_road_layout::Connector};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
};
pub const VERSION: &str = "papyrus-source-road-inputs-1";
const ENUM_PROFILES: [&[u8]; 3] = [
    include_bytes!("../../../compatibility/infrastructure/client-e7156192-road-enums.json"),
    include_bytes!("../../../compatibility/infrastructure/client-199a470f-road-enums.json"),
    include_bytes!("../../../compatibility/infrastructure/client-516653f6-road-enums.json"),
];
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
#[derive(Debug, Serialize)]
pub struct Roles {
    source_sha256: String,
    reference_sha256: String,
    roadside: i32,
    paved: i32,
    trail: i32,
    #[serde(skip_serializing)]
    power: Option<i32>,
}
impl Roles {
    pub fn reference_sha256(&self) -> &str {
        &self.reference_sha256
    }
    pub fn power(&self) -> Option<i32> { self.power }
    /// Known code semantics are selected by exact gameplay receipts, never a
    /// RustNative version name. Unknown code is not assigned guessed enum roles.
    pub fn for_source(source: &infrastructure::Source) -> Result<Option<Self>, String> {
        let Some(trails) = source.profile.trails.as_ref() else {
            return Ok(None);
        };
        Self::for_receipts(
            &source.sha256,
            source.gameplay_receipts(),
            source.profile.road_type,
            trails.infrastructure_type,
        )
    }
    fn for_receipts(
        source: &str,
        receipts: &[SourceFile],
        paved: i32,
        trail: i32,
    ) -> Result<Option<Self>, String> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Profile {
            format: String,
            schema: u32,
            id: String,
            metadata_version: u32,
            sources: Vec<SourceFile>,
            reference_sha256: String,
            roadside: i32,
            paved: i32,
            trail: i32,
            power: Option<i32>,
        }
        for raw in ENUM_PROFILES {
            let profile: Profile = serde_json::from_slice(raw).map_err(|e| e.to_string())?;
            if profile.format != "papyrus-road-enum-profile"
                || profile.schema != 1
                || profile.id.is_empty()
                || profile.metadata_version == 0
                || profile.sources.len() != 2
                || !crate::source_road_assets::valid_digest(&profile.reference_sha256)
                || !(0..=1024).contains(&profile.roadside)
                || !(0..=64).contains(&profile.paved)
                || !(0..=64).contains(&profile.trail)
                || profile.paved == profile.trail
                || profile.power.is_some_and(|power| !(0..=64).contains(&power)
                    || power == profile.paved || power == profile.trail)
            {
                return Err("Invalid bundled selected-code road enum policy".into());
            }
            if profile.sources != receipts {
                continue;
            }
            if !crate::source_road_assets::valid_digest(source)
                || profile.paved != paved
                || profile.trail != trail
            {
                return Err(
                    "Selected road enum policy contradicts the infrastructure snapshot".into(),
                );
            }
            return Ok(Some(Self {
                source_sha256: source.into(),
                reference_sha256: profile.reference_sha256,
                roadside: profile.roadside,
                paved,
                trail,
                power: profile.power,
            }));
        }
        Ok(None)
    }
    fn parse(
        bytes: &[u8],
        source: &str,
        expected: &[SourceFile],
        paved: i32,
        trail: i32,
    ) -> Result<Self, String> {
        let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        let receipts: Vec<SourceFile> =
            serde_json::from_value(value["sources"].clone()).map_err(|e| e.to_string())?;
        if value["format"] != "papyrus-client-road-code-reference"
            || value["schema"] != 1
            || expected.len() != 2
            || receipts != expected
            || expected[0].relative_path != "GameAssembly.dll"
            || expected[1].relative_path
                != "RustClient_Data/il2cpp_data/Metadata/global-metadata.dat"
        {
            return Err(
                "Client road enum reference differs from selected gameplay receipts".into(),
            );
        }
        let role = |kind: &str, name: &str, maximum: i64| {
            value["integer_fields"][kind][name]
                .as_i64()
                .filter(|v| (0..=maximum).contains(v))
                .map(|v| v as i32)
                .ok_or(format!(
                    "Missing bounded selected-client enum {kind}.{name}"
                ))
        };
        let roadside = role("MonumentType", "Roadside", 1024)?;
        if role("InfrastructureType", "Road", 64)? != paved
            || role("InfrastructureType", "Trail", 64)? != trail
            || paved == trail
        {
            return Err("Client road/trail roles differ from the selected scene".into());
        }
        Ok(Self {
            source_sha256: source.into(),
            reference_sha256: hash(bytes),
            roadside,
            paved,
            trail,
            power: value["integer_fields"]["InfrastructureType"]["Power"].as_i64()
                .filter(|value| (0..=64).contains(value)).map(|value| value as i32),
        })
    }
    /// Only enum interpretation is authorized by this reference. In particular,
    /// missing client literal metadata cannot authorize a search-depth policy.
    pub fn load(
        path: &Path,
        source: &infrastructure::Source,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        if cancelled() {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(2 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 2 * 1024 * 1024 {
            return Err("Client road role reference exceeds 2 MiB".into());
        }
        let roles = Self::parse(
            &bytes,
            &source.sha256,
            source.gameplay_receipts(),
            source.profile.road_type,
            source
                .profile
                .trails
                .as_ref()
                .ok_or("Missing selected trail scene policy")?
                .infrastructure_type,
        )?;
        Ok((!cancelled()).then_some(roles))
    }
}
#[derive(Debug, PartialEq, Serialize)]
pub struct Endpoint {
    pub monument: u64,
    pub connector: i64,
    pub prefab: String,
    pub world: [f32; 3],
    pub hierarchy_active_without_root: bool,
}
#[derive(Debug, Serialize)]
pub struct Prepared {
    pub engine: &'static str,
    pub assets_sha256: String,
    pub roles_sha256: String,
    pub source_sha256: String,
    pub lattice: CellLattice,
    pub trail: bool,
    pub connectors: Vec<Connector>,
    pub endpoints: Vec<Endpoint>,
    pub skipped_roadside: Vec<u64>,
    pub skipped_nonmonuments: Vec<u64>,
}
pub fn prepare(
    project: &Project,
    solution: &Solution,
    assets: &Assets,
    roles: &Roles,
    lattice: CellLattice,
    trail: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Prepared>, String> {
    if cancelled() {
        return Ok(None);
    }
    if project.size as f32 != lattice.size()
        || project
            .roads
            .as_ref()
            .is_none_or(|r| r.source_sha256 != assets.infrastructure_sha256())
        || project
            .monument_assets
            .as_ref()
            .is_none_or(|a| a.catalog_sha256 != assets.catalog_sha256())
        || roles.source_sha256 != assets.infrastructure_sha256()
        || project.targets.len() > 128
        || solution.outcomes.len() != project.targets.len()
        || (trail
            && project
                .roads
                .as_ref()
                .is_some_and(|r| r.include_trails == Some(false)))
    {
        return Err(
            "Source road inputs differ from project, selected assets, family or lattice".into(),
        );
    }
    let targets: BTreeMap<_, _> = project.targets.iter().map(|t| (t.instance_id, t)).collect();
    if targets.len() != project.targets.len() {
        return Err("Duplicate source road project target".into());
    }
    let mut seen = BTreeSet::new();
    let mut prepared = Prepared {
        engine: VERSION,
        assets_sha256: assets.sha256.clone(),
        roles_sha256: roles.reference_sha256.clone(),
        source_sha256: roles.source_sha256.clone(),
        lattice,
        trail,
        connectors: Vec::new(),
        endpoints: Vec::new(),
        skipped_roadside: Vec::new(),
        skipped_nonmonuments: Vec::new(),
    };
    let family = if trail { roles.trail } else { roles.paved };
    for (ordinal, outcome) in solution.outcomes.iter().enumerate() {
        if cancelled() {
            return Ok(None);
        }
        if !seen.insert(outcome.id) {
            return Err("Duplicate source road placement outcome".into());
        }
        let target = targets
            .get(&outcome.id)
            .ok_or("Unknown source road placement outcome")?;
        let Some(pose) = &outcome.position else {
            continue;
        };
        if [pose.x, pose.y, pose.z]
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 10000.)
            || !pose.yaw.is_finite()
            || pose.yaw.abs() > 36000.
        {
            return Err("Invalid source road instance pose".into());
        }
        let path = target
            .prefab_path
            .as_deref()
            .ok_or("Source road target has no prefab identity")?;
        let prefab = assets.prefab(path)?;
        let Some(monument) = &prefab.monument else {
            prepared.skipped_nonmonuments.push(outcome.id);
            continue;
        };
        if monument.monument_type == roles.roadside {
            prepared.skipped_roadside.push(outcome.id);
            continue;
        }
        let sockets: Vec<_> = prefab
            .connectors
            .iter()
            .filter(|s| s.infrastructure_type == family)
            .collect();
        if sockets.is_empty() {
            continue;
        }
        // Source uses the local Bounds extents, independently of its centre or
        // instance yaw, with reciprocal multiplication before the grid scale.
        let inverse = 1. / lattice.size();
        let radius = ((monument.bounds_extent[0] * inverse)
            .max(monument.bounds_extent[2] * inverse)
            * lattice.resolution() as f32)
            .ceil() as usize;
        if !(1..=1024).contains(&radius) {
            return Err("Source monument has unsupported road push radius".into());
        }
        let centre = lattice.cell([pose.x, pose.z])?;
        let (s, c) = pose.yaw.to_radians().sin_cos();
        for socket in sockets {
            if cancelled() {
                return Ok(None);
            }
            if prepared.connectors.len() == 1024 {
                return Err("Source road connector allowance exhausted".into());
            }
            let [x, y, z] = socket.position;
            // Studio's +X -> +Z yaw is negated at the Unity export boundary.
            let world = [pose.x + c * x - s * z, pose.y + y, pose.z + s * x + c * z];
            if world.iter().any(|v| !v.is_finite() || v.abs() > 10000.) {
                return Err("Source road socket leaves supported world bounds".into());
            }
            prepared.connectors.push(Connector {
                monument: ordinal as u32,
                cell: lattice.cell([world[0], world[2]])?,
                centre,
                radius,
                roadside: false,
            });
            prepared.endpoints.push(Endpoint {
                monument: outcome.id,
                connector: socket.object_id,
                prefab: path.into(),
                world,
                hierarchy_active_without_root: socket.hierarchy_active_without_root,
            });
        }
    }
    Ok((!cancelled()).then_some(prepared))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    #[test]
    fn bundled_roles_are_bound_to_both_code_files_and_scene_roles() {
        for raw in ENUM_PROFILES {
            let value: Value = serde_json::from_slice(raw).unwrap();
            let receipts: Vec<SourceFile> =
                serde_json::from_value(value["sources"].clone()).unwrap();
            let source = "a".repeat(64);
            let roles = Roles::for_receipts(&source, &receipts, 0, 2)
                .unwrap()
                .unwrap();
            assert_eq!((roles.roadside, roles.paved, roles.trail), (7, 0, 2));
            assert_eq!(
                roles.reference_sha256,
                value["reference_sha256"].as_str().unwrap()
            );
            for index in 0..2 {
                let mut changed = receipts.clone();
                changed[index].sha256 = "f".repeat(64);
                assert!(
                    Roles::for_receipts(&source, &changed, 0, 2)
                        .unwrap()
                        .is_none()
                );
                let mut changed = receipts.clone();
                changed[index].bytes += 1;
                assert!(
                    Roles::for_receipts(&source, &changed, 0, 2)
                        .unwrap()
                        .is_none()
                );
            }
            let mut reordered = receipts.clone();
            reordered.reverse();
            assert!(
                Roles::for_receipts(&source, &reordered, 0, 2)
                    .unwrap()
                    .is_none()
            );
            assert!(Roles::for_receipts(&source, &receipts, 1, 2).is_err());
            assert!(Roles::for_receipts("invalid", &receipts, 0, 2).is_err());
        }
    }
    fn receipts() -> Vec<SourceFile> {
        [
            "GameAssembly.dll",
            "RustClient_Data/il2cpp_data/Metadata/global-metadata.dat",
        ]
        .iter()
        .map(|p| SourceFile {
            relative_path: p.to_string(),
            sha256: "a".repeat(64),
            bytes: 100,
        })
        .collect()
    }
    fn role_document() -> Value {
        json!({"format":"papyrus-client-road-code-reference","schema":1,"sources":receipts(),
            "integer_fields":{"MonumentType":{"Roadside":99},"InfrastructureType":{"Road":7,"Trail":9}},
            "absent_literal_metadata":["GenerateRoadRing","GenerateRoadLayout"]})
    }
    fn fixture(kind: Option<i32>) -> (Project, Solution, Assets, Roles) {
        let path = "assets/bundled/prefabs/test.prefab";
        let mut project = Project {
            size: 1000,
            ..Project::default()
        };
        project.roads = Some(infrastructure::Recipe::new("b".repeat(64)));
        project.monument_assets = Some(crate::AssetIdentity {
            catalog_sha256: "c".repeat(64),
            bindings_sha256: "d".repeat(64),
        });
        let id = u64::MAX - 1;
        project.targets = vec![crate::MonumentTarget {
            instance_id: id,
            name: "Test".into(),
            prefab_path: Some(path.into()),
            x: 0.,
            z: 0.,
            radius: 100.,
            max_displacement_m: None,
            strength: 1.,
            exact: false,
            yaw: Some(0.),
            presence: crate::Presence::Required,
        }];
        let solution = Solution {
            outcomes: vec![papyrus_terrain::placement::Outcome {
                id,
                position: Some(papyrus_terrain::placement::Position {
                    x: 100.,
                    y: 20.,
                    z: 200.,
                    yaw: 37.5,
                    clearance: 100.,
                }),
                distance: None,
                within_radius: true,
                reason: String::new(),
            }],
            required_missing: 0,
            attempts: 1,
        };
        let mut assets:Assets=serde_json::from_value(json!({"format":"papyrus-road-layout-assets","schema":1,
            "adapter":crate::source_road_assets::ADAPTER,"catalog_sha256":"c".repeat(64),"infrastructure_sha256":"b".repeat(64),
            "sources":receipts(),"extractor_sha256":"e".repeat(64),"prefabs":{path:{"version":1,"space":"prefab-root-local-unity",
                "connectorOrder":"root-first-depth-first-component-order","complete":true,"issues":[],"nodesVisited":2,
                "monument":kind.map(|v|json!({"objectId":99,"monumentType":v,"boundsCenter":[300.,0.,-200.],"boundsExtent":[40.,20.,60.]})),
                "connectors":[{"objectId":1000,"infrastructureType":7,"position":[10.,2.,20.],"forward":[0.,0.,1.],"hierarchyActiveWithoutRoot":true},
                    {"objectId":80,"infrastructureType":7,"position":[-10.,3.,-20.],"forward":[0.,0.,1.],"hierarchyActiveWithoutRoot":false},
                    {"objectId":81,"infrastructureType":9,"position":[20.,4.,10.],"forward":[0.,0.,1.],"hierarchyActiveWithoutRoot":false}]}}})).unwrap();
        assets.sha256 = "f".repeat(64);
        let roles = Roles::parse(
            &serde_json::to_vec(&role_document()).unwrap(),
            &"b".repeat(64),
            &receipts(),
            7,
            9,
        )
        .unwrap();
        (project, solution, assets, roles)
    }
    #[test]
    fn arbitrary_selected_roles_and_ordered_pose_transforms_match_unity_export_boundary() {
        let (p, mut s, a, r) = fixture(Some(2));
        let lattice = CellLattice::new(1000., 7.5).unwrap();
        for yaw in [0_f32, 37.5, 90., 180., 270.] {
            s.outcomes[0].position.as_mut().unwrap().yaw = yaw;
            let inputs = prepare(&p, &s, &a, &r, lattice, false, || false)
                .unwrap()
                .unwrap();
            assert_eq!(
                inputs
                    .endpoints
                    .iter()
                    .map(|e| e.connector)
                    .collect::<Vec<_>>(),
                [1000, 80]
            );
            assert!(!inputs.endpoints[1].hierarchy_active_without_root);
            assert_eq!(inputs.endpoints[0].monument, u64::MAX - 1);
            assert_eq!(inputs.connectors[0].monument, 0);
            assert_eq!(inputs.connectors[0].radius, 8); // Bounds centre must not enlarge source push radius.
            assert_eq!(
                inputs.connectors[0].centre,
                lattice.cell([100., 200.]).unwrap()
            );
            let angle = -yaw.to_radians() * 0.5;
            let offset =
                papyrus_terrain::decor_fit::rotate([0., angle.sin(), 0., angle.cos()], [10., 2., 20.]);
            for (actual, expected) in inputs.endpoints[0].world.into_iter().zip([
                100. + offset[0],
                20. + offset[1],
                200. + offset[2],
            ]) {
                assert!((actual - expected).abs() < 0.0001);
            }
        }
        let trail = prepare(&p, &s, &a, &r, lattice, true, || false)
            .unwrap()
            .unwrap();
        assert_eq!(trail.endpoints.len(), 1);
        assert_eq!(trail.endpoints[0].connector, 81);
        assert!(!trail.endpoints[0].hierarchy_active_without_root);
    }
    #[test]
    fn roadside_nonmonument_and_missing_role_cases_never_fall_back_to_names_or_numbers() {
        let lattice = CellLattice::new(1000., 7.5).unwrap();
        for kind in [None, Some(99)] {
            let (p, s, a, r) = fixture(kind);
            let inputs = prepare(&p, &s, &a, &r, lattice, false, || false)
                .unwrap()
                .unwrap();
            assert!(inputs.connectors.is_empty());
            assert_eq!(
                inputs.skipped_nonmonuments.len(),
                usize::from(kind.is_none())
            );
            assert_eq!(inputs.skipped_roadside.len(), usize::from(kind.is_some()));
        }
        for field in ["Road", "Trail"] {
            let mut doc = role_document();
            doc["integer_fields"]["InfrastructureType"][field] = Value::Null;
            assert!(
                Roles::parse(
                    &serde_json::to_vec(&doc).unwrap(),
                    &"b".repeat(64),
                    &receipts(),
                    7,
                    9
                )
                .is_err()
            );
        }
        let mut doc = role_document();
        doc["integer_fields"]["MonumentType"]["Roadside"] = Value::Null;
        assert!(
            Roles::parse(
                &serde_json::to_vec(&doc).unwrap(),
                &"b".repeat(64),
                &receipts(),
                7,
                9
            )
            .is_err()
        );
        let mut doc = role_document();
        doc["sources"][0]["sha256"] = "0".repeat(64).into();
        assert!(
            Roles::parse(
                &serde_json::to_vec(&doc).unwrap(),
                &"b".repeat(64),
                &receipts(),
                7,
                9
            )
            .is_err()
        );
    }
    #[test]
    fn mismatched_placement_source_disabled_trails_and_cancellation_are_atomic() {
        use std::cell::Cell;
        let (mut p, mut s, a, r) = fixture(Some(2));
        let lattice = CellLattice::new(1000., 7.5).unwrap();
        assert!(
            prepare(
                &p,
                &s,
                &a,
                &r,
                CellLattice::new(2000., 7.5).unwrap(),
                false,
                || false
            )
            .is_err()
        );
        p.roads.as_mut().unwrap().include_trails = Some(false);
        assert!(prepare(&p, &s, &a, &r, lattice, true, || false).is_err());
        let polls = Cell::new(0);
        assert!(
            prepare(&p, &s, &a, &r, lattice, false, || {
                polls.set(polls.get() + 1);
                polls.get() > 2
            })
            .unwrap()
            .is_none()
        );
        s.outcomes[0].id = 3;
        assert!(prepare(&p, &s, &a, &r, lattice, false, || false).is_err());
    }
}
