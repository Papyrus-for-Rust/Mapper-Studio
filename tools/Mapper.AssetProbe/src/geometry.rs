//! Numeric hierarchy/water metadata. Mesh payloads are never requested.
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
type Matrix = [[f64; 4]; 4];
const IDENTITY: Matrix = [
    [1., 0., 0., 0.],
    [0., 1., 0., 0.],
    [0., 0., 1., 0.],
    [0., 0., 0., 1.],
];
fn reference(value: &Value) -> Result<i64, String> {
    let id = value["m_PathID"].as_i64();
    if value["m_FileID"].as_i64() != Some(0) || id.is_none_or(|id| id == 0) {
        return Err("Missing or external object reference".into());
    }
    Ok(id.unwrap())
}
fn vector(value: &Value, names: &str) -> Result<Vec<f64>, String> {
    names
        .chars()
        .map(|c| {
            value[c.to_string()]
                .as_f64()
                .filter(|v| v.is_finite() && v.abs() <= 1_000_000.)
                .ok_or_else(|| "Nonfinite or excessive transform".into())
        })
        .collect()
}
fn flag(value: &Value) -> Result<bool, String> {
    match value {
        Value::Bool(v) => Ok(*v),
        Value::Number(n) if n.as_i64() == Some(0) => Ok(false),
        Value::Number(n) if n.as_i64() == Some(1) => Ok(true),
        _ => Err("Unknown serialized boolean state".into()),
    }
}
fn enabled(fields: &Value) -> Result<bool, String> {
    flag(&fields["m_Enabled"])
}
fn body_type(value: &Value) -> Result<i64, String> {
    value
        .as_i64()
        .filter(|v| [1, 2, 4, 8, 16, 32].contains(v))
        .ok_or_else(|| "Unknown water body type requires an adapter update".into())
}
fn multiply(a: Matrix, b: Matrix) -> Result<Matrix, String> {
    let out = std::array::from_fn(|i| {
        std::array::from_fn(|j| (0..4).fold(0., |sum, k| sum + a[i][k] * b[k][j]))
    });
    if out
        .iter()
        .flatten()
        .any(|v| !v.is_finite() || v.abs() > 1_000_000.)
    {
        return Err("Hierarchy transform exceeds inspection bounds".into());
    }
    Ok(out)
}
fn matrix(fields: &Value) -> Result<Matrix, String> {
    let p = vector(&fields["m_LocalPosition"], "xyz")?;
    let s = vector(&fields["m_LocalScale"], "xyz")?;
    let q = vector(&fields["m_LocalRotation"], "xyzw")?;
    let norm = q.iter().fold(0., |a, v| a + v * v).sqrt();
    if (norm - 1.).abs() > 0.001 {
        return Err("Invalid transform quaternion".into());
    }
    let (x, y, z, w) = (q[0] / norm, q[1] / norm, q[2] / norm, q[3] / norm);
    let r = [
        [
            1. - 2. * (y * y + z * z),
            2. * (x * y - z * w),
            2. * (x * z + y * w),
        ],
        [
            2. * (x * y + z * w),
            1. - 2. * (x * x + z * z),
            2. * (y * z - x * w),
        ],
        [
            2. * (x * z - y * w),
            2. * (y * z + x * w),
            1. - 2. * (x * x + y * y),
        ],
    ];
    Ok(std::array::from_fn(|i| {
        std::array::from_fn(|j| {
            if i == 3 {
                if j == 3 { 1. } else { 0. }
            } else if j == 3 {
                p[i]
            } else {
                r[i][j] * s[j]
            }
        })
    }))
}
fn box_geometry(world: Matrix, fields: &Value) -> Result<Value, String> {
    let size = vector(&fields["m_Size"], "xyz")?;
    if size.iter().any(|v| *v <= 0.) {
        return Err("Invalid box dimensions".into());
    }
    let local = vector(&fields["m_Center"], "xyz")?;
    let center: [f64; 3] =
        std::array::from_fn(|i| (0..3).fold(0., |a, j| a + world[i][j] * local[j]) + world[i][3]);
    let axes: [[f64; 3]; 3] =
        std::array::from_fn(|j| std::array::from_fn(|i| world[i][j] * size[j] / 2.));
    if center
        .iter()
        .chain(axes.iter().flatten())
        .any(|v| !v.is_finite() || v.abs() > 1_000_000.)
    {
        return Err("Excessive collider bounds".into());
    }
    let lengths = axes.map(|a| a.iter().fold(0., |s, v| s + v * v).sqrt());
    if lengths.iter().any(|v| *v < 1e-8) {
        return Err("Degenerate collider axes".into());
    }
    let flat = axes[1][0].abs() + axes[1][2].abs() <= lengths[1] * 1e-6
        && axes[0][1].abs() + axes[2][1].abs() <= lengths[0].max(lengths[2]) * 1e-6
        && (0..3).fold(0., |a, i| a + axes[0][i] * axes[2][i]).abs()
            <= lengths[0] * lengths[2] * 1e-6;
    Ok(json!({"shape":"box","center":center,"halfAxes":axes,"flatOrthogonal":flat}))
}
fn collider_kind(kind: i32) -> Option<&'static str> {
    match kind {
        64 => Some("mesh"),
        65 => Some("box"),
        135 => Some("sphere"),
        136 => Some("capsule"),
        154 => Some("terrain"),
        _ => None,
    }
}
fn components(go: &Value) -> Result<Vec<i64>, String> {
    go["m_Component"]
        .as_array()
        .filter(|a| a.len() <= 256)
        .ok_or("Component count exceeds inspection bounds")?
        .iter()
        .map(|v| reference(&v["component"]))
        .collect()
}
struct Record {
    kind: i32,
    fields: Value,
}
struct Node {
    world: Matrix,
    active: bool,
    relative: bool,
    ids: Vec<i64>,
}
struct Inspection<F> {
    read: F,
    cache: HashMap<i64, Arc<Record>>,
    nodes: Vec<(i64, Node)>,
    by_go: HashMap<i64, usize>,
    result: Value,
}
impl<F: FnMut(i64) -> Result<(i32, Value), String>> Inspection<F> {
    fn get(&mut self, id: i64) -> Result<Arc<Record>, String> {
        if let Some(record) = self.cache.get(&id) {
            return Ok(record.clone());
        }
        if self.cache.len() >= 262144 {
            return Err("Object count exceeds inspection bounds".into());
        }
        let (kind, fields) = (self.read)(id)?;
        let record = Arc::new(Record { kind, fields });
        self.cache.insert(id, record.clone());
        Ok(record)
    }
    fn issue(&mut self, id: i64, error: String) {
        self.result["complete"] = false.into();
        let issues = self.result["issues"].as_array_mut().unwrap();
        if issues.len() < 256 {
            issues
                .push(json!({"objectId":id,"reason":error.chars().take(256).collect::<String>()}));
        }
    }
    fn hierarchy(&mut self, root: i64) -> Result<(), String> {
        let go = self.get(root)?;
        if go.kind != 1 {
            return Err("Prefab root is not a GameObject".into());
        }
        let mut roots = Vec::new();
        for id in components(&go.fields)? {
            if [4, 224].contains(&self.get(id)?.kind) {
                roots.push(id);
            }
        }
        if roots.len() != 1 {
            return Err("Prefab needs exactly one root Transform".into());
        }
        let root_transform = self.get(roots[0])?;
        if vector(&root_transform.fields["m_LocalScale"], "xyz")?
            .iter()
            .any(|v| (*v - 1.).abs() > 1e-6)
        {
            return Err("Non-unit prefab root scale needs an instance-scale adapter".into());
        }
        let mut stack = vec![(roots[0], IDENTITY, true, true, 0)];
        let mut seen = HashSet::new();
        while let Some((id, parent, parent_active, parent_relative, depth)) = stack.pop() {
            if seen.contains(&id) {
                return Err("Cyclic or shared prefab hierarchy".into());
            }
            if seen.len() >= 65536 || depth > 128 {
                return Err("Prefab hierarchy exceeds node/depth budget".into());
            }
            seen.insert(id);
            let transform = self.get(id)?;
            if ![4, 224].contains(&transform.kind) {
                return Err("Hierarchy child is not a Transform".into());
            }
            let go_id = reference(&transform.fields["m_GameObject"])?;
            let go = self.get(go_id)?;
            if go.kind != 1 || self.by_go.contains_key(&go_id) {
                return Err("Invalid or shared GameObject".into());
            }
            let local = if depth == 0 {
                IDENTITY
            } else {
                matrix(&transform.fields)?
            };
            let world = multiply(parent, local)?;
            let own_active =
                flag(&go.fields["m_IsActive"]).map_err(|_| "Unknown GameObject active state")?;
            let active = parent_active && own_active;
            let relative = parent_relative && (depth == 0 || own_active);
            let ids = components(&go.fields)?;
            self.by_go.insert(go_id, self.nodes.len());
            self.nodes.push((
                go_id,
                Node {
                    world,
                    active,
                    relative,
                    ids,
                },
            ));
            self.result["nodesVisited"] = self.nodes.len().into();
            let children = transform.fields["m_Children"]
                .as_array()
                .filter(|a| a.len() <= 65536)
                .ok_or("Excessive child transform list")?;
            for child in children.iter().rev() {
                stack.push((reference(child)?, world, active, relative, depth + 1));
            }
            // The world matrix, flags and component IDs now live in Node. Later
            // water scanning needs only these objects' class IDs, not their
            // duplicate serialized hierarchy fields. Seen/shared checks run
            // before any possible reuse of the cleared records.
            self.cache.insert(
                id,
                Arc::new(Record {
                    kind: transform.kind,
                    fields: Value::Null,
                }),
            );
            self.cache.insert(
                go_id,
                Arc::new(Record {
                    kind: go.kind,
                    fields: Value::Null,
                }),
            );
        }
        Ok(())
    }
    fn collider(&mut self, id: i64) -> Result<Value, String> {
        let record = self.get(id)?;
        let shape = collider_kind(record.kind)
            .ok_or("Water reference is not a supported collider class")?;
        let go = reference(&record.fields["m_GameObject"])?;
        let &index = self
            .by_go
            .get(&go)
            .ok_or("Collider lies outside the prefab hierarchy")?;
        let node = &self.nodes[index].1;
        if !node.ids.contains(&id) {
            return Err("Collider reference contradicts its GameObject".into());
        }
        let geometry = if record.kind == 65 {
            box_geometry(node.world, &record.fields)?
        } else {
            json!({"shape":shape})
        };
        let enabled = enabled(&record.fields)?;
        Ok(
            json!({"objectId":id,"active":node.active && enabled,"componentEnabled":enabled,"hierarchyActive":node.active,"hierarchyActiveWithoutRoot":node.relative,"geometry":geometry}),
        )
    }
    fn bodies(&mut self) {
        for index in 0..self.nodes.len() {
            let (go_id, node) = &self.nodes[index];
            let (go_id, active, relative, ids) =
                (*go_id, node.active, node.relative, node.ids.clone());
            let mut scripts = Vec::new();
            for id in &ids {
                let result = (|| {
                    let record = self.get(*id)?;
                    if record.kind == 114 {
                        enabled(&record.fields)?;
                        Ok(Some(record))
                    } else {
                        Ok(None)
                    }
                })();
                match result {
                    Ok(Some(record)) => scripts.push((*id, record)),
                    Ok(None) => {}
                    Err(error) => self.issue(*id, error),
                }
            }
            let water: Vec<_> = scripts
                .iter()
                .filter(|(_, r)| {
                    ["Type", "Triggers", "IsOcean", "MaterialBlend"]
                        .iter()
                        .all(|key| r.fields.get(key).is_some())
                })
                .collect();
            let additions: Vec<_> = scripts
                .iter()
                .filter(|(_, r)| r.fields.get("automaticallyAddTerrainTopology").is_some())
                .collect();
            if water.len() > 1 || additions.len() > 1 {
                self.issue(go_id, "Ambiguous water components".into());
                continue;
            }
            for (id, record) in &water {
                let result = (|| -> Result<Value, String> {
                    let bodies = self.result["waterBodies"].as_array().unwrap();
                    let references = record.fields["Triggers"]
                        .as_array()
                        .filter(|a| a.len() <= 1024)
                        .ok_or("Water body count exceeds inspection bounds")?;
                    if bodies.len() >= 1024 {
                        return Err("Water body count exceeds inspection bounds".into());
                    }
                    let existing: usize = bodies
                        .iter()
                        .map(|v| v["triggers"].as_array().unwrap().len())
                        .sum();
                    let mut triggers = Vec::new();
                    let mut unique = HashSet::new();
                    for pointer in references {
                        let id = reference(pointer)?;
                        unique.insert(id);
                        triggers.push(self.collider(id)?);
                    }
                    if unique.len() != triggers.len() || existing + triggers.len() > 4096 {
                        return Err("Duplicate or excessive water trigger references".into());
                    }
                    let enabled = enabled(&record.fields)?;
                    Ok(
                        json!({"objectId":id,"active":active && enabled,"componentEnabled":enabled,"hierarchyActive":active,"hierarchyActiveWithoutRoot":relative,"bodyType":body_type(&record.fields["Type"])? ,"isOcean":flag(&record.fields["IsOcean"])? ,"triggers":triggers}),
                    )
                })();
                match result {
                    Ok(body) => self.result["waterBodies"]
                        .as_array_mut()
                        .unwrap()
                        .push(body),
                    Err(error) => self.issue(*id, error),
                }
            }
            for (id, record) in additions {
                let result = (|| -> Result<Value, String> {
                    if self.result["contributors"].as_array().unwrap().len() >= 1024 {
                        return Err("Water contributor count exceeds inspection bounds".into());
                    }
                    let mut candidates = Vec::new();
                    for id in &ids {
                        if collider_kind(self.get(*id)?.kind).is_some() {
                            candidates.push(*id);
                        }
                    }
                    if candidates.len() != 1 {
                        return Err("AddToWaterMap needs one unambiguous collider".into());
                    }
                    let shape = self.collider(candidates[0])?;
                    let enabled = enabled(&record.fields)?;
                    let kind = water
                        .first()
                        .map(|(_, r)| body_type(&r.fields["Type"]))
                        .transpose()?;
                    Ok(
                        json!({"objectId":id,"active":active && enabled,"componentEnabled":enabled,"hierarchyActive":active,"hierarchyActiveWithoutRoot":relative,"bodyType":kind,"addTopology":flag(&record.fields["automaticallyAddTerrainTopology"])? ,"collider":shape}),
                    )
                })();
                match result {
                    Ok(body) => self.result["contributors"]
                        .as_array_mut()
                        .unwrap()
                        .push(body),
                    Err(error) => self.issue(*id, error),
                }
            }
        }
    }
}
pub fn inspect(root: i64, read: impl FnMut(i64) -> Result<(i32, Value), String>) -> Value {
    let mut inspection = Inspection {
        read,
        cache: HashMap::new(),
        nodes: Vec::new(),
        by_go: HashMap::new(),
        result: json!({"version":1,"space":"prefab-root-local-unity","nodesVisited":0,"waterBodies":[],"contributors":[],"issues":[],"complete":true}),
    };
    if let Err(error) = inspection.hierarchy(root) {
        inspection.issue(root, error);
        return inspection.result;
    }
    inspection.bodies();
    inspection.result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_hierarchy_water_fixtures_match_without_reading_mesh_payloads() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/water-geometry-reference.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let records = &case["records"];
            let mut seen = Vec::new();
            let actual = inspect(case["root"].as_i64().unwrap(), |id| {
                seen.push(id);
                let record = records.get(id.to_string()).ok_or_else(|| id.to_string())?;
                Ok((record[0].as_i64().unwrap() as i32, record[1].clone()))
            });
            assert_eq!(actual, case["expected"], "{}", case["name"]);
            if case["name"] == "mesh_no_payload_read" {
                assert!(!seen.contains(&999));
            }
        }
    }
}
