//! Versioned world-space position, height, and mask graphs for native terrain.
use crate::native_layers::Layers;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

pub const VERSION: u32 = 1;
pub const MAX_NODES: usize = 64;
const MAX_GROUPS: usize = 32;
const MAX_EVALUATED_NODES: usize = 256;
pub const MAX_HEIGHTMAP_EDGE: usize = 512;
pub const MAX_HEIGHTMAP_PIXELS: usize = MAX_HEIGHTMAP_EDGE * MAX_HEIGHTMAP_EDGE;
const MAX_WORK: u64 = 2_000_000_000;
const WORLD_SLOT: usize = MAX_EVALUATED_NODES;
const POSITION_LIMIT: f32 = 1_000_000.;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Graph {
    pub version: u32,
    pub output: u32,
    pub nodes: Vec<Node>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub heightmaps: BTreeMap<u32, Heightmap>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<NodeGroup>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub definitions: BTreeMap<u32, GroupDefinition>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeGroup {
    pub name: String,
    pub nodes: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupDefinition {
    pub name: String,
    pub inputs: Vec<GroupInput>,
    pub output: u32,
    pub nodes: Vec<Node>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupInput {
    pub name: String,
    pub port: Port,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Heightmap {
    pub label: String,
    pub dimensions: [u16; 2],
    /// Image order, with the north edge in the first row.
    #[serde(with = "heightmap_samples")]
    pub samples: Arc<[u16]>,
}

mod heightmap_samples {
    use super::{Arc, MAX_HEIGHTMAP_PIXELS};
    use serde::{Deserialize, Deserializer, Serializer, de};
    use std::fmt::Write;

    // Four hexadecimal digits per sample keep pretty-printed recipe size
    // bounded while preserving the exact unsigned values.
    pub fn serialize<S: Serializer>(samples: &[u16], serializer: S) -> Result<S::Ok, S::Error> {
        let mut encoded = String::with_capacity(samples.len() * 4);
        for sample in samples {
            write!(encoded, "{sample:04x}").expect("writing into a String");
        }
        serializer.serialize_str(&encoded)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(input: D) -> Result<Arc<[u16]>, D::Error> {
        let encoded = String::deserialize(input)?;
        if encoded.len() > MAX_HEIGHTMAP_PIXELS * 4 || encoded.len() % 4 != 0 || !encoded.is_ascii()
        {
            return Err(de::Error::custom("Invalid heightmap sample data"));
        }
        let mut samples = Vec::with_capacity(encoded.len() / 4);
        for start in (0..encoded.len()).step_by(4) {
            samples.push(
                u16::from_str_radix(&encoded[start..start + 4], 16).map_err(de::Error::custom)?,
            );
        }
        Ok(samples.into())
    }
}

impl Heightmap {
    fn valid(&self) -> bool {
        let [w, h] = self.dimensions.map(usize::from);
        (2..=MAX_HEIGHTMAP_EDGE).contains(&w)
            && (2..=MAX_HEIGHTMAP_EDGE).contains(&h)
            && self.samples.len() == w * h
            && self.label.len() <= 128
            && !self.label.is_empty()
    }

    fn sample(&self, uv: [f32; 2]) -> f32 {
        let [w, h] = self.dimensions.map(usize::from);
        let x = uv[0].clamp(0., 1.) * (w - 1) as f32;
        let z = (1. - uv[1].clamp(0., 1.)) * (h - 1) as f32;
        let [i, j] = [x as usize, z as usize];
        let [next_x, next_z] = [(i + 1).min(w - 1), (j + 1).min(h - 1)];
        let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
        let row = |z| {
            lerp(
                f32::from(self.samples[z * w + i]),
                f32::from(self.samples[z * w + next_x]),
                x.fract(),
            )
        };
        lerp(row(j), row(next_z), z.fract()) / f32::from(u16::MAX)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub id: u32,
    pub op: Operation,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Group {
        definition: u32,
        ports: [Option<Port>; 3],
        links: [u32; 3],
        output: Port,
    },
    GroupInput {
        slot: u8,
        port: Port,
    },
    NativeHeight,
    Height {
        metres: f32,
    },
    Heightmap {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<u32>,
        asset: u32,
        center: [f32; 2],
        size: [f32; 2],
        low: f32,
        high: f32,
    },
    Mask {
        coverage: f32,
    },
    WorldPosition,
    TransformPosition {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<u32>,
        offset: [f32; 2],
        scale: [f32; 2],
        rotation: f32,
    },
    WarpPosition {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<u32>,
        x_mask: u32,
        z_mask: u32,
        amount: [f32; 2],
    },
    MirrorPosition {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<u32>,
        center: [f32; 2],
        axes: [bool; 2],
    },
    Ellipse {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<u32>,
        center: [f32; 2],
        radii: [f32; 2],
        rotation: f32,
        feather: f32,
    },
    Gradient {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<u32>,
        start: [f32; 2],
        end: [f32; 2],
    },
    Noise {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<u32>,
        wavelength: f32,
        octaves: u8,
        persistence: f32,
        mode: NoiseMode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seed: Option<u32>,
    },
    Cellular {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<u32>,
        wavelength: f32,
        jitter: f32,
        mode: CellularMode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seed: Option<u32>,
    },
    Rings {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<u32>,
        center: [f32; 2],
        spacing: f32,
        width: f32,
        feather: f32,
    },
    CircleDistance {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<u32>,
        center: [f32; 2],
        radius: f32,
    },
    BoxDistance {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<u32>,
        center: [f32; 2],
        half_size: [f32; 2],
        rotation: f32,
    },
    CapsuleDistance {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<u32>,
        start: [f32; 2],
        end: [f32; 2],
        radius: f32,
    },
    CombineDistance {
        left: u32,
        right: u32,
        mode: DistanceMode,
        smoothing: f32,
    },
    OffsetDistance {
        distance: u32,
        metres: f32,
    },
    DistanceMask {
        distance: u32,
        feather: f32,
    },
    Combine {
        left: u32,
        right: u32,
        mode: MaskMode,
    },
    Invert {
        input: u32,
    },
    Remap {
        input: u32,
        low: f32,
        high: f32,
    },
    ElevationMask {
        height: u32,
        low: f32,
        high: f32,
    },
    Blend {
        low: u32,
        high: u32,
        mask: u32,
    },
    Raise {
        height: u32,
        mask: u32,
        metres: f32,
    },
    Terrace {
        height: u32,
        step: f32,
        softness: f32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaskMode {
    Union,
    Intersect,
    Subtract,
    Multiply,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DistanceMode {
    Union,
    Intersect,
    Subtract,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoiseMode {
    Fractal,
    Ridged,
    Billow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellularMode {
    Centers,
    Borders,
    Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Port {
    Position,
    Height,
    Mask,
    Distance,
}

#[derive(Clone, Copy)]
pub struct Input {
    pub name: &'static str,
    pub port: Port,
    pub source: Option<u32>,
    pub optional: bool,
}

enum Connection<'a> {
    Required(&'static str, Port, &'a mut u32),
    Position(&'a mut Option<u32>),
}

impl Connection<'_> {
    fn describe(self) -> Input {
        match self {
            Self::Required(name, port, source) => Input {
                name,
                port,
                source: (*source != 0).then_some(*source),
                optional: false,
            },
            Self::Position(source) => Input {
                name: "Position",
                port: Port::Position,
                source: *source,
                optional: true,
            },
        }
    }
}

impl Operation {
    pub fn port(&self) -> Port {
        match self {
            Self::Group { output, .. } => *output,
            Self::GroupInput { port, .. } => *port,
            Self::NativeHeight
            | Self::Height { .. }
            | Self::Heightmap { .. }
            | Self::Blend { .. }
            | Self::Raise { .. }
            | Self::Terrace { .. } => Port::Height,
            Self::WorldPosition
            | Self::TransformPosition { .. }
            | Self::WarpPosition { .. }
            | Self::MirrorPosition { .. } => Port::Position,
            Self::CircleDistance { .. }
            | Self::BoxDistance { .. }
            | Self::CapsuleDistance { .. }
            | Self::CombineDistance { .. }
            | Self::OffsetDistance { .. } => Port::Distance,
            _ => Port::Mask,
        }
    }

    pub fn inputs(&self) -> [Option<Input>; 3] {
        let mut operation = *self;
        operation
            .connections()
            .map(|input| input.map(Connection::describe))
    }

    /// Use the same physical input slots in authoring and compilation.
    pub fn set_input(&mut self, slot: usize, source: Option<u32>) -> bool {
        match self.connections().into_iter().nth(slot).flatten() {
            Some(Connection::Required(_, _, input)) => *input = source.unwrap_or(0),
            Some(Connection::Position(input)) => *input = source,
            None => return false,
        }
        true
    }

    fn connections(&mut self) -> [Option<Connection<'_>>; 3] {
        use Connection::{Position, Required};
        use Port::{Distance, Height, Mask};
        match self {
            Self::Group { ports, links, .. } => {
                let [a, b, c] = links;
                [
                    ports[0].map(|port| Required("Input 1", port, a)),
                    ports[1].map(|port| Required("Input 2", port, b)),
                    ports[2].map(|port| Required("Input 3", port, c)),
                ]
            }
            Self::TransformPosition { position, .. }
            | Self::MirrorPosition { position, .. }
            | Self::Ellipse { position, .. }
            | Self::Gradient { position, .. }
            | Self::Noise { position, .. }
            | Self::Cellular { position, .. }
            | Self::Rings { position, .. }
            | Self::CircleDistance { position, .. }
            | Self::BoxDistance { position, .. }
            | Self::CapsuleDistance { position, .. }
            | Self::Heightmap { position, .. } => [Some(Position(position)), None, None],
            Self::WarpPosition {
                position,
                x_mask,
                z_mask,
                ..
            } => [
                Some(Position(position)),
                Some(Required("X mask", Mask, x_mask)),
                Some(Required("Z mask", Mask, z_mask)),
            ],
            Self::Combine { left, right, .. } => [
                Some(Required("First mask", Mask, left)),
                Some(Required("Second mask", Mask, right)),
                None,
            ],
            Self::CombineDistance { left, right, .. } => [
                Some(Required("First field", Distance, left)),
                Some(Required("Second field", Distance, right)),
                None,
            ],
            Self::OffsetDistance { distance, .. } | Self::DistanceMask { distance, .. } => {
                [Some(Required("Distance", Distance, distance)), None, None]
            }
            Self::Invert { input } | Self::Remap { input, .. } => {
                [Some(Required("Mask", Mask, input)), None, None]
            }
            Self::ElevationMask { height, .. } | Self::Terrace { height, .. } => {
                [Some(Required("Height", Height, height)), None, None]
            }
            Self::Blend { low, high, mask } => [
                Some(Required("Low height", Height, low)),
                Some(Required("High height", Height, high)),
                Some(Required("Mask", Mask, mask)),
            ],
            Self::Raise { height, mask, .. } => [
                Some(Required("Height", Height, height)),
                Some(Required("Mask", Mask, mask)),
                None,
            ],
            _ => [None, None, None],
        }
    }

    fn valid(&self) -> bool {
        let bounded = |v: f32, low: f32, high: f32| (low..=high).contains(&v);
        let point = |p: [f32; 2]| p.into_iter().all(|v| bounded(v, -24_000., 24_000.));
        let unit = |v| bounded(v, 0., 1.);
        match *self {
            Self::Height { metres } => bounded(metres, -500., 500.),
            Self::Heightmap {
                center,
                size,
                low,
                high,
                ..
            } => {
                point(center)
                    && size.iter().all(|&v| bounded(v, 1., 24_000.))
                    && bounded(low, -500., 500.)
                    && bounded(high, low, 500.)
            }
            Self::Mask { coverage } => unit(coverage),
            Self::TransformPosition {
                offset,
                scale,
                rotation,
                ..
            } => {
                point(offset)
                    && scale.into_iter().all(|v| bounded(v.abs(), 0.001, 64.))
                    && bounded(rotation, -360., 360.)
            }
            Self::WarpPosition { amount, .. } => point(amount),
            Self::MirrorPosition { center, axes, .. } => {
                point(center) && axes.into_iter().any(|v| v)
            }
            Self::Ellipse {
                center,
                radii,
                rotation,
                feather,
                ..
            } => {
                point(center)
                    && radii.into_iter().all(|v| bounded(v, 1., 24_000.))
                    && bounded(rotation, -360., 360.)
                    && unit(feather)
            }
            Self::Gradient { start, end, .. } => {
                point(start) && point(end) && (end[0] - start[0]).hypot(end[1] - start[1]) >= 1.
            }
            Self::Noise {
                wavelength,
                octaves,
                persistence,
                ..
            } => {
                bounded(wavelength, 2., 24_000.) && (1..=8).contains(&octaves) && unit(persistence)
            }
            Self::Cellular {
                wavelength, jitter, ..
            } => bounded(wavelength, 2., 24_000.) && unit(jitter),
            Self::Rings {
                center,
                spacing,
                width,
                feather,
                ..
            } => {
                point(center)
                    && bounded(spacing, 2., 24_000.)
                    && bounded(width, 0.1, spacing)
                    && bounded(feather, 0., width * 0.5)
            }
            Self::CircleDistance { center, radius, .. } => {
                point(center) && bounded(radius, 1., 24_000.)
            }
            Self::BoxDistance {
                center,
                half_size,
                rotation,
                ..
            } => {
                point(center)
                    && half_size.into_iter().all(|v| bounded(v, 1., 24_000.))
                    && bounded(rotation, -360., 360.)
            }
            Self::CapsuleDistance {
                start, end, radius, ..
            } => {
                point(start)
                    && point(end)
                    && (end[0] - start[0]).hypot(end[1] - start[1]) >= 1.
                    && bounded(radius, 1., 24_000.)
            }
            Self::CombineDistance { smoothing, .. } => bounded(smoothing, 0., 24_000.),
            Self::OffsetDistance { metres, .. } => bounded(metres, -24_000., 24_000.),
            Self::DistanceMask { feather, .. } => bounded(feather, 0., 24_000.),
            Self::Remap { low, high, .. } => unit(low) && unit(high) && high - low >= 0.0001,
            Self::ElevationMask { low, high, .. } => {
                bounded(low, -500., 500.) && bounded(high, -500., 500.) && high - low >= 0.01
            }
            Self::Raise { metres, .. } => bounded(metres, -1000., 1000.),
            Self::Terrace { step, softness, .. } => bounded(step, 0.1, 500.) && unit(softness),
            _ => true,
        }
    }
}

struct Step<'a> {
    node: &'a Node,
    inputs: [usize; 3],
    constants: [f32; 3],
    heightmap: Option<&'a Heightmap>,
}

struct Compiler<'a> {
    definitions: &'a BTreeMap<u32, GroupDefinition>,
    heightmaps: &'a BTreeMap<u32, Heightmap>,
    active_definitions: Vec<u32>,
    work: usize,
    schedule: Vec<Step<'a>>,
    shared_steps: HashMap<(*const Node, [usize; 3]), usize>,
}

struct Scope<'a> {
    nodes: &'a [Node],
    by_id: BTreeMap<u32, usize>,
    visiting: [bool; MAX_NODES],
    compiled: [Option<usize>; MAX_NODES],
    inputs: &'a [GroupInput],
    bindings: [usize; 3],
}

impl<'a> Scope<'a> {
    fn new(
        nodes: &'a [Node],
        inputs: &'a [GroupInput],
        bindings: [usize; 3],
    ) -> Result<Self, String> {
        if !(1..=MAX_NODES).contains(&nodes.len()) {
            return Err("Invalid terrain graph node count".into());
        }
        let mut by_id = BTreeMap::new();
        for (index, node) in nodes.iter().enumerate() {
            if node.id == 0 || by_id.insert(node.id, index).is_some() || !node.op.valid() {
                return Err(format!("Invalid terrain graph node {}", node.id));
            }
        }
        Ok(Self {
            nodes,
            by_id,
            visiting: [false; MAX_NODES],
            compiled: [None; MAX_NODES],
            inputs,
            bindings,
        })
    }
}

impl<'a> Compiler<'a> {
    fn visit(&mut self, scope: &mut Scope<'a>, index: usize) -> Result<usize, String> {
        if let Some(slot) = scope.compiled[index] {
            return Ok(slot);
        }
        let node = &scope.nodes[index];
        if scope.visiting[index] {
            return Err(format!("Terrain graph cycle at node {}", node.id));
        }
        self.work += 1;
        if self.work > MAX_EVALUATED_NODES * 8 {
            return Err("Terrain group expansion exceeds its work limit".into());
        }
        scope.visiting[index] = true;
        let mut inputs = [WORLD_SLOT; 3];
        for (slot, input) in inputs.iter_mut().zip(node.op.inputs()) {
            let Some(input) = input else { continue };
            let Some(id) = input.source else {
                if input.optional {
                    continue;
                }
                return Err(format!(
                    "Connect {} on terrain graph node {}",
                    input.name, node.id
                ));
            };
            let dependency = *scope.by_id.get(&id).ok_or_else(|| {
                format!("Terrain graph node {} refers to missing node {id}", node.id)
            })?;
            if scope.nodes[dependency].op.port() != input.port {
                return Err(format!(
                    "Terrain graph node {} has the wrong port type from node {id}",
                    node.id
                ));
            }
            *slot = self.visit(scope, dependency)?;
        }
        let slot = match node.op {
            Operation::Group { definition, .. } => self.instantiate(node, definition, inputs)?,
            Operation::GroupInput { slot, port } => {
                let slot = usize::from(slot);
                if scope.inputs.get(slot).map(|input| input.port) != Some(port) {
                    return Err("Invalid group input node".into());
                }
                scope.bindings[slot]
            }
            _ => self.emit(node, inputs)?,
        };
        scope.compiled[index] = Some(slot);
        scope.visiting[index] = false;
        Ok(slot)
    }

    fn instantiate(&mut self, node: &Node, id: u32, inputs: [usize; 3]) -> Result<usize, String> {
        if self.active_definitions.contains(&id) {
            return Err(format!("Recursive terrain group {id}"));
        }
        let definition = self
            .definitions
            .get(&id)
            .ok_or("Missing terrain group definition")?;
        let expected = definition.instance(id)?;
        let Operation::Group {
            ports,
            links,
            output,
            ..
        } = node.op
        else {
            unreachable!()
        };
        let Operation::Group {
            ports: wanted,
            output: result,
            ..
        } = expected
        else {
            unreachable!()
        };
        if ports != wanted
            || output != result
            || ports
                .iter()
                .zip(links)
                .any(|(port, link)| port.is_none() && link != 0)
        {
            return Err("Terrain group instance port signature differs from its definition".into());
        }
        let mut scope = Scope::new(&definition.nodes, &definition.inputs, inputs)?;
        let index = *scope
            .by_id
            .get(&definition.output)
            .ok_or("Missing terrain group output")?;
        self.active_definitions.push(id);
        let slot = self.visit(&mut scope, index)?;
        self.active_definitions.pop();
        Ok(slot)
    }

    fn emit(&mut self, node: &'a Node, inputs: [usize; 3]) -> Result<usize, String> {
        // A member of a shared definition keeps the same parameters and seed
        // derivation in each instance. Its resolved input slots identify the
        // instances that can reuse this calculation.
        let shared =
            (!self.active_definitions.is_empty()).then_some((std::ptr::from_ref(node), inputs));
        if let Some(slot) = shared.and_then(|key| self.shared_steps.get(&key)) {
            return Ok(*slot);
        }
        if self.schedule.len() == MAX_EVALUATED_NODES {
            return Err("Terrain group expansion exceeds 256 evaluated nodes".into());
        }
        let constants = match node.op {
            Operation::Ellipse { rotation, .. }
            | Operation::TransformPosition { rotation, .. }
            | Operation::BoxDistance { rotation, .. } => {
                let (sin, cos) = rotation.to_radians().sin_cos();
                [sin, cos, 0.]
            }
            Operation::Gradient { start, end, .. }
            | Operation::CapsuleDistance { start, end, .. } => {
                let [x, z] = [end[0] - start[0], end[1] - start[1]];
                [x, z, 1. / (x * x + z * z)]
            }
            _ => [0.; 3],
        };
        let slot = self.schedule.len();
        self.schedule.push(Step {
            node,
            inputs,
            constants,
            heightmap: match node.op {
                Operation::Heightmap { asset, .. } => Some(
                    self.heightmaps
                        .get(&asset)
                        .ok_or_else(|| format!("Choose a heightmap for node {}", node.id))?,
                ),
                _ => None,
            },
        });
        if let Some(key) = shared {
            self.shared_steps.insert(key, slot);
        }
        Ok(slot)
    }
}

fn retain_output_steps(schedule: &mut Vec<Step<'_>>, output: usize) {
    let mut used = [false; MAX_EVALUATED_NODES + 1];
    used[output] = true;
    for (index, step) in schedule.iter().enumerate().rev() {
        if used[index] {
            for input in step.inputs {
                used[input] = true;
            }
        }
    }
    let mut remap = [WORLD_SLOT; MAX_EVALUATED_NODES + 1];
    let mut index = 0;
    let mut next = 0;
    schedule.retain_mut(|step| {
        let retain = used[index];
        if retain {
            step.inputs = step.inputs.map(|slot| remap[slot]);
            remap[index] = next;
            next += 1;
        }
        index += 1;
        retain
    });
}

impl GroupDefinition {
    pub fn instance(&self, id: u32) -> Result<Operation, String> {
        let output = self
            .nodes
            .iter()
            .find(|node| node.id == self.output)
            .ok_or("Missing terrain group output")?
            .op
            .port();
        if self.inputs.len() > 3 {
            return Err("Terrain groups support up to three inputs".into());
        }
        Ok(Operation::Group {
            definition: id,
            ports: std::array::from_fn(|i| self.inputs.get(i).map(|input| input.port)),
            links: [0; 3],
            output,
        })
    }
}

impl Graph {
    pub fn evaluation_key(&self) -> impl Serialize + '_ {
        #[derive(Serialize)]
        struct Evaluation<'a> {
            version: u32,
            output: u32,
            nodes: &'a [Node],
            #[serde(skip_serializing_if = "BTreeMap::is_empty")]
            heightmaps: &'a BTreeMap<u32, Heightmap>,
            #[serde(skip_serializing_if = "BTreeMap::is_empty")]
            definitions: &'a BTreeMap<u32, GroupDefinition>,
        }
        Evaluation {
            version: self.version,
            output: self.output,
            nodes: &self.nodes,
            heightmaps: &self.heightmaps,
            definitions: &self.definitions,
        }
    }

    /// Duplicate a group as an independently editable branch. Internal edges
    /// are remapped, while incoming edges still refer to their outside sources.
    pub fn duplicate_group(&mut self, index: usize) -> Result<usize, String> {
        let group = self.groups.get(index).ok_or("Missing node group")?;
        if self.nodes.len() + group.nodes.len() > MAX_NODES {
            return Err("Duplicating this group exceeds the node limit".into());
        }
        let ids = (1..).filter(|id| !self.nodes.iter().any(|node| node.id == *id));
        let map: BTreeMap<_, _> = group.nodes.iter().copied().zip(ids).collect();
        let mut copies = Vec::with_capacity(group.nodes.len());
        for &old in &group.nodes {
            let mut node = self
                .nodes
                .iter()
                .find(|node| node.id == old)
                .ok_or("Missing group member")?
                .clone();
            node.id = map[&old];
            for (slot, input) in node.op.inputs().into_iter().enumerate() {
                if let Some(&source) = input
                    .and_then(|input| input.source)
                    .and_then(|id| map.get(&id))
                {
                    node.op.set_input(slot, Some(source));
                }
            }
            copies.push(node);
        }
        let mut name: String = group.name.chars().take(42).collect();
        name.push_str(" copy");
        self.groups.push(NodeGroup {
            name,
            nodes: copies.iter().map(|node| node.id).collect(),
        });
        self.nodes.extend(copies);
        Ok(self.groups.len() - 1)
    }

    /// Replace a frame with an instance of a shared, typed definition.
    /// The external output keeps its node ID so existing consumers stay wired.
    pub fn link_group(&mut self, index: usize, selected: u32) -> Result<u32, String> {
        let frame = self.groups.get(index).ok_or("Missing node group")?;
        if self.definitions.len() == MAX_GROUPS {
            return Err("Linked group limit reached".into());
        }
        let contains = |id| frame.nodes.contains(&id);
        let mut outputs: std::collections::BTreeSet<_> = self
            .nodes
            .iter()
            .filter(|node| !contains(node.id))
            .flat_map(|node| {
                node.op
                    .inputs()
                    .into_iter()
                    .flatten()
                    .filter_map(|input| input.source)
            })
            .filter(|&id| contains(id))
            .collect();
        if contains(self.output) {
            outputs.insert(self.output);
        }
        if outputs.len() > 1 {
            return Err(
                "A linked group can expose one output. Disconnect extra outputs first.".into(),
            );
        }
        let output = outputs.into_iter().next().unwrap_or(selected);
        if !contains(output) {
            return Err("Select a member to use as the group output".into());
        }
        let mut nodes: Vec<_> = self
            .nodes
            .iter()
            .filter(|node| contains(node.id))
            .cloned()
            .collect();
        let mut ports: Vec<(u32, GroupInput)> = Vec::new();
        for node in &nodes {
            for input in node.op.inputs().into_iter().flatten() {
                let Some(source) = input.source.filter(|&id| !contains(id)) else {
                    continue;
                };
                if !ports.iter().any(|(id, _)| *id == source) {
                    if ports.len() == 3 {
                        return Err("A linked group can expose up to three inputs".into());
                    }
                    ports.push((
                        source,
                        GroupInput {
                            name: input.name.into(),
                            port: input.port,
                        },
                    ));
                }
            }
        }
        if nodes.len() + ports.len() > MAX_NODES
            || self
                .definitions
                .values()
                .map(|group| group.nodes.len())
                .sum::<usize>()
                + nodes.len()
                + ports.len()
                > MAX_EVALUATED_NODES
        {
            return Err("Linked group exceeds the definition node limit".into());
        }
        let placeholders: Vec<_> = (1..)
            .filter(|&id| !contains(id))
            .take(ports.len())
            .collect();
        for node in &mut nodes {
            for (slot, input) in node.op.inputs().into_iter().enumerate() {
                if let Some(index) = input
                    .and_then(|input| input.source)
                    .and_then(|source| ports.iter().position(|(id, _)| *id == source))
                {
                    node.op.set_input(slot, Some(placeholders[index]));
                }
            }
        }
        nodes.extend(ports.iter().enumerate().map(|(slot, (_, input))| Node {
            id: placeholders[slot],
            op: Operation::GroupInput {
                slot: slot as u8,
                port: input.port,
            },
        }));
        let id = (1..).find(|id| !self.definitions.contains_key(id)).unwrap();
        let definition = GroupDefinition {
            name: frame.name.clone(),
            inputs: ports.iter().map(|(_, input)| input.clone()).collect(),
            output,
            nodes,
        };
        let mut instance = definition.instance(id)?;
        for (slot, (source, _)) in ports.into_iter().enumerate() {
            instance.set_input(slot, Some(source));
        }
        self.nodes.retain(|node| !contains(node.id));
        self.nodes.push(Node {
            id: output,
            op: instance,
        });
        self.definitions.insert(id, definition);
        self.groups.remove(index);
        Ok(output)
    }

    pub fn read(reader: impl std::io::Read) -> Result<Self, String> {
        use std::io::Read;
        const LIMIT: u64 = 2 * 1024 * 1024;
        let mut bytes = Vec::new();
        reader
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > LIMIT {
            return Err("Terrain graph file exceeds 2 MiB".into());
        }
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())
    }

    fn compile(&self, size: u32) -> Result<Vec<Step<'_>>, String> {
        let (resolution, _) = crate::native_layers::dimensions(size)?;
        if self.version != VERSION || !(1..=MAX_NODES).contains(&self.nodes.len()) {
            return Err("Unsupported terrain graph version or node count".into());
        }
        if self.heightmaps.len() > MAX_NODES
            || self
                .heightmaps
                .iter()
                .any(|(&id, image)| id == 0 || !image.valid())
            || self
                .heightmaps
                .values()
                .map(|image| image.samples.len())
                .sum::<usize>()
                > MAX_HEIGHTMAP_PIXELS
        {
            return Err("Invalid heightmap data or recipe pixel limit exceeded".into());
        }
        let mut root = Scope::new(&self.nodes, &[], [WORLD_SLOT; 3])?;
        let mut grouped = std::collections::BTreeSet::new();
        if self.groups.len() > MAX_NODES
            || self.groups.iter().any(|group| {
                group.name.trim().is_empty()
                    || group.name.chars().count() > 48
                    || group.nodes.is_empty()
                    || group
                        .nodes
                        .iter()
                        .any(|id| !root.by_id.contains_key(id) || !grouped.insert(*id))
            })
        {
            return Err("Invalid node group name or membership".into());
        }
        if self.definitions.len() > MAX_GROUPS
            || self
                .definitions
                .values()
                .map(|group| group.nodes.len())
                .sum::<usize>()
                > MAX_EVALUATED_NODES
        {
            return Err("Terrain group definitions exceed their storage limit".into());
        }
        let compiler = || Compiler {
            definitions: &self.definitions,
            heightmaps: &self.heightmaps,
            active_definitions: Vec::new(),
            work: 0,
            schedule: Vec::new(),
            shared_steps: HashMap::new(),
        };
        for (&id, definition) in &self.definitions {
            if id == 0
                || !valid_group_name(&definition.name)
                || definition
                    .inputs
                    .iter()
                    .any(|input| !valid_group_name(&input.name))
            {
                return Err("Invalid linked group name or input".into());
            }
            definition.instance(id)?;
            let mut checker = compiler();
            checker.active_definitions.push(id);
            let mut scope = Scope::new(&definition.nodes, &definition.inputs, [WORLD_SLOT; 3])?;
            for i in 0..definition.nodes.len() {
                checker.visit(&mut scope, i)?;
            }
        }
        let output = *root
            .by_id
            .get(&self.output)
            .ok_or("Missing terrain graph output")?;
        if self.nodes[output].op.port() != Port::Height {
            return Err("Terrain graph output requires a height port".into());
        }
        let mut compiler = compiler();
        let output = compiler.visit(&mut root, output)?;
        for i in 0..self.nodes.len() {
            compiler.visit(&mut root, i)?;
        }
        retain_output_steps(&mut compiler.schedule, output);
        let weight = compiler
            .schedule
            .iter()
            .map(|step| match step.node.op {
                Operation::Noise { octaves, .. } => u64::from(octaves) * 3,
                Operation::Cellular { .. } => 25,
                Operation::CircleDistance { .. } => 3,
                Operation::BoxDistance { .. } | Operation::CapsuleDistance { .. } => 5,
                Operation::CombineDistance { .. } | Operation::DistanceMask { .. } => 2,
                Operation::Heightmap { .. } => 4,
                _ => 1,
            })
            .sum::<u64>();
        if weight * (resolution as u64).pow(2) > MAX_WORK {
            return Err("Terrain graph exceeds the work limit for this map size".into());
        }
        Ok(compiler.schedule)
    }

    pub fn validate(&self, size: u32) -> Result<(), String> {
        self.compile(size).map(|_| ())
    }

    /// Evaluate into a private replacement grid, publishing only on success.
    pub fn render(
        &self,
        layers: &Layers,
        maximum_bytes: usize,
        pool: &rayon::ThreadPool,
        cancelled: impl Fn() -> bool + Sync,
    ) -> Result<Option<Vec<i16>>, String> {
        let schedule = self.compile(layers.size)?;
        let (n, _) = crate::native_layers::dimensions(layers.size)?;
        if layers.height_resolution != n || layers.height.len() != n * n {
            return Err("Terrain graph input dimensions differ from the recipe".into());
        }
        if n * n * size_of::<i16>() > maximum_bytes {
            return Err("Terrain graph exceeds its height buffer allowance".into());
        }
        if cancelled() {
            return Ok(None);
        }
        let mut output = Vec::new();
        output
            .try_reserve_exact(n * n)
            .map_err(|err| err.to_string())?;
        output.resize(n * n, 0);
        let passthrough = matches!(
            schedule[schedule.len() - 1].node.op,
            Operation::NativeHeight
        );
        let half = layers.size as f32 * 0.5;
        let spacing = layers.size as f32 / (n - 1) as f32;
        let done = pool.install(|| {
            output
                .par_chunks_mut(n)
                .zip(layers.height.par_chunks(n))
                .enumerate()
                .all(|(z, (target, native))| {
                    let world_z = z as f32 * spacing - half;
                    let mut values = [[0.; 2]; MAX_EVALUATED_NODES + 1];
                    for (x, (target, &source)) in target.iter_mut().zip(native).enumerate() {
                        if x % 256 == 0 && cancelled() {
                            return false;
                        }
                        *target = if passthrough {
                            source
                        } else {
                            let position = [x as f32 * spacing - half, world_z];
                            let result = sample(
                                &schedule,
                                &mut values,
                                position,
                                source,
                                layers.terrain_seed,
                            );
                            crate::erosion::encode(result.clamp(-500., 500.))
                        };
                    }
                    true
                })
        });
        Ok((done && !cancelled()).then_some(output))
    }
}

fn smooth(t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    t * t * (3. - 2. * t)
}

fn valid_group_name(name: &str) -> bool {
    !name.trim().is_empty() && name.chars().count() <= 48 && !name.chars().any(char::is_control)
}

fn smooth_min(a: f32, b: f32, smoothing: f32) -> f32 {
    let closest = a.min(b);
    if smoothing == 0. {
        return closest;
    }
    let overlap = (smoothing - (a - b).abs()).max(0.) / smoothing;
    closest - smoothing * 0.25 * overlap * overlap
}

fn sample(
    schedule: &[Step<'_>],
    values: &mut [[f32; 2]; MAX_EVALUATED_NODES + 1],
    p: [f32; 2],
    source: i16,
    seed: u32,
) -> f32 {
    values[WORLD_SLOT] = p;
    for (i, step) in schedule.iter().enumerate() {
        let read = |slot: usize| values[step.inputs[slot]][0];
        let point = || values[step.inputs[0]];
        let bounded =
            |position: [f32; 2]| position.map(|v| v.clamp(-POSITION_LIMIT, POSITION_LIMIT));
        let scalar = match step.node.op {
            Operation::Group { .. } | Operation::GroupInput { .. } => {
                unreachable!("compiled group")
            }
            Operation::WorldPosition => {
                values[i] = p;
                continue;
            }
            Operation::TransformPosition { offset, scale, .. } => {
                let [x, z] = point();
                let [x, z] = [x * scale[0], z * scale[1]];
                let [sin, cos, _] = step.constants;
                values[i] = bounded([x * cos - z * sin + offset[0], x * sin + z * cos + offset[1]]);
                continue;
            }
            Operation::WarpPosition { amount, .. } => {
                let [x, z] = point();
                values[i] = bounded([
                    x + (read(1) * 2. - 1.) * amount[0],
                    z + (read(2) * 2. - 1.) * amount[1],
                ]);
                continue;
            }
            Operation::MirrorPosition { center, axes, .. } => {
                let p = point();
                values[i] = bounded(std::array::from_fn(|axis| {
                    if axes[axis] {
                        center[axis] + (p[axis] - center[axis]).abs()
                    } else {
                        p[axis]
                    }
                }));
                continue;
            }
            Operation::NativeHeight => crate::erosion::decode(source),
            Operation::Height { metres } => metres,
            Operation::Heightmap {
                center,
                size,
                low,
                high,
                ..
            } => {
                let p = point();
                let uv = std::array::from_fn(|i| (p[i] - center[i]) / size[i] + 0.5);
                low + (high - low) * step.heightmap.expect("compiled heightmap").sample(uv)
            }
            Operation::Mask { coverage } => coverage,
            Operation::Ellipse {
                center,
                radii,
                feather,
                ..
            } => {
                let p = point();
                let [sin, cos, _] = step.constants;
                let [x, z] = [p[0] - center[0], p[1] - center[1]];
                let distance =
                    ((x * cos + z * sin) / radii[0]).hypot((-x * sin + z * cos) / radii[1]);
                if feather == 0. {
                    u8::from(distance <= 1.) as f32
                } else {
                    smooth((1. - distance) / feather)
                }
            }
            Operation::Gradient { start, .. } => {
                let p = point();
                let [x, z, inverse] = step.constants;
                (((p[0] - start[0]) * x + (p[1] - start[1]) * z) * inverse).clamp(0., 1.)
            }
            Operation::Noise {
                wavelength,
                octaves,
                persistence,
                mode,
                seed: fixed,
                ..
            } => noise(
                point(),
                wavelength,
                octaves,
                persistence,
                mode,
                fixed.unwrap_or_else(|| hash(seed ^ step.node.id.wrapping_mul(0x9e37_79b9))),
            ),
            Operation::Cellular {
                wavelength,
                jitter,
                mode,
                seed: fixed,
                ..
            } => cellular(
                point(),
                wavelength,
                jitter,
                mode,
                fixed.unwrap_or_else(|| hash(seed ^ step.node.id.wrapping_mul(0x9e37_79b9))),
            ),
            Operation::Rings {
                center,
                spacing,
                width,
                feather,
                ..
            } => {
                let [x, z] = point();
                let distance = (x - center[0]).hypot(z - center[1]);
                let nearest = (distance / spacing).round() * spacing;
                let margin = width * 0.5 - (distance - nearest).abs();
                if feather == 0. {
                    u8::from(margin >= 0.) as f32
                } else {
                    smooth(margin / feather)
                }
            }
            Operation::CircleDistance { center, radius, .. } => {
                let [x, z] = point();
                let [x, z] = [x - center[0], z - center[1]];
                (x * x + z * z).sqrt() - radius
            }
            Operation::BoxDistance {
                center, half_size, ..
            } => {
                let [x, z] = point();
                let [x, z] = [x - center[0], z - center[1]];
                let [sin, cos, _] = step.constants;
                let dx = (x * cos + z * sin).abs() - half_size[0];
                let dz = (-x * sin + z * cos).abs() - half_size[1];
                let [out_x, out_z] = [dx.max(0.), dz.max(0.)];
                (out_x * out_x + out_z * out_z).sqrt() + dx.max(dz).min(0.)
            }
            Operation::CapsuleDistance { start, radius, .. } => {
                let [x, z] = point();
                let [x, z] = [x - start[0], z - start[1]];
                let [dx, dz, inverse] = step.constants;
                let t = ((x * dx + z * dz) * inverse).clamp(0., 1.);
                let [out_x, out_z] = [x - dx * t, z - dz * t];
                (out_x * out_x + out_z * out_z).sqrt() - radius
            }
            Operation::CombineDistance {
                mode, smoothing, ..
            } => match mode {
                DistanceMode::Union => smooth_min(read(0), read(1), smoothing),
                DistanceMode::Intersect => -smooth_min(-read(0), -read(1), smoothing),
                DistanceMode::Subtract => -smooth_min(-read(0), read(1), smoothing),
            },
            Operation::OffsetDistance { metres, .. } => read(0) - metres,
            Operation::DistanceMask { feather, .. } => {
                let distance = read(0);
                if feather == 0. {
                    u8::from(distance <= 0.) as f32
                } else {
                    smooth(0.5 - distance / feather)
                }
            }
            Operation::Combine { mode, .. } => match mode {
                MaskMode::Union => read(0).max(read(1)),
                MaskMode::Intersect => read(0).min(read(1)),
                MaskMode::Subtract => (read(0) - read(1)).max(0.),
                MaskMode::Multiply => read(0) * read(1),
            },
            Operation::Invert { .. } => 1. - read(0),
            Operation::Remap { low, high, .. } | Operation::ElevationMask { low, high, .. } => {
                smooth((read(0) - low) / (high - low))
            }
            Operation::Blend { .. } => read(0) + (read(1) - read(0)) * read(2),
            Operation::Raise { metres, .. } => read(0) + read(1) * metres,
            Operation::Terrace { step, softness, .. } => {
                let scaled = read(0) / step;
                let base = scaled.floor();
                let ramp = if softness == 0. {
                    0.
                } else {
                    smooth((scaled - base - (1. - softness)) / softness)
                };
                (base + ramp) * step
            }
        };
        values[i] = [scalar, 0.];
    }
    values[schedule.len() - 1][0]
}

fn hash(mut value: u32) -> u32 {
    value = (value ^ (value >> 16)).wrapping_mul(0x7feb_352d);
    value = (value ^ (value >> 15)).wrapping_mul(0x846c_a68b);
    value ^ (value >> 16)
}

fn unit_hash(seed: u32) -> f32 {
    (hash(seed) >> 8) as f32 / 16_777_215.
}

fn cellular(p: [f32; 2], wavelength: f32, jitter: f32, mode: CellularMode, seed: u32) -> f32 {
    let [x, z] = p.map(|value| value / wavelength);
    let [left, top] = [x.floor(), z.floor()];
    let [u, v] = [x - left, z - top];
    let mut nearest = f32::INFINITY;
    let mut second = f32::INFINITY;
    let mut cell = 0;
    // Two in-range feature points are within two cell lengths, including
    // at a cell corner. Other cells cannot improve either of the two distances.
    for dz in -2..=2 {
        for dx in -2..=2 {
            let gx = (left as i32 + dx) as u32;
            let gz = (top as i32 + dz) as u32;
            let key = seed ^ gx.wrapping_mul(0x85eb_ca6b) ^ gz.wrapping_mul(0xc2b2_ae35);
            let rx = dx as f32 + 0.5 + (unit_hash(key) - 0.5) * jitter - u;
            let rz = dz as f32 + 0.5 + (unit_hash(key ^ 0xc078_7b29) - 0.5) * jitter - v;
            let distance = rx * rx + rz * rz;
            if distance < nearest {
                second = nearest;
                nearest = distance;
                cell = key;
            } else if distance < second {
                second = distance;
            }
        }
    }
    match mode {
        CellularMode::Centers => {
            (1. - nearest.sqrt() * std::f32::consts::FRAC_1_SQRT_2).clamp(0., 1.)
        }
        CellularMode::Borders => 1. - smooth((second.sqrt() - nearest.sqrt()) * 2.),
        CellularMode::Value => unit_hash(cell ^ 0xb169_9119),
    }
}

fn noise(
    p: [f32; 2],
    wavelength: f32,
    octaves: u8,
    persistence: f32,
    mode: NoiseMode,
    seed: u32,
) -> f32 {
    let [mut x, mut z] = p.map(|v| v / wavelength);
    let mut weight = 1.;
    let mut sum = 0.;
    let mut total = 0.;
    for octave in 0..octaves {
        let [left, top] = [x.floor(), z.floor()];
        let [u, v] = [smooth(x - left), smooth(z - top)];
        let lattice = |dx: i32, dz: i32| {
            let x = (left as i32 + dx) as u32;
            let z = (top as i32 + dz) as u32;
            let mixed = seed
                ^ x.wrapping_mul(0x85eb_ca6b)
                ^ z.wrapping_mul(0xc2b2_ae35)
                ^ u32::from(octave).wrapping_mul(0x27d4_eb2f);
            unit_hash(mixed)
        };
        let [a, b, c, d] = [lattice(0, 0), lattice(1, 0), lattice(0, 1), lattice(1, 1)];
        let upper = a + (b - a) * u;
        let lower = c + (d - c) * u;
        let value = upper + (lower - upper) * v;
        let detail = match mode {
            NoiseMode::Fractal => value,
            NoiseMode::Ridged => 1. - (2. * value - 1.).abs(),
            NoiseMode::Billow => (2. * value - 1.).abs(),
        };
        sum += detail * weight;
        total += weight;
        x *= 2.;
        z *= 2.;
        weight *= persistence;
    }
    (sum / total).clamp(0., 1.)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layers() -> Layers {
        let (height_resolution, surface_resolution) =
            crate::native_layers::dimensions(1000).unwrap();
        Layers {
            size: 1000,
            terrain_seed: 731,
            biome_seed: 2,
            height_resolution,
            surface_resolution,
            height: vec![crate::erosion::encode(20.); height_resolution.pow(2)],
            biome: Vec::new(),
            splat: Vec::new(),
            topology: Vec::new(),
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        }
    }

    #[test]
    fn group_labels_leave_generation_keys_unchanged_and_duplicates_keep_typed_wiring() {
        let mut graph: Graph = serde_json::from_str(
            r#"{
            "version":1,"output":3,"nodes":[
                {"id":1,"op":{"kind":"native_height"}},
                {"id":2,"op":{"kind":"mask","coverage":0.5}},
                {"id":3,"op":{"kind":"raise","height":1,"mask":2,"metres":20}}
            ]
        }"#,
        )
        .unwrap();
        let before = serde_json::to_vec(&graph).unwrap();
        graph.groups.push(NodeGroup {
            name: "Ridge".into(),
            nodes: vec![2, 3],
        });
        assert_eq!(before, serde_json::to_vec(&graph.evaluation_key()).unwrap());
        let index = graph.duplicate_group(0).unwrap();
        let ids = &graph.groups[index].nodes;
        assert_eq!(ids, &[4, 5]);
        assert!(matches!(
            graph.nodes[4].op,
            Operation::Raise {
                height: 1,
                mask: 4,
                ..
            }
        ));
        assert_eq!(graph.output, 3);
        assert!(graph.validate(1000).is_ok());
        let bytes = serde_json::to_vec(&graph).unwrap();
        assert_eq!(Graph::read(bytes.as_slice()).unwrap(), graph);
        graph.groups[1].nodes.push(2);
        assert!(graph.validate(1000).is_err());
    }

    #[test]
    fn linked_groups_keep_independent_inputs_and_share_nested_edits() {
        let mut graph: Graph = serde_json::from_str(
            r#"{
                "version":1,"output":4,"nodes":[
                    {"id":1,"op":{"kind":"height","metres":10}},
                    {"id":2,"op":{"kind":"mask","coverage":0.5}},
                    {"id":3,"op":{"kind":"raise","height":1,"mask":2,"metres":20}},
                    {"id":4,"op":{"kind":"raise","height":3,"mask":2,"metres":10}}
                ],
                "groups":[{"name":"Slope","nodes":[3]}]
            }"#,
        )
        .unwrap();
        let at = |graph: &Graph| {
            let schedule = graph.compile(1000).unwrap();
            sample(
                &schedule,
                &mut [[0.; 2]; MAX_EVALUATED_NODES + 1],
                [0.; 2],
                0,
                73,
            )
        };
        assert_eq!(at(&graph), 25.);
        assert_eq!(graph.link_group(0, 3), Ok(3));
        assert_eq!(at(&graph), 25.);

        graph.groups.push(NodeGroup {
            name: "Slope and crest".into(),
            nodes: vec![3, 4],
        });
        assert_eq!(graph.link_group(0, 4), Ok(4));
        assert_eq!(at(&graph), 25.);
        let mut copy = graph.definitions[&2].instance(2).unwrap();
        for (slot, input) in copy.inputs().into_iter().flatten().enumerate() {
            let source = match input.port {
                Port::Height => 5,
                Port::Mask => 2,
                _ => panic!("Unexpected linked input"),
            };
            copy.set_input(slot, Some(source));
        }
        graph.nodes.push(Node {
            id: 5,
            op: Operation::Height { metres: 40. },
        });
        graph.nodes.push(Node { id: 6, op: copy });
        graph.output = 6;
        assert_eq!(at(&graph), 55.);

        let key = serde_json::to_vec(&graph.evaluation_key()).unwrap();
        graph.definitions.get_mut(&1).unwrap().nodes[0].op = Operation::Raise {
            height: 1,
            mask: 2,
            metres: 30.,
        };
        assert_ne!(key, serde_json::to_vec(&graph.evaluation_key()).unwrap());
        assert_eq!(at(&graph), 60.);
        graph.output = 4;
        assert_eq!(at(&graph), 30.);
        let bytes = serde_json::to_vec(&graph).unwrap();
        let graph = Graph::read(bytes.as_slice()).unwrap();
        assert_eq!(at(&graph), 30.);
    }

    #[test]
    fn linked_copies_share_only_the_steps_with_matching_inputs() {
        let mut graph: Graph = serde_json::from_str(
            r#"{
                "version":1,"output":4,"nodes":[
                    {"id":1,"op":{"kind":"height","metres":10}},
                    {"id":2,"op":{"kind":"mask","coverage":0.5}},
                    {"id":3,"op":{"kind":"noise","mode":"ridged","wavelength":500,"octaves":3,"persistence":0.5}},
                    {"id":4,"op":{"kind":"raise","height":1,"mask":3,"metres":20}},
                    {"id":5,"op":{"kind":"height","metres":30}}
                ],
                "groups":[{"name":"Hills","nodes":[3,4]}]
            }"#,
        )
        .unwrap();
        let at = |schedule: &[Step<'_>]| {
            sample(
                schedule,
                &mut [[0.; 2]; MAX_EVALUATED_NODES + 1],
                [10., 20.],
                0,
                73,
            )
        };
        let expected = at(&graph.compile(1000).unwrap());
        graph.link_group(0, 4).unwrap();
        graph.nodes.push(Node {
            id: 6,
            op: graph.nodes.last().unwrap().op,
        });
        graph.nodes.push(Node {
            id: 7,
            op: Operation::Blend {
                low: 4,
                high: 6,
                mask: 2,
            },
        });
        graph.output = 7;
        let shared = graph.compile(1000).unwrap();
        assert_eq!(shared.len(), 5);
        assert_eq!(at(&shared), expected);

        graph
            .nodes
            .iter_mut()
            .find(|node| node.id == 6)
            .unwrap()
            .op
            .set_input(0, Some(5));
        let distinct = graph.compile(1000).unwrap();
        assert_eq!(distinct.len(), 7);
        assert_eq!(at(&distinct), expected + 10.);
    }

    #[test]
    fn unused_group_inputs_stay_validated_without_entering_the_sample_loop() {
        let mut graph: Graph = serde_json::from_str(
            r#"{
                "version":1,"output":5,"nodes":[
                    {"id":1,"op":{"kind":"height","metres":10}},
                    {"id":2,"op":{"kind":"noise","mode":"ridged","wavelength":500,"octaves":3,"persistence":0.5}},
                    {"id":3,"op":{"kind":"invert","input":2}},
                    {"id":4,"op":{"kind":"terrace","height":1,"step":5,"softness":0}},
                    {"id":5,"op":{"kind":"terrace","height":4,"step":5,"softness":0}}
                ],
                "groups":[{"name":"Terrace","nodes":[3,4]}]
            }"#,
        )
        .unwrap();
        graph.link_group(0, 4).unwrap();
        let retained: Vec<_> = graph
            .compile(1000)
            .unwrap()
            .iter()
            .map(|step| step.node.id)
            .collect();
        assert_eq!(retained, [1, 4, 5]);

        graph.nodes.retain(|node| node.id != 2);
        assert!(graph.validate(1000).unwrap_err().contains("missing node 2"));
    }

    #[test]
    fn linked_group_validation_rejects_recursion_even_in_unused_members() {
        let mut graph: Graph = serde_json::from_str(
            r#"{
                "version":1,"output":1,"nodes":[{"id":1,"op":{"kind":"height","metres":5}}],
                "groups":[{"name":"Base","nodes":[1]}]
            }"#,
        )
        .unwrap();
        graph.link_group(0, 1).unwrap();
        let recursive = graph.definitions[&1].instance(1).unwrap();
        graph.definitions.get_mut(&1).unwrap().nodes.push(Node {
            id: 2,
            op: recursive,
        });
        assert_eq!(
            graph.validate(1000),
            Err("Recursive terrain group 1".into())
        );
    }

    #[test]
    fn embedded_heightmap_samples_world_coordinates_and_round_trips() {
        let image = Heightmap {
            label: "Height study".into(),
            dimensions: [2, 2],
            samples: Arc::from([0, u16::MAX, u16::MAX, 0]),
        };
        let mut graph = Graph {
            version: VERSION,
            output: 1,
            heightmaps: BTreeMap::from([(4, image)]),
            groups: Vec::new(),
            definitions: Default::default(),
            nodes: vec![Node {
                id: 1,
                op: Operation::Heightmap {
                    position: None,
                    asset: 4,
                    center: [10., 30.],
                    size: [100., 200.],
                    low: -20.,
                    high: 80.,
                },
            }],
        };
        let bytes = serde_json::to_vec(&graph).unwrap();
        assert_eq!(Graph::read(bytes.as_slice()).unwrap(), graph);
        let schedule = graph.compile(1000).unwrap();
        let at = |p| sample(&schedule, &mut [[0.; 2]; MAX_EVALUATED_NODES + 1], p, 0, 73);
        assert_eq!(at([-40., 130.]), -20.);
        assert_eq!(at([-40., -70.]), 80.);
        assert_eq!(at([10., 30.]), 30.);
        assert_eq!(at([100., 200.]), 80.);
        let native = layers();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let rendered = graph
            .render(&native, 16 << 20, &pool, || false)
            .unwrap()
            .unwrap();
        assert_eq!(rendered[0], crate::erosion::encode(80.));
        assert_eq!(
            rendered[native.height_resolution * (native.height_resolution - 1)],
            crate::erosion::encode(-20.)
        );
        graph.heightmaps.clear();
        assert!(
            graph
                .validate(1000)
                .unwrap_err()
                .contains("Choose a heightmap")
        );
    }

    #[test]
    fn validates_types_dependencies_and_disconnected_cycles() {
        let mut graph = Graph {
            version: 1,
            output: 4,
            heightmaps: Default::default(),
            groups: Vec::new(),
            definitions: Default::default(),
            nodes: vec![
                Node {
                    id: 4,
                    op: Operation::Raise {
                        height: 1,
                        mask: 2,
                        metres: 10.,
                    },
                },
                Node {
                    id: 2,
                    op: Operation::Mask { coverage: 0.5 },
                },
                Node {
                    id: 1,
                    op: Operation::NativeHeight,
                },
            ],
        };
        assert!(graph.validate(1000).is_ok());
        graph.nodes[0].op = Operation::Raise {
            height: 2,
            mask: 2,
            metres: 10.,
        };
        assert!(graph.validate(1000).unwrap_err().contains("wrong port"));
        graph.nodes[0].op = Operation::Raise {
            height: 1,
            mask: 3,
            metres: 10.,
        };
        assert!(graph.validate(1000).unwrap_err().contains("missing node 3"));
        graph.nodes[0].op = Operation::Raise {
            height: 1,
            mask: 2,
            metres: 10.,
        };
        graph.nodes.push(Node {
            id: 5,
            op: Operation::Invert { input: 5 },
        });
        assert!(graph.validate(1000).unwrap_err().contains("cycle"));
    }

    #[test]
    fn a_shared_warp_can_move_a_mask_without_moving_the_native_height() {
        let mut graph: Graph = serde_json::from_str(
            r#"{
          "version": 1, "output": 16, "nodes": [
            {"id": 1, "op": {"kind": "world_position"}},
            {"id": 2, "op": {"kind": "transform_position", "position": 1,
                                "offset": [-100, 0], "scale": [1, 1], "rotation": 0}},
            {"id": 3, "op": {"kind": "mask", "coverage": 0.75}},
            {"id": 4, "op": {"kind": "mask", "coverage": 0.5}},
            {"id": 5, "op": {"kind": "warp_position", "position": 2,
                                "x_mask": 3, "z_mask": 4, "amount": [100, 10]}},
            {"id": 6, "op": {"kind": "ellipse", "position": 5, "center": [50, 0],
                                "radii": [20, 20], "rotation": 0, "feather": 0}},
            {"id": 7, "op": {"kind": "native_height"}},
            {"id": 16, "op": {"kind": "raise", "height": 7, "mask": 6, "metres": 10}}
          ]
        }"#,
        )
        .unwrap();
        let source = crate::erosion::encode(28.);
        let h = crate::erosion::decode(source);
        let value = |graph: &Graph, p| {
            let plan = graph.compile(1000).unwrap();
            sample(
                &plan,
                &mut [[0.; 2]; MAX_EVALUATED_NODES + 1],
                p,
                source,
                17,
            )
        };
        assert_eq!(value(&graph, [100., 0.]), h + 10.);
        assert_eq!(value(&graph, [50., 0.]), h);
        if let Operation::Ellipse { position, .. } = &mut graph.nodes[5].op {
            *position = Some(7);
        }
        assert!(graph.validate(1000).unwrap_err().contains("wrong port"));
    }

    #[test]
    fn cellular_modes_locate_centers_and_borders_in_cell_units() {
        assert_eq!(
            cellular([50., 50.], 100., 0., CellularMode::Centers, 15),
            1.
        );
        assert_eq!(
            cellular([50., 50.], 100., 0., CellularMode::Borders, 15),
            0.
        );
        assert_eq!(
            cellular([100., 50.], 100., 0., CellularMode::Borders, 15),
            1.
        );
        let value = |x, seed| cellular([x, 45.], 100., 0., CellularMode::Value, seed);
        assert_eq!(value(20., 15), value(75., 15));
        assert_ne!(value(20., 15), value(120., 15));
        assert_ne!(value(20., 15), value(20., 19));
    }

    #[test]
    fn lagoon_recipe_builds_land_around_a_channel_connected_to_the_ocean() {
        let mut graph = Graph::read(
            include_bytes!("../../../docs/examples/terrain-graphs/tidal-lagoon.json").as_slice(),
        )
        .unwrap();
        let plan = graph.compile(4000).unwrap();
        let at = |p| sample(&plan, &mut [[0.; 2]; MAX_EVALUATED_NODES + 1], p, 0, 731);
        assert_eq!(at([0., 0.]), -24.); // lagoon centre
        assert_eq!(at([660., 710.]), -24.); // outlet across the island's rim
        assert_eq!(at([1500., 1500.]), -24.);
        assert!(at([-700., 0.]) >= 28.);
        assert!(at([650., -300.]) > 0.);
        let serialized = serde_json::to_vec(&graph).unwrap();
        assert_eq!(Graph::read(serialized.as_slice()).unwrap(), graph);

        assert!(graph.nodes[6].op.set_input(0, Some(8)));
        assert!(graph.validate(4000).unwrap_err().contains("wrong port"));
    }

    #[test]
    fn smoothing_makes_a_wider_join_and_preserves_distant_edges() {
        assert_eq!(smooth_min(0., 0., 0.), 0.);
        assert_eq!(smooth_min(0., 0., 80.), -20.);
        assert_eq!(smooth_min(-30., 70., 80.), -30.);
        assert!(smooth_min(5., 10., f32::MIN_POSITIVE).is_finite());
    }

    #[test]
    fn rings_can_follow_a_mirrored_position_frame() {
        let graph: Graph = serde_json::from_str(
            r#"{
          "version": 1, "output": 4, "nodes": [
            {"id": 1, "op": {"kind": "mirror_position", "center": [25, 0], "axes": [true, false]}},
            {"id": 2, "op": {"kind": "rings", "position": 1, "center": [100, 0],
                                "spacing": 100, "width": 20, "feather": 5}},
            {"id": 3, "op": {"kind": "height", "metres": 0}},
            {"id": 4, "op": {"kind": "raise", "height": 3, "mask": 2, "metres": 50}}
          ]
        }"#,
        )
        .unwrap();
        let plan = graph.compile(1000).unwrap();
        let value = |x| {
            sample(
                &plan,
                &mut [[0.; 2]; MAX_EVALUATED_NODES + 1],
                [x, 0.],
                0,
                1,
            )
        };
        assert_eq!(value(100.), 50.);
        assert_eq!(value(-50.), 50.);
        assert_eq!(value(75.), 0.);
        assert_eq!(value(92.5), 25.);
    }

    #[test]
    fn a_mask_can_blend_a_new_island_into_a_lower_seafloor() {
        let graph: Graph = serde_json::from_str(
            r#"{
          "version": 1, "output": 80, "nodes": [
            {"id": 80, "op": {"kind": "blend", "low": 10, "high": 20, "mask": 30}},
            {"id": 10, "op": {"kind": "height", "metres": -30}},
            {"id": 20, "op": {"kind": "height", "metres": 50}},
            {"id": 30, "op": {"kind": "ellipse", "center": [0, 0], "radii": [275, 160],
                                "rotation": 0, "feather": 0.25}}
          ]
        }"#,
        )
        .unwrap();
        let layers = layers();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let height = graph
            .render(&layers, 16 << 20, &pool, || false)
            .unwrap()
            .unwrap();
        let n = layers.height_resolution;
        assert_eq!(height[(n / 2) * n + n / 2], crate::erosion::encode(50.));
        assert_eq!(height[0], crate::erosion::encode(-30.));
        assert_ne!(layers.height, height);
        assert!(graph.render(&layers, 1, &pool, || false).is_err());
        assert!(
            graph
                .render(&layers, 16 << 20, &pool, || true)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn seed_controls_noise_and_worker_count_preserves_output() {
        let graph: Graph = serde_json::from_str(
            r#"{
          "version": 1, "output": 3, "nodes": [
            {"id": 1, "op": {"kind": "native_height"}},
            {"id": 2, "op": {"kind": "noise", "mode": "ridged", "wavelength": 220,
                                "octaves": 3, "persistence": 0.5}},
            {"id": 3, "op": {"kind": "raise", "height": 1, "mask": 2, "metres": 15}}
          ]
        }"#,
        )
        .unwrap();
        let mut layers = layers();
        let render = |layers: &Layers, threads| {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            graph
                .render(layers, 16 << 20, &pool, || false)
                .unwrap()
                .unwrap()
        };
        let first = render(&layers, 1);
        assert_eq!(first, render(&layers, 3));
        layers.terrain_seed += 1;
        assert_ne!(first, render(&layers, 1));
    }
}
