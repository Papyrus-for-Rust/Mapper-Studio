//! Decode captured node/helper components without loading any additional assets.
use super::*;
use papyrus_terrain::{environment, powerline_wires};
use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct WireTemplate {
    component: i64,
    local_to_root: [[f32; 4]; 4],
    wire: String,
    maximum_distance: f32,
    connections: Option<Vec<LocalConnection>>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Vector {
    x: f32,
    y: f32,
    z: f32,
}
impl Vector {
    fn array(&self) -> [f32; 3] {
        [self.x, self.y, self.z]
    }
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalConnection {
    #[serde(rename = "inOffset")]
    incoming: Vector,
    #[serde(rename = "outOffset")]
    outgoing: Vector,
    radius: f32,
    hidden: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pointer {
    #[serde(rename = "m_FileID")]
    file: i32,
    #[serde(rename = "m_PathID")]
    path: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    guid: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NodeFields {
    #[serde(rename = "WirePrefab")]
    wire: Reference,
    #[serde(rename = "MaxDistance")]
    maximum_distance: f32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HelperFields {
    connections: Vec<LocalConnection>,
    showgizmo: Value,
}

#[derive(Debug, Serialize)]
pub struct PlacedWireNode {
    instance: u64,
    component: i64,
    root: String,
    wire: String,
    position: [f32; 3],
    maximum_distance: f32,
    connections: Option<Vec<powerline_wires::Connection>>,
}
impl PlacedWireNode {
    pub fn input(&self) -> powerline_wires::Node<'_> {
        powerline_wires::Node {
            instance: self.instance,
            component: self.component,
            root: &self.root,
            wire: &self.wire,
            position: self.position,
            maximum_distance: self.maximum_distance,
            connections: self.connections.as_deref(),
        }
    }
}
fn flag(value: &Value) -> Result<bool, String> {
    match value {
        Value::Bool(value) => Ok(*value),
        Value::Number(value) if value.as_u64() == Some(0) => Ok(false),
        Value::Number(value) if value.as_u64() == Some(1) => Ok(true),
        _ => Err("Invalid wire component flag".into()),
    }
}
fn component(row: &RuntimeComponent, name: &str) -> bool {
    row.component.as_array().is_some_and(|owner| {
        owner.len() == 3 && owner[0] == "Assembly-CSharp" && owner[1] == "" && owner[2] == name
    })
}
fn fields(row: &RuntimeComponent) -> Result<Value, String> {
    let mut fields: serde_json::Map<String, Value> = serde_json::from_str(
        row.fields
            .as_ref()
            .ok_or("Missing wire component fields")?
            .get(),
    )
    .map_err(|error| error.to_string())?;
    let object: Pointer = serde_json::from_value(
        fields
            .remove("m_GameObject")
            .ok_or("Missing wire object reference")?,
    )
    .map_err(|error| error.to_string())?;
    let script: Pointer = serde_json::from_value(
        fields
            .remove("m_Script")
            .ok_or("Missing wire script reference")?,
    )
    .map_err(|error| error.to_string())?;
    if object.file != 0
        || object.path != row.game_object
        || !(0..=4096).contains(&script.file)
        || script.path == 0
    {
        return Err("Wire component identity differs from its runtime row".into());
    }
    flag(
        &fields
            .remove("m_Enabled")
            .ok_or("Missing wire component enabled flag")?,
    )?;
    if !fields.remove("m_Name").is_some_and(|value| {
        value
            .as_str()
            .is_some_and(|name| name.len() <= 1024 && !name.chars().any(char::is_control))
    }) {
        return Err("Invalid wire component name".into());
    }
    Ok(Value::Object(fields))
}
pub(crate) fn templates(
    rows: &[RuntimeComponent],
    resources: &BTreeMap<String, String>,
) -> Result<Vec<WireTemplate>, String> {
    validate_runtime(rows)?;
    let mut helpers = BTreeMap::new();
    for row in rows
        .iter()
        .filter(|row| component(row, "PowerLineWireConnectionHelper"))
    {
        helpers.entry(row.game_object).or_insert(row);
    }
    let mut output = Vec::new();
    for node in rows
        .iter()
        .filter(|row| row.hierarchy_active_without_root && component(row, "PowerlineNode"))
    {
        if node.local_to_root[3] != [0., 0., 0., 1.] {
            return Err("Wire node transform is not affine".into());
        }
        let values: NodeFields =
            serde_json::from_value(fields(node)?).map_err(|error| error.to_string())?;
        if !resources.contains_key(&values.wire.guid)
            || !values.maximum_distance.is_finite()
            || values.maximum_distance.abs() > 10000.
        {
            return Err("Invalid or unresolved wire node parameters".into());
        }
        // GetComponent selects the first helper on this object, including a
        // disabled component. Awake registration follows active hierarchy state.
        let helper = helpers.get(&node.game_object);
        let connections = helper
            .map(|helper| -> Result<Vec<LocalConnection>, String> {
                if helper.local_to_root != node.local_to_root
                    || !helper.hierarchy_active_without_root
                {
                    return Err("Wire helper transform differs from its node".into());
                }
                let fields = fields(helper)?;
                if fields["connections"]
                    .as_array()
                    .is_none_or(|values| values.len() > 256)
                {
                    return Err("Wire helper connection roster exceeds its bound".into());
                }
                let helper: HelperFields =
                    serde_json::from_value(fields).map_err(|error| error.to_string())?;
                flag(&helper.showgizmo)?;
                for connection in &helper.connections {
                    if connection
                        .incoming
                        .array()
                        .iter()
                        .chain(&connection.outgoing.array())
                        .any(|value| !value.is_finite() || value.abs() > 10000.)
                        || !connection.radius.is_finite()
                        || connection.radius.abs() > 1000.
                    {
                        return Err("Invalid authored wire connection".into());
                    }
                    flag(&connection.hidden)?;
                }
                Ok(helper.connections)
            })
            .transpose()?;
        output.push(WireTemplate {
            component: node.object_id,
            local_to_root: node.local_to_root,
            wire: values.wire.guid,
            maximum_distance: values.maximum_distance,
            connections,
        });
    }
    Ok(output)
}
impl Source {
    pub fn wire_templates(
        &self,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<BTreeMap<String, Vec<WireTemplate>>>, String> {
        let mut output = BTreeMap::new();
        for (path, prefab) in &self.document.prefabs {
            if cancelled() {
                return Ok(None);
            }
            let placement: road_object_source::Prefab =
                serde_json::from_str(prefab.placement.get()).map_err(|error| error.to_string())?;
            for name in ["PowerlineNode", "PowerLineWireConnectionHelper"] {
                let count = prefab
                    .runtime
                    .iter()
                    .filter(|row| row.hierarchy_active_without_root && component(row, name))
                    .count();
                if count != placement.components.get(name).copied().unwrap_or(0) {
                    return Err("Wire component coverage differs from its prefab roster".into());
                }
            }
            output.insert(
                path.clone(),
                templates(&prefab.runtime, &self.document.wire_resources)?,
            );
        }
        Ok((!cancelled()).then_some(output))
    }
}
impl WireTemplate {
    pub(crate) fn wire_resource(&self) -> &str {
        &self.wire
    }
    /// `root` is the actual scene transform-root name used by registration.
    /// The caller retains global spawn order when collecting the returned nodes.
    pub fn place(
        &self,
        instance: u64,
        root: &str,
        pose: &environment::Transform,
    ) -> Result<PlacedWireNode, String> {
        if root.is_empty() || root.len() > 1024 || root.chars().any(char::is_control) {
            return Err("Invalid placed wire root identity".into());
        }
        if pose.scale.iter().any(|value| !value.is_finite() || *value == 0.) {
            return Err("Invalid placed wire root scale".into());
        }
        environment::Transform {
            // Map records allow every finite nonzero scale. Preserve tiny root
            // scales for point transforms while reusing the pose validator.
            scale: pose.scale.map(|value| value.abs().max(0.000001)),
            ..*pose
        }
        .validate()?;
        let magnitude = pose
            .rotation
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt();
        let rotation = pose.rotation.map(|value| value / magnitude);
        let point = |local: [f32; 3]| -> Result<[f32; 3], String> {
            let local: [f32; 3] = std::array::from_fn(|row| {
                self.local_to_root[row][3]
                    + (0..3)
                        .map(|column| self.local_to_root[row][column] * local[column])
                        .sum::<f32>()
            });
            let offset = environment::rotate(
                rotation,
                std::array::from_fn(|axis| local[axis] * pose.scale[axis]),
            );
            let world: [f32; 3] = std::array::from_fn(|axis| pose.position[axis] + offset[axis]);
            if world
                .iter()
                .any(|value| !value.is_finite() || value.abs() > 1_000_000.)
            {
                return Err("Placed wire endpoint exceeds coordinate bounds".into());
            }
            Ok(world)
        };
        let connections = self
            .connections
            .as_ref()
            .map(|connections| {
                connections
                    .iter()
                    .map(|connection| {
                        Ok(powerline_wires::Connection {
                            incoming: point(connection.incoming.array())?,
                            outgoing: point(connection.outgoing.array())?,
                            radius: connection.radius,
                            hidden: flag(&connection.hidden)?,
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()
            })
            .transpose()?;
        Ok(PlacedWireNode {
            instance,
            component: self.component,
            root: root.into(),
            wire: self.wire.clone(),
            position: point([0.; 3])?,
            maximum_distance: self.maximum_distance,
            connections,
        })
    }
}

#[cfg(test)]
mod tests;
