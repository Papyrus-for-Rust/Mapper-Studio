//! Bounded environment geometry and independent box/convex-hull overlap models.
//! Unresolved geometry and incomplete world input cannot produce clearance.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
mod hull;
pub mod scene;
pub use hull::Convex;

pub const VERSION: &str = "papyrus-environment-hulls-2";
const LIMIT: f32 = 1_000_000.;
const MAX_VOLUMES: usize = 131072;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transform {
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
}
fn vector(v: &[f32]) -> bool {
    v.iter().all(|x| x.is_finite() && x.abs() <= LIMIT)
}
fn quaternion(q: [f32; 4]) -> bool {
    vector(&q) && (q.iter().map(|v| v * v).sum::<f32>() - 1.).abs() <= 0.001
}
impl Transform {
    pub fn validate(&self) -> Result<(), String> {
        if !vector(&self.position)
            || !quaternion(self.rotation)
            || self
                .scale
                .iter()
                .any(|v| !v.is_finite() || !(0.000001..=10000.).contains(v))
        {
            return Err("Unsupported environment transform (positive scale required)".into());
        }
        Ok(())
    }
}
pub fn multiply(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [
        a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
        a[3] * b[1] + a[1] * b[3] + a[2] * b[0] - a[0] * b[2],
        a[3] * b[2] + a[2] * b[3] + a[0] * b[1] - a[1] * b[0],
        a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
    ]
}
pub fn rotate(q: [f32; 4], p: [f32; 3]) -> [f32; 3] {
    let [x, y, z, w] = q;
    let (xx, yy, zz) = (x * 2., y * 2., z * 2.);
    [
        (1. - (y * yy + z * zz)) * p[0] + (x * yy - w * zz) * p[1] + (x * zz + w * yy) * p[2],
        (x * yy + w * zz) * p[0] + (1. - (x * xx + z * zz)) * p[1] + (y * zz - w * xx) * p[2],
        (x * zz - w * yy) * p[0] + (y * zz + w * xx) * p[1] + (1. - (x * xx + y * yy)) * p[2],
    ]
}
fn scale(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| a[i] * b[i])
}
fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| a[i] + b[i])
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Box3 {
    pub center: [f32; 3],
    pub rotation: [f32; 4],
    pub extents: [f32; 3],
}
impl Box3 {
    pub fn validate(&self) -> Result<(), String> {
        if !vector(&self.center)
            || !quaternion(self.rotation)
            || self
                .extents
                .iter()
                .any(|v| !v.is_finite() || !(0.000001..=10000.).contains(v))
        {
            return Err("Invalid environment box".into());
        }
        Ok(())
    }
    fn axes(&self) -> [[f64; 3]; 3] {
        let length = self
            .rotation
            .iter()
            .map(|v| f64::from(*v).powi(2))
            .sum::<f64>()
            .sqrt();
        let [x, y, z, w] = self.rotation.map(|v| f64::from(v) / length);
        [
            [
                1. - 2. * (y * y + z * z),
                2. * (x * y + w * z),
                2. * (x * z - w * y),
            ],
            [
                2. * (x * y - w * z),
                1. - 2. * (x * x + z * z),
                2. * (y * z + w * x),
            ],
            [
                2. * (x * z + w * y),
                2. * (y * z - w * x),
                1. - 2. * (x * x + y * y),
            ],
        ]
    }
    pub fn bounds(&self) -> Bounds {
        let axes = self.axes();
        let half: [f64; 3] = std::array::from_fn(|i| {
            (0..3)
                .map(|j| axes[j][i].abs() * f64::from(self.extents[j]))
                .sum()
        });
        Bounds {
            minimum: std::array::from_fn(|i| f64::from(self.center[i]) - half[i]),
            maximum: std::array::from_fn(|i| f64::from(self.center[i]) + half[i]),
        }
    }
}
/// Authored checks rotate their centre offset but retain the instance rotation
/// for the query axes, exactly as EnvironmentVolumeCheckEx constructs its OBB.
pub fn check_box(
    local_position: [f32; 3],
    local_rotation: [f32; 4],
    center: [f32; 3],
    size: [f32; 3],
    pose: Transform,
) -> Result<Box3, String> {
    pose.validate()?;
    if !vector(&local_position)
        || !quaternion(local_rotation)
        || !vector(&center)
        || size
            .iter()
            .any(|v| !v.is_finite() || *v <= 0. || *v > 10000.)
    {
        return Err("Invalid authored environment check".into());
    }
    let offset = add(local_position, rotate(local_rotation, center));
    let result = Box3 {
        center: add(
            pose.position,
            rotate(pose.rotation, scale(offset, pose.scale)),
        ),
        rotation: pose.rotation,
        extents: scale(size, pose.scale).map(|v| v * 0.5),
    };
    result.validate()?;
    Ok(result)
}
/// Mirrors OBB's numeric constructor followed by OBB.Transform. Its rotation is
/// deliberately local * instance; it is not the registered collider transform.
pub fn volume_box(
    local: Transform,
    center: [f32; 3],
    size: [f32; 3],
    pose: Transform,
) -> Result<Box3, String> {
    local.validate()?;
    pose.validate()?;
    if !vector(&center)
        || size
            .iter()
            .any(|v| !v.is_finite() || *v <= 0. || *v > 10000.)
    {
        return Err("Invalid authored environment volume".into());
    }
    let p = add(
        local.position,
        rotate(local.rotation, scale(local.scale, center)),
    );
    let result = Box3 {
        center: add(pose.position, rotate(pose.rotation, scale(p, pose.scale))),
        rotation: multiply(local.rotation, pose.rotation),
        extents: scale(pose.scale, scale(local.scale, size.map(|v| v * 0.5))),
    };
    result.validate()?;
    Ok(result)
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bounds {
    pub minimum: [f64; 3],
    pub maximum: [f64; 3],
}
impl Bounds {
    fn valid(&self) -> bool {
        (0..3).all(|i| {
            self.minimum[i].is_finite()
                && self.maximum[i].is_finite()
                && self.minimum[i] >= -2_000_000.
                && self.maximum[i] <= 2_000_000.
                && self.minimum[i] <= self.maximum[i]
        })
    }
    fn overlaps(self, b: Self, tolerance: f64) -> bool {
        // Broad-phase padding must cover the narrow phase's numerical reserve,
        // including long, thin convex hulls queried at an arbitrary orientation.
        let rounding = (0..3)
            .map(|i| (self.maximum[i] - self.minimum[i]) + (b.maximum[i] - b.minimum[i]))
            .fold(1., f64::max)
            * 1e-9;
        (0..3).all(|i| {
            self.minimum[i] <= b.maximum[i] + tolerance + rounding
                && b.minimum[i] <= self.maximum[i] + tolerance + rounding
        })
    }
    fn union(self, b: Self) -> Self {
        Self {
            minimum: std::array::from_fn(|i| self.minimum[i].min(b.minimum[i])),
            maximum: std::array::from_fn(|i| self.maximum[i].max(b.maximum[i])),
        }
    }
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] * b[0] + a[1] * b[1]) + a[2] * b[2]
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
pub fn overlap(a: Box3, b: Box3, tolerance: f64) -> bool {
    let aa = a.axes();
    let ba = b.axes();
    let delta = std::array::from_fn(|i| f64::from(b.center[i]) - f64::from(a.center[i]));
    let separated = |axis: [f64; 3]| {
        let length = dot(axis, axis).sqrt();
        if length < 1e-12 {
            return false;
        }
        let axis = axis.map(|v| v / length);
        let radius = (0..3)
            .map(|i| {
                f64::from(a.extents[i]) * dot(aa[i], axis).abs()
                    + f64::from(b.extents[i]) * dot(ba[i], axis).abs()
            })
            .sum::<f64>();
        dot(delta, axis).abs() > radius + tolerance + radius * 1e-12
    };
    !aa.into_iter().chain(ba).any(separated)
        && !(0..3).any(|i| (0..3).any(|j| separated(cross(aa[i], ba[j]))))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Key {
    pub instance: u64,
    pub component: i64,
}
#[derive(Clone, Debug)]
pub enum Shape {
    Box(Box3),
    /// Independent convex hull of captured vertices; not a cooked PhysX mesh.
    Convex(Convex),
    /// A conservative bound must come from the selected collider geometry model,
    /// never from an unrelated display mesh or EnvironmentVolume.Center/Size.
    /// Unknown bounds remain global blockers for a matching query mask.
    Unresolved {
        bounds: Option<Bounds>,
    },
}
#[derive(Clone, Debug)]
pub struct Volume {
    pub key: Key,
    pub mask: u32,
    pub shape: Shape,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coverage {
    Complete,
    Incomplete,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum Verdict {
    Clear,
    Blocked(Key),
    Unresolved(Key),
    Incomplete,
    Budget,
    Cancelled,
}
#[derive(Clone, Debug, Serialize)]
pub struct Assessment {
    pub verdict: Verdict,
    pub work: usize,
    pub candidates: usize,
}
#[derive(Debug)]
struct Node {
    bounds: Bounds,
    mask: u32,
    range: std::ops::Range<usize>,
    children: Option<[usize; 2]>,
}
pub struct World {
    volumes: Vec<Volume>,
    bounds: Vec<Bounds>,
    unbounded: [Option<Key>; 32],
    unbounded_mask: u32,
    nodes: Vec<Node>,
    coverage: Coverage,
}
struct Meter<'a, F: Fn() -> bool> {
    used: usize,
    limit: usize,
    cancelled: &'a F,
}
impl<F: Fn() -> bool> Meter<'_, F> {
    fn spend(&mut self, n: usize) -> Result<(), Verdict> {
        if (self.cancelled)() {
            return Err(Verdict::Cancelled);
        }
        if n > self.limit.saturating_sub(self.used) {
            return Err(Verdict::Budget);
        }
        self.used += n;
        Ok(())
    }
}
impl World {
    /// Caller-supplied coverage must describe the generated world, not merely
    /// the current prefab. Construction/cancellation never publishes half an index.
    pub fn build(
        volumes: Vec<Volume>,
        coverage: Coverage,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Result<Self, Verdict>, String> {
        if volumes.len() > MAX_VOLUMES || !(1..=128_000_000).contains(&maximum_work) {
            return Err("Invalid environment index allowance".into());
        }
        let mut meter = Meter {
            used: 0,
            limit: maximum_work,
            cancelled: &cancelled,
        };
        Self::build_with_meter(volumes, coverage, &mut meter)
    }
    fn build_with_meter<F: Fn() -> bool>(
        volumes: Vec<Volume>,
        coverage: Coverage,
        meter: &mut Meter<'_, F>,
    ) -> Result<Result<Self, Verdict>, String> {
        if volumes.len() > MAX_VOLUMES {
            return Err("Excessive environment volumes".into());
        }
        let mut keys = BTreeSet::new();
        let mut ordered = Vec::new();
        let mut unbounded = [None; 32];
        let mut unbounded_mask = 0;
        for volume in volumes {
            if let Err(v) = meter.spend(1) {
                return Ok(Err(v));
            }
            if volume.key.instance == 0 || volume.key.component == 0 || !keys.insert(volume.key) {
                return Err("Duplicate or missing environment identity".into());
            }
            let bounds = match &volume.shape {
                Shape::Box(b) => {
                    b.validate()?;
                    Some(b.bounds())
                }
                Shape::Convex(hull) => Some(hull.bounds()),
                Shape::Unresolved { bounds } => *bounds,
            };
            if let Some(bounds) = bounds {
                if !bounds.valid() {
                    return Err("Invalid unresolved environment bounds".into());
                }
                ordered.push((volume, bounds));
            } else {
                if let Err(v) = meter.spend(volume.mask.count_ones() as usize) {
                    return Ok(Err(v));
                }
                let mut bits = volume.mask;
                while bits != 0 {
                    let bit = bits.trailing_zeros() as usize;
                    bits &= bits - 1;
                    unbounded[bit] =
                        Some(unbounded[bit].map_or(volume.key, |key: Key| key.min(volume.key)));
                }
                unbounded_mask |= volume.mask;
            }
        }
        if let Err(v) = meter.spend(1) {
            return Ok(Err(v));
        }
        let mut nodes = Vec::new();
        if !ordered.is_empty()
            && let Err(v) = Self::build_node(&mut ordered, 0, &mut nodes, meter)
        {
            return Ok(Err(v));
        }
        let (volumes, bounds) = ordered.into_iter().unzip();
        Ok(Ok(Self {
            volumes,
            bounds,
            unbounded,
            unbounded_mask,
            nodes,
            coverage,
        }))
    }
    fn build_node<F: Fn() -> bool>(
        items: &mut [(Volume, Bounds)],
        offset: usize,
        nodes: &mut Vec<Node>,
        meter: &mut Meter<'_, F>,
    ) -> Result<usize, Verdict> {
        meter.spend(items.len() + 1)?;
        let mut bounds = items[0].1;
        let mut mask = 0;
        for (volume, b) in items.iter() {
            bounds = bounds.union(*b);
            mask |= volume.mask;
        }
        let id = nodes.len();
        nodes.push(Node {
            bounds,
            mask,
            range: offset..offset + items.len(),
            children: None,
        });
        if items.len() > 8 {
            let axis = (0..3)
                .max_by(|&a, &b| {
                    (bounds.maximum[a] - bounds.minimum[a])
                        .total_cmp(&(bounds.maximum[b] - bounds.minimum[b]))
                })
                .unwrap();
            meter.spend(items.len() * (items.len().ilog2() as usize + 1))?;
            items.sort_unstable_by(|a, b| {
                (a.1.minimum[axis] + a.1.maximum[axis])
                    .total_cmp(&(b.1.minimum[axis] + b.1.maximum[axis]))
                    .then(a.0.key.cmp(&b.0.key))
            });
            meter.spend(1)?;
            let mid = items.len() / 2;
            let (a, b) = items.split_at_mut(mid);
            let left = Self::build_node(a, offset, nodes, meter)?;
            let right = Self::build_node(b, offset + mid, nodes, meter)?;
            nodes[id].children = Some([left, right]);
        }
        Ok(id)
    }
    pub fn query(
        &self,
        query: Box3,
        mask: u32,
        separation_tolerance: f64,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Assessment, String> {
        query.validate()?;
        if !separation_tolerance.is_finite()
            || !(0. ..=1.).contains(&separation_tolerance)
            || !(1..=128_000_000).contains(&maximum_work)
        {
            return Err("Invalid environment query allowance".into());
        }
        let mut meter = Meter {
            used: 0,
            limit: maximum_work,
            cancelled: &cancelled,
        };
        let mut unknown = None;
        let mut candidates = 0;
        let bounds = query.bounds();
        let result = (|| -> Result<Verdict, Verdict> {
            meter.spend(1)?;
            if mask == 0 {
                return Ok(Verdict::Clear);
            }
            let mut bits = mask & self.unbounded_mask;
            while bits != 0 {
                meter.spend(1)?;
                let bit = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                let key = self.unbounded[bit].expect("Indexed unknown mask has a witness");
                unknown = Some(unknown.map_or(key, |previous: Key| previous.min(key)));
            }
            // Balanced construction and the volume cap bound traversal depth.
            // Keep candidate queries allocation-free.
            let mut stack = [0usize; 64];
            let mut count = 0;
            if !self.nodes.is_empty() {
                count = 1;
            }
            while count > 0 {
                count -= 1;
                let id = stack[count];
                meter.spend(1)?;
                let node = &self.nodes[id];
                if node.mask & mask == 0 || !node.bounds.overlaps(bounds, separation_tolerance) {
                    continue;
                }
                if let Some([a, b]) = node.children {
                    stack[count] = b;
                    stack[count + 1] = a;
                    count += 2;
                    continue;
                }
                for index in node.range.clone() {
                    meter.spend(1)?;
                    let volume = &self.volumes[index];
                    if volume.mask & mask == 0
                        || !self.bounds[index].overlaps(bounds, separation_tolerance)
                    {
                        continue;
                    }
                    candidates += 1;
                    match &volume.shape {
                        Shape::Box(b) => {
                            meter.spend(15)?;
                            if overlap(query, *b, separation_tolerance) {
                                return Ok(Verdict::Blocked(volume.key));
                            }
                        }
                        Shape::Convex(hull) => {
                            if hull.overlaps(query, separation_tolerance, &mut meter)? {
                                return Ok(Verdict::Blocked(volume.key));
                            }
                        }
                        Shape::Unresolved { .. } => {
                            unknown = Some(volume.key);
                        }
                    }
                }
            }
            Ok(if let Some(key) = unknown {
                Verdict::Unresolved(key)
            } else if self.coverage == Coverage::Incomplete && mask != 0 {
                Verdict::Incomplete
            } else {
                Verdict::Clear
            })
        })();
        Ok(Assessment {
            verdict: result.unwrap_or_else(|v| v),
            work: meter.used,
            candidates,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cube(x: f32) -> Box3 {
        Box3 {
            center: [x, 0., 0.],
            rotation: [0., 0., 0., 1.],
            extents: [1.; 3],
        }
    }
    fn volume(id: u64, x: f32) -> Volume {
        Volume {
            key: Key {
                instance: id,
                component: 1,
            },
            mask: 17,
            shape: Shape::Box(cube(x)),
        }
    }
    #[test]
    fn source_numeric_boxes_match_owned_constructors_and_transform() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/environment-box-reference.json"
        ))
        .unwrap();
        assert_eq!(fixture["format"], "papyrus-owned-environment-box-reference");
        assert_eq!(
            fixture["sources"]["Rust.Global.dll"],
            "44dd8a1323b12d7dd55aece202d8f2a587c75bd825cd218509f47c326cc409f0"
        );
        assert_eq!(fixture["gameProcessStarted"], false);
        assert_eq!(fixture["nativeLibraryLoaded"], false);
        assert_eq!(fixture["physicsExecuted"], false);
        for row in fixture["rows"].as_array().unwrap() {
            let input = &row["input"];
            let a3 = |key: &str| serde_json::from_value::<[f32; 3]>(input[key].clone()).unwrap();
            let a4 = |key: &str| serde_json::from_value::<[f32; 4]>(input[key].clone()).unwrap();
            let pose = Transform {
                position: a3("Position"),
                rotation: a4("Rotation"),
                scale: a3("Scale"),
            };
            let b = if input["Kind"] == "check" {
                check_box(
                    a3("LocalPosition"),
                    a4("LocalRotation"),
                    a3("Center"),
                    a3("Size"),
                    pose,
                )
            } else {
                volume_box(
                    Transform {
                        position: a3("LocalPosition"),
                        rotation: a4("LocalRotation"),
                        scale: a3("LocalScale"),
                    },
                    a3("Center"),
                    a3("Size"),
                    pose,
                )
            }
            .unwrap();
            for (name, actual) in [
                ("center", b.center.to_vec()),
                ("rotation", b.rotation.to_vec()),
                ("extents", b.extents.to_vec()),
            ] {
                let expected: Vec<f32> = serde_json::from_value(row[name].clone()).unwrap();
                for (a, e) in actual.iter().zip(expected) {
                    assert!(
                        (a - e).abs() <= 0.0001 + e.abs() * 0.000001,
                        "{name}: {a} != {e}"
                    );
                }
            }
        }
    }
    #[test]
    fn boxes_masks_touching_tolerance_and_incomplete_worlds_are_distinct() {
        let world = World::build(vec![volume(1, 0.)], Coverage::Complete, 100, || false)
            .unwrap()
            .ok()
            .unwrap();
        assert!(matches!(
            world
                .query(cube(2.), 17, 0., 100, || false)
                .unwrap()
                .verdict,
            Verdict::Blocked(_)
        ));
        assert_eq!(
            world
                .query(cube(2.1), 17, 0., 100, || false)
                .unwrap()
                .verdict,
            Verdict::Clear
        );
        assert!(matches!(
            world
                .query(cube(2.1), 17, 0.11, 100, || false)
                .unwrap()
                .verdict,
            Verdict::Blocked(_)
        ));
        assert_eq!(
            world.query(cube(0.), 2, 0., 100, || false).unwrap().verdict,
            Verdict::Clear
        );
        let partial = World::build(vec![], Coverage::Incomplete, 100, || false)
            .unwrap()
            .ok()
            .unwrap();
        assert_eq!(
            partial
                .query(cube(0.), 17, 0., 100, || false)
                .unwrap()
                .verdict,
            Verdict::Incomplete
        );
    }
    #[test]
    fn unresolved_geometry_is_not_silently_clear_and_known_hits_take_precedence() {
        let unresolved = Volume {
            key: Key {
                instance: 1,
                component: 1,
            },
            mask: 17,
            shape: Shape::Unresolved { bounds: None },
        };
        let world = World::build(vec![unresolved.clone()], Coverage::Complete, 100, || false)
            .unwrap()
            .ok()
            .unwrap();
        assert!(matches!(
            world
                .query(cube(500.), 17, 0., 100, || false)
                .unwrap()
                .verdict,
            Verdict::Unresolved(_)
        ));
        let hit = World::build(
            vec![unresolved, volume(2, 0.)],
            Coverage::Incomplete,
            100,
            || false,
        )
        .unwrap()
        .ok()
        .unwrap();
        assert_eq!(
            hit.query(cube(0.), 17, 0., 100, || false).unwrap().verdict,
            Verdict::Blocked(Key {
                instance: 2,
                component: 1
            })
        );
        assert_eq!(
            hit.query(cube(0.), 17, 0., 1, || false).unwrap().verdict,
            Verdict::Budget
        );
        assert_eq!(
            hit.query(cube(0.), 17, 0., 100, || true).unwrap().verdict,
            Verdict::Cancelled
        );
    }
    #[test]
    fn index_prunes_large_worlds_and_build_is_transactional() {
        let volumes = (1..=4096)
            .map(|i| volume(i, i as f32 * 10.))
            .collect::<Vec<_>>();
        assert!(matches!(
            World::build(volumes.clone(), Coverage::Complete, 1, || false).unwrap(),
            Err(Verdict::Budget)
        ));
        assert!(matches!(
            World::build(volumes.clone(), Coverage::Complete, 2_000_000, || true).unwrap(),
            Err(Verdict::Cancelled)
        ));
        let world = World::build(volumes, Coverage::Complete, 2_000_000, || false)
            .unwrap()
            .ok()
            .unwrap();
        let result = world.query(cube(9.), 17, 0., 1000, || false).unwrap();
        assert!(matches!(result.verdict, Verdict::Blocked(_)));
        assert!(result.candidates <= 8);
        assert!(result.work < 100);
        assert!(
            World::build(
                vec![volume(1, 0.), volume(1, 10.)],
                Coverage::Complete,
                100,
                || false
            )
            .is_err()
        );
    }
    #[test]
    fn rotated_thin_boxes_can_have_overlapping_bounds_without_intersecting() {
        let angle = std::f32::consts::FRAC_PI_4;
        let rotation = [0., (angle / 2.).sin(), 0., (angle / 2.).cos()];
        let a = Box3 {
            center: [0.; 3],
            rotation,
            extents: [3., 0.2, 0.2],
        };
        let b = Box3 {
            center: [0.6 * angle.sin(), 0., 0.6 * angle.cos()],
            ..a
        };
        assert!(a.bounds().overlaps(b.bounds(), 0.));
        assert!(!overlap(a, b, 0.));
        assert!(overlap(a, b, 0.21));
        let world = World::build(
            vec![Volume {
                key: Key {
                    instance: 1,
                    component: 1,
                },
                mask: 1,
                shape: Shape::Box(a),
            }],
            Coverage::Complete,
            100,
            || false,
        )
        .unwrap()
        .ok()
        .unwrap();
        let result = world.query(b, 1, 0., 100, || false).unwrap();
        assert_eq!(result.verdict, Verdict::Clear);
        assert_eq!(result.candidates, 1);
    }
    #[test]
    fn bounded_unresolved_shapes_do_not_make_unrelated_space_unknown() {
        let volume = Volume {
            key: Key {
                instance: 1,
                component: 1,
            },
            mask: 1,
            shape: Shape::Unresolved {
                bounds: Some(cube(0.).bounds()),
            },
        };
        let world = World::build(vec![volume], Coverage::Complete, 100, || false)
            .unwrap()
            .ok()
            .unwrap();
        assert_eq!(
            world.query(cube(5.), 1, 0., 100, || false).unwrap().verdict,
            Verdict::Clear
        );
        assert!(matches!(
            world.query(cube(0.), 1, 0., 100, || false).unwrap().verdict,
            Verdict::Unresolved(_)
        ));
        assert!(world.query(cube(0.), 1, f64::NAN, 100, || false).is_err());
    }
    #[test]
    fn three_dimensional_edge_separation_requires_cross_product_axes() {
        let a = Box3 {
            center: [0.; 3],
            rotation: [0., 0., 0., 1.],
            extents: [3., 0.2, 0.3],
        };
        let b = Box3 {
            center: [-2.82, -1.69, 0.03],
            rotation: [0.2, 0.4, 0.4, 0.8],
            extents: [2., 0.3, 0.2],
        };
        a.validate().unwrap();
        b.validate().unwrap();
        let aa = a.axes();
        let ba = b.axes();
        let delta = b.center.map(f64::from);
        // All six face-normal projections overlap. A face-only or AABB test
        // would incorrectly block this space; the separating axis is edge/edge.
        for axis in aa.into_iter().chain(ba) {
            let radius = (0..3)
                .map(|i| {
                    f64::from(a.extents[i]) * dot(aa[i], axis).abs()
                        + f64::from(b.extents[i]) * dot(ba[i], axis).abs()
                })
                .sum::<f64>();
            assert!(dot(delta, axis).abs() < radius - 0.05);
        }
        assert!(a.bounds().overlaps(b.bounds(), 0.));
        assert!(!overlap(a, b, 0.));
        assert!(!overlap(b, a, 0.));
        assert!(overlap(a, b, 0.2));
    }
    #[test]
    fn global_unknowns_are_indexed_by_all_mask_bits_instead_of_scanned_per_query() {
        let volumes = (1..=4096)
            .map(|id| Volume {
                key: Key {
                    instance: id,
                    component: 1,
                },
                mask: 0x80000001,
                shape: Shape::Unresolved { bounds: None },
            })
            .collect();
        let world = World::build(volumes, Coverage::Complete, 100000, || false)
            .unwrap()
            .ok()
            .unwrap();
        let result = world
            .query(cube(500.), 0x80000000, 0., 10, || false)
            .unwrap();
        assert_eq!(
            result.verdict,
            Verdict::Unresolved(Key {
                instance: 1,
                component: 1
            })
        );
        assert_eq!(result.work, 2);
        assert_eq!(
            world
                .query(cube(500.), 2, 0., 10, || false)
                .unwrap()
                .verdict,
            Verdict::Clear
        );
        assert_eq!(
            world.query(cube(500.), 0, 0., 1, || false).unwrap().verdict,
            Verdict::Clear
        );
    }
}
