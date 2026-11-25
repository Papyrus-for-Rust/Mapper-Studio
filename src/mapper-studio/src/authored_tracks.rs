//! Immutable installation-bound authored tracks. No collider or runtime join acceptance.
use crate::{
    infrastructure,
    installation::{self, SourceFile},
    prefab_assets::Catalog,
    rail_source,
};
use papyrus_terrain::authored_spline::Curve;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
};
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Track {
    pub object_id: i64,
    pub curve_index: usize,
    pub transform: [[f64; 4]; 4],
    pub rotation: [[f64; 3]; 3],
    pub local_scale: [f64; 3],
    pub component_enabled: bool,
    pub hierarchy_active_without_root: bool,
    pub hierarchy: i32,
    pub is_station: bool,
    pub above_ground_spawn: bool,
}
#[derive(serde::Serialize)]
pub struct Port {
    pub path_id: u64,
    pub monument: u64,
    pub connector: i64,
}
fn port_checks(
    graph: &papyrus_terrain::rail_track_graph::Graph,
    owners: &BTreeMap<u64, u64>,
    generated: &BTreeMap<u64, u64>,
    ports: &[Port],
) -> Result<Vec<Value>, String> {
    let mut result = Vec::new();
    for p in ports {
        let world = *generated
            .get(&p.path_id)
            .ok_or("Missing generated monument approach")?;
        let span = graph
            .spans
            .iter()
            .find(|s| s.track_id == world && s.first_control == 0)
            .ok_or("Missing generated approach start span")?;
        let mut connections = Vec::new();
        for (i, c) in graph.connections.iter().enumerate() {
            let other = if c.a.span == span.id && !c.a.end {
                Some(c.b)
            } else if c.b.span == span.id && !c.b.end {
                Some(c.a)
            } else {
                None
            };
            if let Some(endpoint) = other
                && owners.get(&graph.spans[endpoint.span].track_id) == Some(&p.monument)
            {
                connections.push(i);
            }
        }
        result.push(json!({"port":p,"connected_to_expected_monument":!connections.is_empty(),"connection_indices":connections}));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_ports_must_join_the_expected_monument_even_in_a_connected_graph() {
        use papyrus_terrain::rail_track_graph::{self, Policy, Settings, StringTag, Track};
        let tracks = [
            Track {
                id: 1,
                generated: false,
                points: vec![[0., 0., 0.], [10., 0., 0.]],
                tangents: vec![[1., 0., 0.]; 2],
            },
            Track {
                id: 2,
                generated: true,
                points: vec![[10., 0., 0.], [20., 0., 0.]],
                tangents: vec![[1., 0., 0.]; 2],
            },
        ];
        let graph = rail_track_graph::build(
            &tracks,
            Policy {
                engine: StringTag::V1,
                maximum_distance_squared: 0.010000001,
                maximum_angle_degrees: 10.,
            },
            Settings { maximum_work: 1000 },
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(graph.components.len(), 1);
        let owners = [(1, 77)].into();
        let generated = [(7, 2)].into();
        let wrong = port_checks(
            &graph,
            &owners,
            &generated,
            &[Port {
                path_id: 7,
                monument: 78,
                connector: 42,
            }],
        )
        .unwrap();
        assert_eq!(wrong[0]["connected_to_expected_monument"], false);
        let right = port_checks(
            &graph,
            &owners,
            &generated,
            &[Port {
                path_id: 7,
                monument: 77,
                connector: 42,
            }],
        )
        .unwrap();
        assert_eq!(right[0]["connected_to_expected_monument"], true);
    }
    #[test]
    fn snapshot_identity_curves_and_instance_references_are_strict_and_cancelable() {
        let source = SourceFile {
            relative_path: "fixture".into(),
            sha256: "a".repeat(64),
            bytes: 1,
        };
        let sources = vec![source];
        let doc = json!({"format":"papyrus-authored-track-source","schema":1,"adapter":"papyrus-authored-tracks-1",
            "catalog_sha256":"b".repeat(64),"infrastructure_sha256":"c".repeat(64),"rail_source_sha256":"d".repeat(64),
            "sources":sources,"extra_sources":[{"relative_path":"RustClient_Data/resources.assets","sha256":"e".repeat(64),"bytes":1},
            {"relative_path":"RustClient_Data/sharedassets0.assets","sha256":"f".repeat(64),"bytes":1}],"code_sources":[],
            "profile_sha256":"a".repeat(64),"extractor_sha256":"b".repeat(64),
            "curves":[{"points":[[0,0,0],[2,0,0]],"tangents":[[2,0,0],[3,0,0]],"length":2,"lut_interval":0.25,
                "samples":[{"distance":0,"position":[0,0,0]},{"distance":2,"position":[2,0,0]}]}],
            "prefabs":{"test.prefab":{"tracks":[{"object_id":1,"curve_index":0,"transform":[[1,0,0,0],[0,1,0,0],[0,0,1,0],[0,0,0,1]],
                "rotation":[[1,0,0],[0,1,0],[0,0,1]],"local_scale":[1,1,1],
                "component_enabled":false,"hierarchy_active_without_root":false,"hierarchy":1,"is_station":false,"above_ground_spawn":false}]}}});
        let decode = |value: &Value, cancel| {
            Source::decode(
                &serde_json::to_vec(value).unwrap(),
                Expected {
                    catalog: &"b".repeat(64),
                    sources: &sources,
                    infrastructure: &"c".repeat(64),
                    rail: &"d".repeat(64),
                    code: &[],
                    prefabs: ["test.prefab"].into(),
                },
                || cancel,
            )
        };
        assert!(decode(&doc, false).is_ok());
        assert!(decode(&doc, true).is_err());
        for (pointer, value) in [
            ("/catalog_sha256", json!("f".repeat(64))),
            ("/rail_source_sha256", json!("f".repeat(64))),
            ("/extra_sources/0/relative_path", json!("../outside")),
            ("/curves/0/samples/1/distance", json!(1)),
            ("/prefabs/test.prefab/tracks/0/curve_index", json!(3)),
            ("/prefabs/test.prefab/tracks/0/transform/3/3", json!(0)),
            ("/prefabs/test.prefab/tracks/0/transform/0/0", json!(0)),
        ] {
            let mut changed = doc.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(decode(&changed, false).is_err(), "accepted {pointer}");
        }
    }
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Prefab {
    tracks: Vec<Track>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    format: String,
    schema: u32,
    adapter: String,
    catalog_sha256: String,
    infrastructure_sha256: String,
    rail_source_sha256: String,
    sources: Vec<SourceFile>,
    extra_sources: Vec<SourceFile>,
    code_sources: Vec<SourceFile>,
    profile_sha256: String,
    extractor_sha256: String,
    curves: Vec<Curve>,
    prefabs: BTreeMap<String, Prefab>,
    #[serde(default)]
    connections: Option<papyrus_terrain::rail_track_graph::Policy>,
}
pub struct Source {
    pub sha256: String,
    doc: Document,
}
struct Expected<'a> {
    catalog: &'a str,
    sources: &'a [SourceFile],
    infrastructure: &'a str,
    rail: &'a str,
    code: &'a [SourceFile],
    prefabs: BTreeSet<&'a str>,
}
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl Source {
    pub fn world_network(
        &self,
        placed: &crate::rail_preview::Monuments,
        generated: &[papyrus_terrain::rail_bed::FittedTrack],
        ports: &[Port],
        rail: &rail_source::Source,
        settings: papyrus_terrain::rail_track_graph::Settings,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Value>, String> {
        let policy = self.doc.connections.ok_or(
            "Authored track snapshot has no world connection adapter; prepare a new snapshot",
        )?;
        settings.validate()?;
        if self.doc.rail_source_sha256 != rail.sha256 {
            return Err("Authored world graph differs from the selected rail snapshot".into());
        }
        let offset = rail.train_spline_offset();
        let mut tracks = Vec::new();
        let mut provenance = Vec::new();
        let mut inactive = 0;
        let mut controls = 0usize;
        let mut owners = BTreeMap::new();
        let mut generated_world = BTreeMap::new();
        let mut placements: Vec<_> = placed.poses.iter().collect();
        placements.sort_by_key(|p| p.monument);
        let mut ids = BTreeSet::new();
        for placement in placements {
            if cancelled() {
                return Ok(None);
            }
            if placement.monument == 0
                || !ids.insert(placement.monument)
                || !placement.yaw.is_finite()
                || placement
                    .position
                    .iter()
                    .any(|v| !v.is_finite() || v.abs() > 10000.)
            {
                return Err("Invalid accepted authored track placement".into());
            }
            let p = self
                .doc
                .prefabs
                .get(&placement.prefab)
                .ok_or("Accepted rail monument lacks authored tracks")?;
            let (s, c) = placement.yaw.to_radians().sin_cos();
            let s = f64::from(s);
            let c = f64::from(c);
            let yaw = |v: [f64; 3]| [c * v[0] - s * v[2], v[1], s * v[0] + c * v[2]];
            let mut instances: Vec<_> = p.tracks.iter().collect();
            instances.sort_by_key(|t| t.object_id);
            for t in instances {
                if cancelled() {
                    return Ok(None);
                }
                if !t.hierarchy_active_without_root {
                    inactive += 1;
                    continue;
                }
                let curve = &self.doc.curves[t.curve_index];
                controls += curve.points.len();
                if controls > 131072 || tracks.len() >= 8192 {
                    return Err(
                        "Placed authored track graph exceeds its control/instance allowance".into(),
                    );
                }
                let points = curve
                    .points
                    .iter()
                    .map(|point| {
                        let local: [f64; 3] = std::array::from_fn(|i| {
                            t.transform[i][3]
                                + (0..3)
                                    .map(|j| t.transform[i][j] * f64::from(point[j]))
                                    .sum::<f64>()
                        });
                        let rotated = yaw(local);
                        std::array::from_fn(|i| {
                            (f64::from(placement.position[i]) + rotated[i]) as f32
                        })
                    })
                    .collect();
                // WorldSpline endpoint collation rotates first, then applies
                // owner-local scale in world axes. Do not commute these steps.
                let tangents = curve
                    .tangents
                    .iter()
                    .map(|tangent| {
                        let local: [f64; 3] = std::array::from_fn(|i| {
                            (0..3)
                                .map(|j| t.rotation[i][j] * f64::from(tangent[j]))
                                .sum::<f64>()
                        });
                        let rotated = yaw(local);
                        std::array::from_fn(|i| (rotated[i] * t.local_scale[i]) as f32)
                    })
                    .collect();
                let id = tracks.len() as u64 + 1;
                tracks.push(papyrus_terrain::rail_track_graph::Track {
                    id,
                    generated: false,
                    points,
                    tangents,
                });
                provenance.push(json!({"track_id":id,"kind":"authored","monument":placement.monument,"prefab":placement.prefab,
                    "object_id":t.object_id,"curve_index":t.curve_index,"component_enabled":t.component_enabled}));
                owners.insert(id, placement.monument);
            }
        }
        let mut generated: Vec<_> = generated.iter().collect();
        generated.sort_by_key(|t| t.id);
        let mut generated_ids = BTreeSet::new();
        let required: BTreeSet<_> = generated
            .iter()
            .filter(|t| t.preserve_start)
            .map(|t| t.id)
            .collect();
        if ports.len() != required.len()
            || ports.iter().map(|p| p.path_id).collect::<BTreeSet<_>>() != required
            || ports.iter().any(|p| !ids.contains(&p.monument))
        {
            return Err("World track graph lacks exact accepted approach port bindings".into());
        }
        for t in generated {
            if t.id == 0 || !generated_ids.insert(t.id) {
                return Err("Duplicate generated track identity".into());
            }
            let id = tracks.len() as u64 + 1;
            controls += t.path.points().len();
            if controls > 131072 || tracks.len() >= 8192 {
                return Err(
                    "Combined world track graph exceeds its control/instance allowance".into(),
                );
            }
            let points = t
                .path
                .points()
                .iter()
                .map(|p| [p[0], p[1] + offset, p[2]])
                .collect();
            tracks.push(papyrus_terrain::rail_track_graph::Track {
                id,
                generated: true,
                points,
                tangents: t.path.tangents().to_vec(),
            });
            provenance.push(json!({"track_id":id,"kind":"generated","path_id":t.id}));
            generated_world.insert(t.id, id);
        }
        let Some(graph) =
            papyrus_terrain::rail_track_graph::build(&tracks, policy, settings, &cancelled)?
        else {
            return Ok(None);
        };
        let ports = port_checks(&graph, &owners, &generated_world, ports)?;
        let all_ports = ports
            .iter()
            .all(|p| p["connected_to_expected_monument"] == true);
        Ok(Some(
            json!({"engine":papyrus_terrain::rail_track_graph::VERSION,"authored_source_sha256":self.sha256,
            "layout_sha256":placed.layout_sha256,"settings":settings,"policy":policy,"accepted_monuments":placed.poses.len(),
            "skipped_inactive_tracks":inactive,"track_provenance":provenance,"world_tracks":tracks,"graph":graph,
            "monument_ports":ports,"all_monument_ports_connected":all_ports,
            "world_pose_graph_built":true,"runtime_collation_validated":false,"colliders_validated":false,"traversal_validated":false,
            "ordering":"stable track IDs, simultaneous cuts at matching original endpoints; independent of Unity object enumeration"}),
        ))
    }
    fn decode(raw: &[u8], e: Expected<'_>, cancelled: impl Fn() -> bool) -> Result<Self, String> {
        if raw.len() > 16 * 1024 * 1024 {
            return Err("Authored track snapshot exceeds 16 MiB".into());
        }
        let d: Document = serde_json::from_slice(raw).map_err(|e| e.to_string())?;
        if d.format != "papyrus-authored-track-source"
            || d.schema != 1
            || d.adapter != "papyrus-authored-tracks-1"
            || d.catalog_sha256 != e.catalog
            || d.infrastructure_sha256 != e.infrastructure
            || d.rail_source_sha256 != e.rail
            || d.sources != e.sources
            || d.code_sources != e.code
            || !hash(&d.profile_sha256)
            || !hash(&d.extractor_sha256)
            || d.prefabs
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                != e.prefabs
            || d.prefabs.is_empty()
            || d.prefabs.len() > 512
            || d.curves.is_empty()
            || d.curves.len() > 1024
            || d.extra_sources.len() != 2
        {
            return Err("Authored tracks differ from selected installation/rail source".into());
        }
        for (r, path) in d.extra_sources.iter().zip([
            "RustClient_Data/resources.assets",
            "RustClient_Data/sharedassets0.assets",
        ]) {
            if r.relative_path != path
                || !hash(&r.sha256)
                || r.bytes == 0
                || r.bytes > 128 * 1024 * 1024
            {
                return Err("Invalid authored track resource receipt".into());
            }
        }
        if let Some(policy) = d.connections {
            policy.validate()?;
        }
        let mut samples = 0;
        for c in &d.curves {
            if cancelled() {
                return Err("Authored track load cancelled".into());
            }
            c.validate()?;
            samples += c.samples.len();
            if samples > 1000000 {
                return Err("Authored track sample allowance exceeded".into());
            }
        }
        let mut total = 0;
        for p in d.prefabs.values() {
            let mut ids = BTreeSet::new();
            total += p.tracks.len();
            if p.tracks.is_empty() || p.tracks.len() > 4096 || total > 8192 {
                return Err("Authored track instance allowance exceeded".into());
            }
            for t in &p.tracks {
                if cancelled() {
                    return Err("Authored track load cancelled".into());
                }
                let m = &t.transform;
                let determinant = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
                    - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
                    + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
                if t.object_id == 0
                    || !ids.insert(t.object_id)
                    || t.curve_index >= d.curves.len()
                    || !(0..=255).contains(&t.hierarchy)
                    || m.iter()
                        .flatten()
                        .any(|v| !v.is_finite() || v.abs() > 10000.)
                    || m[3] != [0., 0., 0., 1.]
                    || determinant.abs() < 1e-8
                    || t.rotation
                        .iter()
                        .flatten()
                        .chain(&t.local_scale)
                        .any(|v| !v.is_finite() || v.abs() > 10000.)
                    || t.rotation
                        .iter()
                        .any(|r| (r.iter().map(|v| v * v).sum::<f64>() - 1.).abs() > 0.001)
                    || (0..3).any(|i| {
                        (i + 1..3).any(|j| {
                            t.rotation[i]
                                .iter()
                                .zip(t.rotation[j])
                                .map(|(a, b)| a * b)
                                .sum::<f64>()
                                .abs()
                                > 0.001
                        })
                    })
                {
                    return Err("Invalid authored track transform/reference".into());
                }
            }
        }
        Ok(Self {
            sha256: format!("{:x}", Sha256::digest(raw)),
            doc: d,
        })
    }
    pub fn load(
        root: &Path,
        path: &Path,
        catalog: &Catalog,
        infrastructure: &infrastructure::Source,
        rail: &rail_source::Source,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, String> {
        let mut raw = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        let source = Self::decode(
            &raw,
            Expected {
                catalog: &catalog.sha256,
                sources: &catalog.sources,
                infrastructure: &infrastructure.sha256,
                rail: &rail.sha256,
                code: rail.gameplay_sources(),
                prefabs: infrastructure
                    .prefabs
                    .iter()
                    .filter(|(_, p)| {
                        p.connectors
                            .iter()
                            .any(|c| c.infrastructure_type == rail.profile.rail_type)
                    })
                    .map(|(p, _)| p.as_str())
                    .collect(),
            },
            &cancelled,
        )?;
        for receipt in &source.doc.extra_sources {
            if installation::hash_file(root, &receipt.relative_path, 128 * 1024 * 1024, &cancelled)?
                != *receipt
            {
                return Err("Authored track resource differs from selected installation".into());
            }
        }
        Ok(source)
    }
    pub fn audit(
        &self,
        infrastructure: &infrastructure::Source,
        rail: &rail_source::Source,
    ) -> Result<Value, String> {
        if self.doc.infrastructure_sha256 != infrastructure.sha256
            || self.doc.rail_source_sha256 != rail.sha256
        {
            return Err("Authored endpoint inspection differs from the selected sources".into());
        }
        let mut ports = Vec::new();
        for (prefab, p) in &self.doc.prefabs {
            for socket in infrastructure.prefabs[prefab]
                .connectors
                .iter()
                .filter(|c| c.infrastructure_type == rail.profile.rail_type)
            {
                let expected = [
                    f64::from(socket.position[0]),
                    f64::from(socket.position[1] + rail.train_spline_offset()),
                    f64::from(socket.position[2]),
                ];
                let mut best: Option<Value> = None;
                let mut best_gap = f64::INFINITY;
                for t in &p.tracks {
                    let c = &self.doc.curves[t.curve_index];
                    for end in [false, true] {
                        let pose = c.sample(if end { c.length } else { 0. })?;
                        let position: [f64; 3] = std::array::from_fn(|i| {
                            t.transform[i][3]
                                + (0..3)
                                    .map(|j| t.transform[i][j] * f64::from(pose.position[j]))
                                    .sum::<f64>()
                        });
                        let tangent: [f64; 3] = std::array::from_fn(|i| {
                            (0..3)
                                .map(|j| {
                                    t.rotation[i][j]
                                        * t.local_scale[i]
                                        * f64::from(pose.tangent[j])
                                        * if end { 1. } else { -1. }
                                })
                                .sum::<f64>()
                        });
                        let gap = position
                            .into_iter()
                            .zip(expected)
                            .map(|(a, b)| (a - b).powi(2))
                            .sum::<f64>()
                            .sqrt();
                        if gap < best_gap {
                            best_gap = gap;
                            let length = tangent.iter().map(|v| v * v).sum::<f64>().sqrt();
                            let dot = tangent
                                .into_iter()
                                .zip(socket.forward)
                                .map(|(a, b)| a * f64::from(b))
                                .sum::<f64>();
                            let angle = (length > 0.)
                                .then(|| (dot / length).clamp(-1., 1.).acos().to_degrees());
                            best = Some(
                                json!({"track_object_id":t.object_id,"curve_index":t.curve_index,"end":end,"position_gap":gap,"outgoing_tangent_angle_degrees":angle,
                            "component_enabled":t.component_enabled,"hierarchy_active_without_root":t.hierarchy_active_without_root}),
                            );
                        }
                    }
                }
                ports.push(json!({"prefab":prefab,"connector_object_id":socket.object_id,"nearest_authored_endpoint":best}));
            }
        }
        Ok(
            json!({"source_sha256":self.sha256,"curves":self.doc.curves.len(),"lookup_samples":self.doc.curves.iter().map(|c|c.samples.len()).sum::<usize>(),
            "prefabs":self.doc.prefabs.len(),"track_instances":self.doc.prefabs.values().map(|p|p.tracks.len()).sum::<usize>(),"local_connector_candidates":ports,
            "scope":"Effective shared LUT data and root-local instance transforms; nearest endpoints are diagnostic candidates only",
            "world_connections_validated":false,"colliders_validated":false,"playable":false}),
        )
    }
}
