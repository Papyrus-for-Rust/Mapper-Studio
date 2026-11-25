//! Transactional source material paint with retained-filter dependencies.
use crate::{decor, decor_fit, decor_topology, native_layers::Layers};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
pub const VERSION: &str = "papyrus-decor-materials-8";
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Removal {
    pub replacement_channels: [u8; 5],
    pub excluded_topology: u32,
}
impl Removal {
    pub fn validate(self) -> Result<(), String> {
        if self.replacement_channels.iter().any(|&channel| channel > 7)
            || self.excluded_topology.count_ones() != 1
        {
            return Err("Invalid source material removal policy".into());
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug)]
pub enum Operation {
    Set,
    Remove(Removal),
}
#[derive(Clone, Debug)]
pub struct Modifier {
    pub position: [f32; 3],
    pub radius: f32,
    pub fade: f32,
    pub opacity: f32,
    pub channel: u8,
    pub operation: Operation,
}
#[derive(Debug, Serialize, Default)]
pub struct Report {
    pub instances: usize,
    pub operations: usize,
    pub changed_cells: usize,
    pub read_locked_cells: usize,
    pub filter_rejections: usize,
    pub read_conflicts: usize,
}
pub(crate) struct State {
    pub cells: Vec<u8>,
    n: usize,
    locks: Vec<bool>,
    pub report: Report,
}
pub(crate) struct Plan {
    pub writes: Vec<(usize, [u8; 8])>,
    reads: Vec<usize>,
    operations: usize,
}
pub(crate) enum Failure {
    Filter,
    Conflict,
    Outside,
    Budget,
    Unsupported,
}
pub(crate) struct Input<'a> {
    pub modifiers: &'a [Modifier],
    pub dependencies: &'a decor_topology::Prefab,
    pub pose: &'a decor_fit::Pose,
    pub stage: &'a decor::Stage,
    pub size: f32,
    pub terrain: Option<crate::native_layers::View<'a>>,
}
pub fn validate(modifiers: &[Modifier]) -> Result<(), String> {
    if modifiers.len() > 4096
        || modifiers.iter().any(|m| {
            !m.position
                .iter()
                .all(|v| v.is_finite() && v.abs() <= 10000.)
                || !m.radius.is_finite()
                || !(0. ..=256.).contains(&m.radius)
                || !m.fade.is_finite()
                || !(0. ..=256.).contains(&m.fade)
                || !m.opacity.is_finite()
                || !(0. ..=1.).contains(&m.opacity)
                || m.channel > 7
        })
    {
        return Err("Invalid source material paint".into());
    }
    for modifier in modifiers {
        if let Operation::Remove(policy) = modifier.operation {
            policy.validate()?;
        }
    }
    Ok(())
}
fn world(local: [f32; 3], pose: &decor_fit::Pose) -> [f32; 3] {
    let p = decor_fit::rotate(
        pose.rotation,
        std::array::from_fn(|i| local[i] * pose.scale[i]),
    );
    std::array::from_fn(|i| p[i] + pose.position[i])
}
fn paint(old: [u8; 8], channel: usize, opacity: f32) -> [u8; 8] {
    if opacity == 0. {
        return old;
    }
    // Source river painting can leave non-255 totals. Normalize only the
    // touched pixel's relative input, preserving the legacy 255-total math.
    let total = old.iter().map(|&v| v as usize).sum::<usize>();
    if total == 0 {
        // Source absolute road writes can wrap a non-normalized channel to
        // zero (BitUtility.Float2Byte). TerrainSplatSet then lerps its empty
        // target from zero to opacity; it does not invent a base material.
        let mut next = old;
        next[channel] = crate::path_surface::float_byte(opacity);
        return next;
    }
    let scale = if total == 255 {
        1.
    } else {
        255. / total.max(1) as f64
    };
    let values: [f64; 8] = std::array::from_fn(|c| {
        old[c] as f64 * scale * (1. - opacity as f64)
            + if c == channel {
                255. * opacity as f64
            } else {
                0.
            }
    });
    let mut next = values.map(|v| v.floor() as u8);
    let mut order: [usize; 8] = std::array::from_fn(|i| i);
    order.sort_by(|&a, &b| {
        (values[b] - next[b] as f64)
            .total_cmp(&(values[a] - next[a] as f64))
            .then(a.cmp(&b))
    });
    let remainder = 255 - next.iter().map(|v| *v as usize).sum::<usize>();
    for &c in order.iter().take(remainder) {
        next[c] += 1;
    }
    next
}
fn remove(old: [u8; 8], source: usize, target: usize, opacity: f32) -> [u8; 8] {
    let source_value = old[source] as f32 * crate::path_surface::BYTE_TO_FLOAT;
    let target_value = old[target] as f32 * crate::path_surface::BYTE_TO_FLOAT;
    let amount = opacity * source_value;
    let mut next = old;
    next[source] = crate::path_surface::float_byte((source_value - amount).clamp(0., 1.));
    // Preserve the source's last write when both channel indices alias.
    next[target] = crate::path_surface::float_byte((target_value + amount).clamp(0., 1.));
    next
}
impl State {
    pub fn new(layers: &Layers) -> Result<Self, String> {
        let n = layers.surface_resolution;
        let count = n * n;
        if count == 0 || layers.splat.len() != count * 8 {
            return Err("Material paint requires an eight-channel terrain grid".into());
        }
        Ok(Self {
            cells: layers.splat.clone(),
            n,
            locks: vec![false; count],
            report: Report::default(),
        })
    }
    fn pixel(&self, i: usize) -> [u8; 8] {
        std::array::from_fn(|c| self.cells[c * self.n * self.n + i])
    }
    pub fn prepare(
        &self,
        input: Input<'_>,
        visits: &mut usize,
        limit: usize,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Option<Plan>, Failure> {
        if cancelled() {
            return Ok(None);
        }
        let Input {
            modifiers,
            dependencies,
            pose,
            stage,
            size,
            terrain,
        } = input;
        let n = self.n;
        if modifiers
            .iter()
            .any(|m| matches!(m.operation, Operation::Remove(_)))
            && terrain.is_none_or(|view| {
                view.size as f32 != size
                    || view.surface_resolution != n
                    || view.biome.len() != n * n * 5
                    || view.topology.len() != n * n
            })
        {
            return Err(Failure::Unsupported);
        }
        let half = size * 0.5;
        let mut visit = || {
            if *visits >= limit {
                Err(Failure::Budget)
            } else {
                *visits += 1;
                Ok(())
            }
        };
        let mut queries = dependencies.queries.clone();
        if stage.filter_mode & 1 != 0 {
            queries.push(decor_topology::Query {
                check_placement: false,
                position: [0.; 3],
                radius: 0.,
                filter: stage.filter.clone(),
            });
        }
        if stage.filter_mode & 2 != 0 {
            queries.extend(dependencies.anchors.iter().map(|p| decor_topology::Query {
                check_placement: false,
                position: *p,
                radius: 0.,
                filter: stage.filter.clone(),
            }));
        }
        let mut reads = Vec::new();
        for q in queries {
            if cancelled() {
                return Ok(None);
            }
            if q.filter.splat_type == -1 {
                continue;
            }
            let p = world(q.position, pose);
            if p[0].abs() > half || p[2].abs() > half {
                return Err(Failure::Outside);
            }
            let x = (p[0] / size + 0.5) * (n - 1) as f32;
            let z = (p[2] / size + 0.5) * (n - 1) as f32;
            let (a, b) = (x.floor() as usize, z.floor() as usize);
            let indices = [
                b * n + a,
                b * n + (a + 1).min(n - 1),
                (b + 1).min(n - 1) * n + a,
                (b + 1).min(n - 1) * n + (a + 1).min(n - 1),
            ];
            let weights = [
                (1. - x.fract()) * (1. - z.fract()),
                x.fract() * (1. - z.fract()),
                (1. - x.fract()) * z.fract(),
                x.fract() * z.fract(),
            ];
            let mut factor = 0.;
            for (i, w) in indices.into_iter().zip(weights) {
                if w == 0. {
                    continue;
                }
                visit()?;
                reads.push(i);
                factor += (0..8)
                    .filter(|c| q.filter.splat_type as u32 & (1 << c) != 0)
                    .map(|c| self.cells[c * n * n + i] as f32 / 255. * w)
                    .sum::<f32>();
            }
            if factor <= 0. {
                return Err(Failure::Filter);
            }
            if reads.len() > 262144 {
                return Err(Failure::Unsupported);
            }
        }
        let index = |w: f32| ((w / size + 0.5) * n as f32).clamp(0., (n - 1) as f32) as usize;
        let mut writes = BTreeMap::new();
        for m in modifiers {
            let p = world(m.position, pose);
            let radius = m.radius * pose.scale[1];
            let fade = m.fade * pose.scale[1];
            if !radius.is_finite() || radius > 512. || !fade.is_finite() || fade > 512. {
                return Err(Failure::Unsupported);
            }
            if p[0].abs() + radius > half || p[2].abs() + radius > half {
                return Err(Failure::Outside);
            }
            let target = if let Operation::Remove(policy) = m.operation {
                let view = terrain.ok_or(Failure::Unsupported)?;
                let center = index(p[2]) * n + index(p[0]);
                visit()?;
                if view.topology[center] as u32 & policy.excluded_topology != 0 {
                    continue;
                }
                let mut maximum = 0;
                let mut selected = 0;
                for channel in 0..5 {
                    visit()?;
                    let value = view.biome[channel * n * n + center];
                    if value >= maximum {
                        maximum = value;
                        selected = channel;
                    }
                }
                Some(policy.replacement_channels[selected] as usize)
            } else {
                None
            };
            let cx = (p[0] / size + 0.5) * n as f32;
            let cz = (p[2] / size + 0.5) * n as f32;
            let r = radius / size * n as f32;
            let f = fade / size * n as f32;
            for z in index(p[2] - radius)..=index(p[2] + radius) {
                if cancelled() {
                    return Ok(None);
                }
                for x in index(p[0] - radius)..=index(p[0] + radius) {
                    visit()?;
                    let d = (x as f32 + 0.5 - cx).hypot(z as f32 + 0.5 - cz);
                    let weight = if f == 0. {
                        if d < r { 1. } else { 0. }
                    } else {
                        ((r - d) / f).clamp(0., 1.)
                    };
                    let t = weight * m.opacity;
                    if t <= 0. {
                        continue;
                    }
                    let i = z * n + x;
                    let old = *writes.get(&i).unwrap_or(&self.pixel(i));
                    let next = if let Some(target) = target {
                        remove(old, m.channel as usize, target, t)
                    } else {
                        paint(old, m.channel as usize, t)
                    };
                    writes.insert(i, next);
                    if writes.len() > 262144 {
                        return Err(Failure::Unsupported);
                    }
                }
            }
        }
        writes.retain(|i, v| *v != self.pixel(*i));
        if writes.keys().any(|i| self.locks[*i]) {
            return Err(Failure::Conflict);
        }
        reads.sort_unstable();
        reads.dedup();
        Ok(Some(Plan {
            writes: writes.into_iter().collect(),
            reads,
            operations: modifiers.len(),
        }))
    }
    pub fn commit(&mut self, plan: Plan) {
        self.report.instances += usize::from(plan.operations > 0);
        self.report.operations += plan.operations;
        self.report.changed_cells += plan.writes.len();
        for (i, v) in plan.writes {
            for (c, byte) in v.into_iter().enumerate() {
                self.cells[c * self.n * self.n + i] = byte;
            }
        }
        for i in plan.reads {
            if !self.locks[i] {
                self.locks[i] = true;
                self.report.read_locked_cells += 1;
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn removal_matches_owned_server_callback_including_aliases_and_non_normalized_pixels() {
        #[derive(Deserialize)]
        struct Case {
            before: [u8; 8],
            source: usize,
            target: usize,
            opacity: f32,
            weight: f32,
            after: [u8; 8],
        }
        #[derive(Deserialize)]
        struct Fixture {
            assembly_sha256: String,
            cases: Vec<Case>,
        }
        let fixture: Fixture =
            serde_json::from_str(include_str!("../tests/fixtures/splat-remove-bd19c4ac.json"))
                .unwrap();
        assert_eq!(
            fixture.assembly_sha256,
            "bd19c4ac705edd18ff556f5dbb3130e098b380577cc2ab0d73a2ec89ee289cc9"
        );
        assert_eq!(fixture.cases.len(), 432);
        for case in fixture.cases {
            assert_eq!(
                remove(
                    case.before,
                    case.source,
                    case.target,
                    case.weight * case.opacity
                ),
                case.after
            );
        }
    }
    fn removal_policy() -> Removal {
        Removal {
            replacement_channels: [2, 4, 4, 1, 0],
            excluded_topology: 8,
        }
    }
    fn removal_layers() -> Layers {
        let mut splat = vec![0; 32 * 32 * 8];
        splat[3 * 32 * 32..4 * 32 * 32].fill(255);
        let mut biome = vec![0; 32 * 32 * 5];
        biome[..32 * 32].fill(255);
        Layers {
            size: 32,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: 33,
            surface_resolution: 32,
            height: vec![16000; 33 * 33],
            biome,
            splat,
            topology: vec![1; 32 * 32],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        }
    }
    #[test]
    fn removal_uses_transformed_center_biome_for_the_whole_brush_and_later_ties_win() {
        for biome in 0..=5 {
            let mut layers = removal_layers();
            let center = 13 * 32 + 16;
            for channel in 0..5 {
                layers.biome[channel * 32 * 32 + center] = 0;
            }
            if biome < 5 {
                layers.biome[biome * 32 * 32 + center] = 255;
            }
            let state = State::new(&layers).unwrap();
            let mut transform = pose();
            let half = 90_f32.to_radians() * 0.5;
            transform.rotation = [0., half.sin(), 0., half.cos()];
            let mut modifier = modifier(3);
            modifier.position = [3., 0., 0.];
            modifier.operation = Operation::Remove(removal_policy());
            modifier.opacity = 0.5;
            let modifiers = [modifier];
            let plan = state
                .prepare(
                    Input {
                        modifiers: &modifiers,
                        dependencies: &decor_topology::Prefab::default(),
                        pose: &transform,
                        stage: &stage(),
                        size: 32.,
                        terrain: Some(layers.view()),
                    },
                    &mut 0,
                    100000,
                    &|| false,
                )
                .ok()
                .flatten()
                .unwrap();
            assert!(!plan.writes.is_empty());
            let target = removal_policy().replacement_channels[biome.min(4)] as usize;
            for (_, pixel) in plan.writes {
                assert!(pixel[target] > 0 && pixel[3] < 255);
                assert!(
                    pixel
                        .iter()
                        .enumerate()
                        .all(|(c, &v)| c == 3 || c == target || v == 0)
                );
            }
            assert_eq!(state.cells, layers.splat);
        }
    }
    #[test]
    fn removal_retains_component_order_exclusions_work_bounds_and_atomic_reads() {
        let mut layers = removal_layers();
        layers.splat.fill(0);
        layers.splat[4 * 1024..5 * 1024].fill(255);
        let state = State::new(&layers).unwrap();
        let mut removal = modifier(3);
        removal.operation = Operation::Remove(removal_policy());
        let dependencies = decor_topology::Prefab::default();
        let transform = pose();
        let stage = stage();
        let modifiers = [modifier(3), removal];
        let input = |terrain| Input {
            modifiers: &modifiers,
            dependencies: &dependencies,
            pose: &transform,
            stage: &stage,
            size: 32.,
            terrain,
        };
        let plan = state
            .prepare(input(Some(layers.view())), &mut 0, 100000, &|| false)
            .ok()
            .flatten()
            .unwrap();
        assert!(!plan.writes.is_empty());
        assert!(
            plan.writes
                .iter()
                .all(|(_, pixel)| pixel[2] == 255 && pixel[3] == 0)
        );
        assert!(matches!(
            state.prepare(input(None), &mut 0, 100000, &|| false),
            Err(Failure::Unsupported)
        ));
        assert!(matches!(
            state.prepare(
                Input {
                    modifiers: &modifiers[1..],
                    ..input(Some(layers.view()))
                },
                &mut 0,
                5,
                &|| false
            ),
            Err(Failure::Budget)
        ));
        assert!(
            state
                .prepare(input(Some(layers.view())), &mut 0, 100000, &|| true)
                .ok()
                .flatten()
                .is_none()
        );
        let mut blocked = layers.topology.clone();
        blocked[16 * 32 + 16] |= 8;
        let view = crate::native_layers::View {
            topology: &blocked,
            ..layers.view()
        };
        let plan = state
            .prepare(input(Some(view)), &mut 0, 100000, &|| false)
            .ok()
            .flatten()
            .unwrap();
        assert!(
            plan.writes
                .iter()
                .all(|(_, pixel)| pixel[3] == 255 && pixel[2] == 0)
        );
        assert_eq!(state.cells, layers.splat);
        assert!(state.locks.iter().all(|locked| !locked));
    }
    fn state() -> State {
        let n = 32;
        let mut cells = vec![0; n * n * 8];
        cells[..n * n].fill(255);
        State {
            cells,
            n,
            locks: vec![false; n * n],
            report: Report::default(),
        }
    }
    fn stage() -> decor::Stage {
        serde_json::from_value(serde_json::json!({"id":"one","order":0,"name":"Paint","kind":"white_noise","folder":"decor/rocks","enabled":true,"mode":-1,"filter":{"SplatType":-1,"BiomeType":-1,"TopologyAny":-1,"TopologyAll":0,"TopologyNot":0},"filter_mode":0,"anchor_mode":0,"distance":0.,"dithering":0.,"density":10.,"cluster":null,"prefabs":["a"]})).unwrap()
    }
    fn pose() -> decor_fit::Pose {
        decor_fit::Pose {
            candidate: 0,
            position: [0.; 3],
            rotation: [0., 0., 0., 1.],
            scale: [1.; 3],
        }
    }
    fn modifier(channel: u8) -> Modifier {
        Modifier {
            position: [0.; 3],
            radius: 4.,
            fade: 0.,
            opacity: 1.,
            channel,
            operation: Operation::Set,
        }
    }
    fn plan(
        s: &State,
        m: &[Modifier],
        d: &decor_topology::Prefab,
        p: &decor_fit::Pose,
        stage: &decor::Stage,
    ) -> Result<Option<Plan>, Failure> {
        s.prepare(
            Input {
                modifiers: m,
                dependencies: d,
                pose: p,
                stage,
                size: 32.,
                terrain: None,
            },
            &mut 0,
            100000,
            &|| false,
        )
    }
    #[test]
    fn normalized_blending_retains_unpainted_ratios_and_component_order() {
        assert_eq!(
            paint([255, 0, 0, 0, 0, 0, 0, 0], 1, 0.5),
            [128, 127, 0, 0, 0, 0, 0, 0]
        );
        for c in 0..8 {
            for t in [0., 0.1, 0.25, 0.55, 1.] {
                let out = paint([63, 64, 128, 0, 0, 0, 0, 0], c, t);
                assert_eq!(out.iter().map(|v| *v as usize).sum::<usize>(), 255);
                if t == 1. {
                    assert_eq!(out[c], 255);
                }
            }
        }
        let mut s = state();
        let original = s.cells.clone();
        let d = decor_topology::Prefab::default();
        let p = pose();
        let st = stage();
        let candidate = plan(&s, &[modifier(1), modifier(2)], &d, &p, &st)
            .ok()
            .flatten()
            .unwrap();
        assert!(!candidate.writes.is_empty());
        assert!(candidate.writes.iter().all(|(_, v)| v[2] == 255));
        assert_eq!(s.cells, original);
        s.commit(candidate);
        assert!(s.report.changed_cells > 0);
        assert_eq!(s.report.operations, 2);
    }
    #[test]
    fn source_river_weights_survive_outside_paint_and_zero_opacity_is_exact() {
        let mut s = state();
        let count = s.n * s.n;
        s.cells[0] = 243;
        s.cells[1] = 250;
        s.cells[count + 1] = 8;
        s.cells[16 * s.n + 16] = 243;
        let layers = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: 33,
            surface_resolution: s.n,
            height: vec![0; 33 * 33],
            biome: vec![0; count * 5],
            splat: s.cells.clone(),
            topology: vec![0; count],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        let mut s = State::new(&layers).unwrap();
        let original = s.cells.clone();
        let mut m = modifier(2);
        m.opacity = 0.5;
        let candidate = plan(
            &s,
            &[m],
            &decor_topology::Prefab::default(),
            &pose(),
            &stage(),
        )
        .ok()
        .flatten()
        .unwrap();
        assert!(!candidate.writes.is_empty());
        for (_, bytes) in &candidate.writes {
            assert_eq!(bytes.iter().map(|&v| v as usize).sum::<usize>(), 255);
        }
        assert_eq!(s.cells, original);
        s.commit(candidate);
        assert_eq!(s.cells[0], 243);
        assert_eq!(s.cells[1], 250);
        assert_eq!(s.cells[count + 1], 8);
        for old in [[243, 0, 0, 0, 0, 0, 0, 0], [250, 8, 0, 0, 0, 0, 0, 0]] {
            assert_eq!(paint(old, 2, 0.), old);
            for opacity in [0.01, 0.25, 0.5, 1.] {
                let result = paint(old, 2, opacity);
                assert_eq!(result.iter().map(|&v| v as usize).sum::<usize>(), 255);
            }
        }
        let mut invalid = layers;
        for c in 0..8 {
            invalid.splat[c * count] = 0;
        }
        let empty = State::new(&invalid).unwrap();
        assert_eq!(empty.pixel(0), [0; 8]);
        invalid.splat.pop();
        assert!(State::new(&invalid).is_err());
    }

    #[test]
    fn source_road_empty_cell_survives_until_a_material_modifier_paints_it() {
        // The source's unchecked byte conversion can clear a formerly valid
        // non-normalized river/monument pixel during an absolute road write.
        let mut pixel = [1, 255, 0, 0, 0, 0, 0, 0];
        crate::path_surface::set_splat(&mut pixel, 1, 0, 0, 0.);
        assert_eq!(pixel, [0; 8]);
        for channel in 0..8 {
            assert_eq!(paint(pixel, channel, 0.), pixel);
            // Owned server BitUtility reference: bd19c4ac/04fbac3b.
            for (opacity, expected) in [(0.001, 0), (0.01, 3), (0.25, 64), (0.5, 128), (1., 255)] {
                let result = paint(pixel, channel, opacity);
                assert_eq!(result[channel], expected);
                assert!(
                    result
                        .iter()
                        .enumerate()
                        .all(|(i, &v)| i == channel || v == 0)
                );
            }
        }
        let mut s = state();
        let cells = s.n * s.n;
        let center = 16 * s.n + 16;
        s.cells[0] = 0;
        s.cells[center] = 0;
        let original = s.cells.clone();
        let candidate = plan(
            &s,
            &[modifier(2)],
            &decor_topology::Prefab::default(),
            &pose(),
            &stage(),
        )
        .ok()
        .flatten()
        .unwrap();
        assert_eq!(s.cells, original);
        s.commit(candidate);
        assert_eq!(s.pixel(0), [0; 8]);
        assert_eq!(s.cells[2 * cells + center], 255);
        assert_eq!(
            s.pixel(center).iter().map(|&v| v as usize).sum::<usize>(),
            255
        );
    }

    #[test]
    fn material_filters_recheck_current_weights_and_lock_bilinear_dependencies() {
        let mut s = state();
        let mut st = stage();
        st.filter_mode = 1;
        st.filter.splat_type = 1;
        let d = decor_topology::Prefab::default();
        let mut p = pose();
        p.position = [0.13, 0., 0.27];
        let read = plan(&s, &[], &d, &p, &st).ok().flatten().unwrap();
        s.commit(read);
        assert_eq!(s.report.read_locked_cells, 4);
        let before = s.cells.clone();
        assert!(matches!(
            plan(&s, &[modifier(1)], &d, &p, &stage()),
            Err(Failure::Conflict)
        ));
        assert_eq!(s.cells, before);
        // A placement's own paint follows its own checks; only earlier accepted reads lock it.
        let mut s = state();
        let own = plan(&s, &[modifier(1)], &d, &p, &st)
            .ok()
            .flatten()
            .unwrap();
        s.commit(own);
        assert!(matches!(plan(&s, &[], &d, &p, &st), Err(Failure::Filter)));
    }
    #[test]
    fn paint_uses_transformed_centers_y_scale_and_circular_fade_not_rotated_aabb() {
        let s = state();
        let mut p = pose();
        let a = 37_f32.to_radians();
        p.rotation = [0., (a / 2.).sin(), 0., (a / 2.).cos()];
        p.scale = [2., 1.5, 0.5];
        let m = Modifier {
            position: [3., 0., 1.],
            radius: 2.,
            fade: 0.,
            opacity: 1.,
            channel: 1,
            operation: Operation::Set,
        };
        let cx = 6. * a.cos() + 0.5 * a.sin();
        let cz = -6. * a.sin() + 0.5 * a.cos();
        let plan = plan(&s, &[m], &decor_topology::Prefab::default(), &p, &stage())
            .ok()
            .flatten()
            .unwrap();
        assert!(plan.writes.len() > 10);
        for (i, v) in &plan.writes {
            let x = (*i % s.n) as f32 + 0.5 - 16.;
            let z = (*i / s.n) as f32 + 0.5 - 16.;
            assert!((x - cx).hypot(z - cz) < 3.00001);
            assert_eq!(v[1], 255);
        }
    }
    #[test]
    fn cancelled_budgeted_and_invalid_paint_cannot_change_state() {
        let s = state();
        let before = s.cells.clone();
        let m = [modifier(1)];
        let d = decor_topology::Prefab::default();
        let p = pose();
        let st = stage();
        let input = || Input {
            modifiers: &m,
            dependencies: &d,
            pose: &p,
            stage: &st,
            size: 32.,
            terrain: None,
        };
        assert!(matches!(
            s.prepare(input(), &mut 0, 1, &|| false),
            Err(Failure::Budget)
        ));
        assert!(
            s.prepare(input(), &mut 0, 100000, &|| true)
                .ok()
                .flatten()
                .is_none()
        );
        assert_eq!(s.cells, before);
        assert_eq!(s.report.changed_cells, 0);
        assert_eq!(s.report.read_locked_cells, 0);
        let mut invalid = modifier(8);
        assert!(validate(&[invalid.clone()]).is_err());
        invalid.channel = 1;
        invalid.opacity = f32::NAN;
        assert!(validate(&[invalid]).is_err());
    }
    #[test]
    fn radial_fade_and_opacity_apply_without_painting_the_square_corners() {
        let s = state();
        let mut m = modifier(1);
        m.fade = 2.;
        m.opacity = 0.55;
        let candidate = plan(
            &s,
            &[m.clone()],
            &decor_topology::Prefab::default(),
            &pose(),
            &stage(),
        )
        .ok()
        .flatten()
        .unwrap();
        for (i, v) in candidate.writes {
            let x = (i % s.n) as f32 + 0.5 - 16.;
            let z = (i / s.n) as f32 + 0.5 - 16.;
            let d = x.hypot(z);
            assert!(d < 4.);
            let expected = 255. * ((4. - d) / 2.).clamp(0., 1.) * 0.55;
            assert!((v[1] as f32 - expected).abs() <= 1.);
        }
        m.fade = 6.;
        m.opacity = 1.;
        let candidate = plan(
            &s,
            &[m],
            &decor_topology::Prefab::default(),
            &pose(),
            &stage(),
        )
        .ok()
        .flatten()
        .unwrap();
        assert!(candidate.writes.iter().all(|(_, v)| v[1] < 170));
    }
}
