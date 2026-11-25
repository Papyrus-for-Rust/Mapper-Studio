//! Data-only evaluation of serialized terrain spawn filters. No runtime entities,
//! physics, navmesh, placement-map occupancy or spawn density simulation.
use crate::{
    erosion::decode,
    native_layers::{Layers, dimensions},
};
use serde::{Deserialize, Serialize};
pub const VERSION: &str = "papyrus-spawn-audit-1";
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
pub struct Filter {
    pub splat_type: i32,
    pub biome_type: i32,
    pub topology_any: i32,
    pub topology_all: i32,
    pub topology_not: i32,
}
impl Filter {
    pub fn validate(&self) -> Result<(), String> {
        if self.splat_type == 0 || self.biome_type == 0 || self.topology_any == 0 {
            return Err("Empty terrain spawn filter mask".into());
        }
        Ok(())
    }
    pub fn topology_matches(&self, value: u32) -> bool {
        (self.topology_any == -1 || value & self.topology_any as u32 != 0)
            && value & self.topology_all as u32 == self.topology_all as u32
            && value & self.topology_not as u32 == 0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Character,
    Population,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub name: String,
    pub kind: Kind,
    pub filter: Filter,
    pub cutoff: f32,
    pub radius: f32,
}
impl Rule {
    pub fn validate(&self) -> Result<(), String> {
        self.filter.validate()?;
        if self.name.is_empty()
            || self.name.len() > 256
            || !self.cutoff.is_finite()
            || !(0. ..=1.).contains(&self.cutoff)
            || !self.radius.is_finite()
            || !(0. ..=100.).contains(&self.radius)
        {
            return Err("Invalid terrain spawn rule".into());
        }
        Ok(())
    }
}
/// Conservative horizontal authored obstacle. Its exclusion is additional to the
/// serialized filter and cannot substitute for MonumentInfo.Distance/PhysX.
pub struct Obstacle {
    pub center: [f32; 2],
    pub extent: [f32; 2],
    pub yaw: f32,
}
#[derive(Debug, Serialize)]
pub struct FlagCount {
    pub name: String,
    pub mask: u32,
    pub cells: usize,
}
#[derive(Debug, Serialize)]
pub struct RuleCount {
    pub name: String,
    pub kind: Kind,
    pub filter: Filter,
    pub cutoff: f32,
    pub radius: f32,
    pub resolution: usize,
    pub distribution_resolution: usize,
    pub passing_samples: usize,
    pub sampled_area_m2: f64,
    pub radius_rejected: usize,
}
#[derive(Debug, Serialize)]
pub struct Audit {
    pub engine: &'static str,
    pub source_sha256: String,
    pub resolution: usize,
    pub unknown_topology_cells: usize,
    pub flags: Vec<FlagCount>,
    pub rules: Vec<RuleCount>,
    pub character_filter_cells: usize,
    pub character_candidate_cells: usize,
    pub character_rejected_height: usize,
    pub character_rejected_alpha: usize,
    pub character_rejected_clearance: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub character_rejected_decoration: Option<usize>,
    pub physics_validated: bool,
    pub source_issues: Vec<String>,
    pub fixed_point_populations: Vec<String>,
    #[serde(skip)]
    pub character_mask: Vec<u8>,
}
impl Audit {
    pub fn json(&self) -> Result<serde_json::Value, String> {
        serde_json::to_value(self).map_err(|e| e.to_string())
    }
    /// Conservative Mapper exclusion, separate from source filters and PhysX.
    /// Cancellation or invalid input leaves this audit unchanged.
    pub fn exclude_decoration(
        &mut self,
        blocked: &[u8],
        cancelled: impl Fn() -> bool,
    ) -> Result<bool, String> {
        if self.resolution == 0
            || self.resolution.checked_mul(self.resolution) != Some(self.character_mask.len())
            || blocked.len() != self.character_mask.len()
            || self.character_rejected_decoration.is_some()
        {
            return Err("Invalid or repeated decoration spawn exclusion".into());
        }
        let mut rejected = Vec::new();
        for (i, (&b, &candidate)) in blocked.iter().zip(&self.character_mask).enumerate() {
            if i % self.resolution == 0 && cancelled() {
                return Ok(false);
            }
            if b > 1 {
                return Err("Invalid decoration blocker cell".into());
            }
            if b != 0 && candidate != 0 {
                rejected.push(i);
            }
        }
        if cancelled() {
            return Ok(false);
        }
        self.character_candidate_cells -= rejected.len();
        self.character_rejected_decoration = Some(rejected.len());
        for i in rejected {
            self.character_mask[i] = 0;
        }
        Ok(true)
    }
}
fn index(n: usize, u: f32, v: f32) -> usize {
    let x = (u * n as f32).floor().clamp(0., (n - 1) as f32) as usize;
    let z = (v * n as f32).floor().clamp(0., (n - 1) as f32) as usize;
    z * n + x
}
struct SplatSample {
    indices: [usize; 4],
    x: f32,
    z: f32,
}

impl SplatSample {
    fn new(n: usize, u: f32, v: f32) -> Self {
        let intervals = (n - 1) as f32;
        let x = u.clamp(0., 1.) * intervals;
        let z = v.clamp(0., 1.) * intervals;
        // Clamped coordinates fit in u32. Truncation provides both the
        // lower sample index and the base for the interpolation fraction.
        let a = x as u32;
        let b = z as u32;
        let fx = x - a as f32;
        let fz = z - b as f32;
        let (a, b) = (a as usize, b as usize);
        let c = (a + 1).min(n - 1);
        let d = (b + 1).min(n - 1);
        Self {
            indices: [b * n + a, b * n + c, d * n + a, d * n + c],
            x: fx,
            z: fz,
        }
    }

    fn channel(&self, data: &[u8], offset: usize) -> f32 {
        let [a, b, c, d] = self.indices.map(|i| data[offset + i] as f32 / 255.);
        let lo = a * (1. - self.x) + b * self.x;
        let hi = c * (1. - self.x) + d * self.x;
        lo * (1. - self.z) + hi * self.z
    }
}
pub(crate) fn factor(layers: &Layers, filter: &Filter, u: f32, v: f32) -> f32 {
    factor_view(layers.view(), filter, u, v)
}
pub(crate) fn placement_factor_view(
    layers: crate::native_layers::View<'_>,
    filter: &Filter,
    u: f32,
    v: f32,
) -> f32 {
    if layers.placement_blocked(u, v) {
        0.
    } else {
        factor_view(layers, filter, u, v)
    }
}
pub(crate) fn factor_view(
    layers: crate::native_layers::View<'_>,
    filter: &Filter,
    u: f32,
    v: f32,
) -> f32 {
    factor_view_at(layers, filter, index(layers.surface_resolution, u, v), u, v)
}

/// Evaluate a filter with the surface cell already resolved for the same UV.
pub(crate) fn factor_view_at(
    layers: crate::native_layers::View<'_>,
    filter: &Filter,
    i: usize,
    u: f32,
    v: f32,
) -> f32 {
    if !filter.topology_matches(layers.topology[i] as u32) {
        return 0.;
    }
    material_factor_at(layers, filter, i, u, v)
}

/// Evaluate splat and biome weights after the caller has checked topology.
#[inline]
pub(crate) fn material_factor_at(
    layers: crate::native_layers::View<'_>,
    filter: &Filter,
    i: usize,
    u: f32,
    v: f32,
) -> f32 {
    let n = layers.surface_resolution;
    let cells = n * n;
    if filter.biome_type != -1 {
        let mut best = 0;
        let mut value = 0;
        // The inspected implementation resolves equal maxima to the later channel.
        for c in 0..5 {
            let b = layers.biome[c * cells + i];
            if b >= value {
                best = c;
                value = b;
            }
        }
        if (1_u32 << best) & filter.biome_type as u32 == 0 {
            return 0.;
        }
    }
    if filter.splat_type == -1 {
        return 1.;
    }
    let sample = SplatSample::new(n, u, v);
    let mask = filter.splat_type as u8;
    if mask.is_power_of_two() {
        return sample
            .channel(layers.splat, mask.trailing_zeros() as usize * cells)
            .clamp(0., 1.);
    }
    (0..8)
        .filter(|c| mask & (1 << c) != 0)
        .map(|c| sample.channel(layers.splat, c * cells))
        .sum::<f32>()
        .clamp(0., 1.)
}
pub(crate) fn height(layers: &Layers, u: f32, v: f32) -> f32 {
    height_view(layers.view(), u, v)
}
pub(crate) fn height_view(layers: crate::native_layers::View<'_>, u: f32, v: f32) -> f32 {
    height_view_with(layers, u, v, decode)
}

#[inline]
pub(crate) fn height_view_with(
    layers: crate::native_layers::View<'_>,
    u: f32,
    v: f32,
    decode: impl Fn(i16) -> f32,
) -> f32 {
    HeightPoint::new(layers.height_resolution, u, v).sample(layers, decode)
}

pub(crate) struct HeightPoint {
    vertex: [usize; 2],
    fraction: [f32; 2],
}

impl HeightPoint {
    #[inline]
    pub(crate) fn new(resolution: usize, u: f32, v: f32) -> Self {
        let intervals = (resolution - 1) as u32 as f32;
        let [x, z] = [u * intervals, v * intervals];
        // Native height rasters have at most 4097 vertices along each axis.
        // Truncation also supplies the base for the interpolation fraction.
        let [a, b] = [x, z].map(|value| value as u32);
        Self {
            vertex: [a as usize, b as usize],
            fraction: [x - a as f32, z - b as f32],
        }
    }

    #[inline]
    pub(crate) fn surface_cell(&self, side: usize, shift: usize) -> usize {
        let [x, z] = self.vertex.map(|vertex| (vertex >> shift).min(side - 1));
        z * side + x
    }

    #[inline]
    pub(crate) fn sample(
        self,
        layers: crate::native_layers::View<'_>,
        decode: impl Fn(i16) -> f32,
    ) -> f32 {
        self.sample_grid(layers.height_resolution, layers.height, decode)
    }

    #[inline]
    pub(crate) fn sample_grid(
        self,
        resolution: usize,
        values: &[i16],
        decode: impl Fn(i16) -> f32,
    ) -> f32 {
        self.interpolate(self.taps(resolution, values).map(decode))
    }

    #[inline]
    fn taps(&self, n: usize, values: &[i16]) -> [i16; 4] {
        let [a, b] = self.vertex;
        let c = (a + 1).min(n - 1);
        let d = (b + 1).min(n - 1);
        [
            values[b * n + a],
            values[b * n + c],
            values[d * n + a],
            values[d * n + c],
        ]
    }

    #[inline]
    pub(crate) fn interpolate(&self, [a, b, c, d]: [f32; 4]) -> f32 {
        let [tx, tz] = self.fraction;
        let lo = a * (1. - tx) + b * tx;
        let hi = c * (1. - tx) + d * tx;
        lo * (1. - tz) + hi * tz
    }
}
/// Checked point sampling for installation adapters outside this crate.
pub fn sample_height(layers: &Layers, u: f32, v: f32) -> Result<f32, String> {
    sample_height_view(layers.view(), u, v)
}
pub fn sample_height_view(
    layers: crate::native_layers::View<'_>,
    u: f32,
    v: f32,
) -> Result<f32, String> {
    let (hr, _) = dimensions(layers.size)?;
    if layers.height_resolution != hr
        || layers.height.len() != hr * hr
        || !u.is_finite()
        || !v.is_finite()
        || !(0. ..=1.).contains(&u)
        || !(0. ..=1.).contains(&v)
    {
        return Err("Invalid terrain height sample".into());
    }
    sample_height_taps(layers, u, v)
}

/// The caller checks the grid dimensions and coordinates before sampling.
pub(crate) fn sample_height_taps(
    layers: crate::native_layers::View<'_>,
    u: f32,
    v: f32,
) -> Result<f32, String> {
    let point = HeightPoint::new(layers.height_resolution, u, v);
    let taps = point.taps(layers.height_resolution, layers.height);
    if taps.iter().any(|h| !(0..=32766).contains(h)) {
        return Err("Invalid sampled height value".into());
    }
    Ok(point.interpolate(taps.map(decode)))
}
pub fn sample_factor(layers: &Layers, filter: &Filter, u: f32, v: f32) -> Result<f32, String> {
    sample_factor_view(layers.view(), filter, u, v)
}
pub fn sample_factor_view(
    layers: crate::native_layers::View<'_>,
    filter: &Filter,
    u: f32,
    v: f32,
) -> Result<f32, String> {
    filter.validate()?;
    let (_, n) = dimensions(layers.size)?;
    if layers.surface_resolution != n
        || layers.placement_blockers.is_some_and(|b| b.len() != n * n)
        || layers.topology.len() != n * n
        || layers.biome.len() != n * n * 5
        || layers.splat.len() != n * n * 8
        || !u.is_finite()
        || !v.is_finite()
        || !(0. ..=1.).contains(&u)
        || !(0. ..=1.).contains(&v)
    {
        return Err("Invalid terrain filter sample".into());
    }
    Ok(placement_factor_view(layers, filter, u, v))
}
pub struct Definition<'a> {
    pub source_sha256: &'a str,
    pub flags: &'a [(String, u32)],
    pub rules: &'a [Rule],
    pub clearance: f32,
}
pub fn audit(
    layers: &Layers,
    alpha: &[u8],
    obstacles: &[Obstacle],
    definition: Definition<'_>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Audit>, String> {
    let (hr, n) = dimensions(layers.size)?;
    let cells = n * n;
    if layers.height_resolution != hr
        || layers.surface_resolution != n
        || layers.height.len() != hr * hr
        || layers.biome.len() != cells * 5
        || layers.splat.len() != cells * 8
        || layers.topology.len() != cells
        || alpha.len() != cells
        || definition.rules.is_empty()
        || definition.rules.len() > 128
        || definition
            .rules
            .iter()
            .filter(|r| r.kind == Kind::Character)
            .count()
            != 1
        || definition.flags.is_empty()
        || definition.flags.len() > 32
        || definition.flags.iter().any(|(_, v)| v.count_ones() != 1)
        || !definition.clearance.is_finite()
        || !(0. ..=500.).contains(&definition.clearance)
        || obstacles.len() > 128
        || layers.height.iter().any(|h| !(0..=32766).contains(h))
    {
        return Err("Invalid terrain spawn audit dimensions or definition".into());
    }
    for rule in definition.rules {
        rule.validate()?;
    }
    let mut known = 0_u32;
    let mut names = std::collections::BTreeSet::new();
    for (name, flag) in definition.flags {
        if name.is_empty() || name.len() > 64 || !names.insert(name) || known & flag != 0 {
            return Err("Invalid topology label dictionary".into());
        }
        known |= flag;
    }
    let mut counts = [0_usize; 32];
    let mut unknown = 0;
    for (i, &word) in layers.topology.iter().enumerate() {
        if i % 16384 == 0 && cancelled() {
            return Ok(None);
        }
        let mut bits = word as u32;
        unknown += usize::from(bits & !known != 0);
        while bits != 0 {
            counts[bits.trailing_zeros() as usize] += 1;
            bits &= bits - 1;
        }
    }
    let mut out = Audit {
        engine: VERSION,
        source_sha256: definition.source_sha256.into(),
        resolution: n,
        unknown_topology_cells: unknown,
        flags: definition
            .flags
            .iter()
            .map(|(name, mask)| FlagCount {
                name: name.clone(),
                mask: *mask,
                cells: counts[mask.trailing_zeros() as usize],
            })
            .collect(),
        rules: Vec::new(),
        character_filter_cells: 0,
        character_candidate_cells: 0,
        character_rejected_height: 0,
        character_rejected_alpha: 0,
        character_rejected_clearance: 0,
        character_rejected_decoration: None,
        physics_validated: false,
        source_issues: Vec::new(),
        fixed_point_populations: Vec::new(),
        character_mask: vec![0; cells],
    };
    let mut prepared = Vec::new();
    for o in obstacles {
        if !o.yaw.is_finite()
            || o.center
                .iter()
                .chain(&o.extent)
                .any(|v| !v.is_finite() || v.abs() > 10000.)
            || o.extent.iter().any(|v| *v < 0.)
        {
            return Err("Invalid spawn-clearance obstacle".into());
        }
        let (s, c) = o.yaw.to_radians().sin_cos();
        prepared.push((o.center, o.extent, s, c));
    }
    for rule in definition.rules {
        let distribution_resolution = (layers.size as usize
            / if rule.kind == Kind::Character { 2 } else { 4 })
        .next_power_of_two();
        let res = if rule.kind == Kind::Character {
            n
        } else {
            distribution_resolution.min(256)
        };
        let mut stats = RuleCount {
            name: rule.name.clone(),
            kind: rule.kind,
            filter: rule.filter.clone(),
            cutoff: rule.cutoff,
            radius: rule.radius,
            resolution: res,
            distribution_resolution,
            passing_samples: 0,
            sampled_area_m2: 0.,
            radius_rejected: 0,
        };
        for z in 0..res {
            if cancelled() {
                return Ok(None);
            }
            let v = (z as f32 + 0.5) / res as f32;
            for x in 0..res {
                let u = (x as f32 + 0.5) / res as f32;
                if factor(layers, &rule.filter, u, v) <= rule.cutoff {
                    continue;
                }
                if rule.radius > 0. {
                    let r = rule.radius / layers.size as f32;
                    if [(u - r, v), (u + r, v), (u, v - r), (u, v + r)]
                        .iter()
                        .any(|&(a, b)| factor(layers, &rule.filter, a, b) <= 0.)
                    {
                        stats.radius_rejected += 1;
                        continue;
                    }
                }
                stats.passing_samples += 1;
                if rule.kind != Kind::Character {
                    continue;
                }
                out.character_filter_cells += 1;
                let i = z * n + x;
                if alpha[i] < 128 {
                    out.character_rejected_alpha += 1;
                    continue;
                }
                if height(layers, u, v) <= 0. {
                    out.character_rejected_height += 1;
                    continue;
                }
                let wx = (u - 0.5) * layers.size as f32;
                let wz = (v - 0.5) * layers.size as f32;
                if prepared.iter().any(|&(center, extent, s, c)| {
                    let dx = wx - center[0];
                    let dz = wz - center[1];
                    let lx = (c * dx + s * dz).abs() - extent[0];
                    let lz = (-s * dx + c * dz).abs() - extent[1];
                    lx.max(0.).hypot(lz.max(0.)) < definition.clearance
                }) {
                    out.character_rejected_clearance += 1;
                    continue;
                }
                out.character_mask[i] = 255;
                out.character_candidate_cells += 1;
            }
        }
        stats.sampled_area_m2 =
            stats.passing_samples as f64 * (layers.size as f64 / res as f64).powi(2);
        out.rules.push(stats);
    }
    Ok((!cancelled()).then_some(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn layers() -> Layers {
        let (hr, n) = dimensions(1000).unwrap();
        let cells = n * n;
        let mut l = Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: n,
            height: vec![18000; hr * hr],
            biome: vec![0; cells * 5],
            splat: vec![0; cells * 8],
            topology: vec![1; cells],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        l.biome[cells..2 * cells].fill(255);
        l.splat[..cells].fill(255);
        l
    }
    fn filter() -> Filter {
        Filter {
            splat_type: -1,
            biome_type: -1,
            topology_any: -1,
            topology_all: 0,
            topology_not: 0,
        }
    }
    fn rule() -> Rule {
        Rule {
            name: "Player".into(),
            kind: Kind::Character,
            filter: filter(),
            cutoff: 0.,
            radius: 0.,
        }
    }
    fn run(l: &Layers, alpha: &[u8], rules: &[Rule], obstacles: &[Obstacle]) -> Audit {
        audit(
            l,
            alpha,
            obstacles,
            Definition {
                source_sha256: "test",
                flags: &[("Field".into(), 1)],
                rules,
                clearance: 50.,
            },
            || false,
        )
        .unwrap()
        .unwrap()
    }
    #[test]
    fn masks_require_any_all_and_no_forbidden_bits() {
        let mut f = filter();
        assert!(f.topology_matches(0));
        f.topology_any = 3;
        f.topology_all = 12;
        f.topology_not = 16;
        assert!(f.topology_matches(13));
        assert!(f.topology_matches(14));
        assert!(!f.topology_matches(12));
        assert!(!f.topology_matches(5));
        assert!(!f.topology_matches(29));
        f.topology_any = 0;
        assert!(f.validate().is_err());
    }
    #[test]
    fn biome_uses_dominant_channel_and_later_channel_wins_ties() {
        let mut l = layers();
        let n = l.surface_resolution;
        let cells = n * n;
        l.biome[..cells].fill(127);
        l.biome[cells..2 * cells].fill(127);
        let mut f = filter();
        f.biome_type = 1;
        assert_eq!(factor(&l, &f, 0.5, 0.5), 0.);
        f.biome_type = 2;
        assert_eq!(factor(&l, &f, 0.5, 0.5), 1.);
        l.biome[..cells].fill(128);
        assert_eq!(factor(&l, &f, 0.5, 0.5), 0.);
    }
    #[test]
    fn checked_height_reuses_exact_taps_and_rejects_invalid_samples() {
        let mut terrain = layers();
        let last = terrain.height.len() - 1;
        terrain.height[last] = 20131;
        for [u, v] in [[0., 0.], [0.4997, 0.8164], [0.999, 0.999], [1., 1.]] {
            assert_eq!(
                sample_height(&terrain, u, v).unwrap().to_bits(),
                height_view(terrain.view(), u, v).to_bits()
            );
        }
        terrain.height[last] = 32767;
        assert_eq!(
            sample_height(&terrain, 1., 1.),
            Err("Invalid sampled height value".into())
        );
    }

    #[test]
    fn splats_interpolate_and_cutoff_is_strict() {
        let mut l = layers();
        let n = l.surface_resolution;
        l.splat[0] = 0;
        let mut f = filter();
        f.splat_type = 1;
        assert!((factor(&l, &f, 0.5 / (n - 1) as f32, 0.) - 0.5).abs() < 1e-5);
        let mut r = rule();
        r.cutoff = 1.;
        let a = run(&l, &vec![255; n * n], &[r], &[]);
        assert_eq!(a.character_filter_cells, 0);
    }

    #[test]
    fn shared_splat_sample_preserves_channel_interpolation_and_mask_order() {
        fn original_channel(data: &[u8], n: usize, channel: usize, u: f32, v: f32) -> f32 {
            let x = u.clamp(0., 1.) * (n - 1) as f32;
            let z = v.clamp(0., 1.) * (n - 1) as f32;
            let a = x.floor() as usize;
            let b = z.floor() as usize;
            let c = (a + 1).min(n - 1);
            let d = (b + 1).min(n - 1);
            let at = |x, z| data[channel * n * n + z * n + x] as f32 / 255.;
            let lo = at(a, b) * (1. - x.fract()) + at(c, b) * x.fract();
            let hi = at(a, d) * (1. - x.fract()) + at(c, d) * x.fract();
            lo * (1. - z.fract()) + hi * z.fract()
        }

        let mut l = layers();
        let n = l.surface_resolution;
        for (i, weight) in l.splat.iter_mut().enumerate() {
            *weight = (i.wrapping_mul(977) ^ (i >> 3)) as u8 / 3;
        }
        let mut f = filter();
        for (u, v) in [
            (0., 0.),
            (-0., 0.),
            (1., 1.),
            (0.25, 1.),
            (0.335, 0.117),
            (0.9991, 0.0001),
        ] {
            for mask in (0..=255).chain([256, i32::MIN | 1, i32::MIN]) {
                f.splat_type = mask;
                let expected = (0..8)
                    .filter(|c| mask & (1 << c) != 0)
                    .map(|c| original_channel(&l.splat, n, c, u, v))
                    .sum::<f32>()
                    .clamp(0., 1.);
                assert_eq!(factor_view(l.view(), &f, u, v).to_bits(), expected.to_bits());
            }
        }
    }

    #[test]
    fn audit_preserves_layers_and_reports_unknown_flags_holes_and_clearance() {
        let mut l = layers();
        let n = l.surface_resolution;
        let cells = n * n;
        l.topology[0] |= i32::MIN;
        let before = l.topology.clone();
        let heights = l.height.clone();
        let mut alpha = vec![255; cells];
        alpha[0] = 0;
        let obstacles = [Obstacle {
            center: [0., 0.],
            extent: [10., 20.],
            yaw: 37.,
        }];
        let a = run(&l, &alpha, &[rule()], &obstacles);
        assert_eq!(a.unknown_topology_cells, 1);
        assert_eq!(a.character_filter_cells, cells);
        assert_eq!(a.character_rejected_alpha, 1);
        assert!(a.character_rejected_clearance > 0);
        assert_eq!(
            a.character_candidate_cells
                + a.character_rejected_alpha
                + a.character_rejected_clearance,
            cells
        );
        assert_eq!(
            a.character_mask.iter().filter(|v| **v != 0).count(),
            a.character_candidate_cells
        );
        assert_eq!(l.topology, before);
        assert_eq!(l.height, heights);
        assert!(!a.physics_validated);
        assert!(a.json().unwrap().get("character_mask").is_none());
    }
    #[test]
    fn wet_terrain_cannot_be_a_player_candidate() {
        let mut l = layers();
        l.height.fill(16000);
        let n = l.surface_resolution;
        let a = run(&l, &vec![255; n * n], &[rule()], &[]);
        assert_eq!(a.character_rejected_height, n * n);
        assert_eq!(a.character_candidate_cells, 0);
    }
    #[test]
    fn radius_checks_neighbor_filters_and_population_sampling_is_bounded() {
        let mut l = layers();
        let n = l.surface_resolution;
        for z in 0..n {
            for x in n / 2..n {
                l.topology[z * n + x] = 0;
            }
        }
        let mut r = rule();
        r.kind = Kind::Population;
        r.name = "Trees".into();
        r.radius = 20.;
        r.filter.topology_any = 1;
        let a = run(&l, &vec![255; n * n], &[rule(), r], &[]);
        assert_eq!(a.rules[1].resolution, 256);
        assert_eq!(a.rules[1].distribution_resolution, 256);
        assert!(a.rules[1].radius_rejected > 0);
        assert!(a.rules[1].passing_samples < 256 * 128);
    }
    #[test]
    fn invalid_dimensions_and_cancellation_do_not_publish_a_partial_audit() {
        let mut l = layers();
        let n = l.surface_resolution;
        let rules = [rule()];
        let flags = [("Field".into(), 1)];
        let d = || Definition {
            source_sha256: "test",
            flags: &flags,
            rules: &rules,
            clearance: 50.,
        };
        assert!(
            audit(&l, &vec![255; n * n], &[], d(), || true)
                .unwrap()
                .is_none()
        );
        l.splat.pop();
        assert!(audit(&l, &vec![255; n * n], &[], d(), || false).is_err());
    }
    #[test]
    fn decoration_exclusion_is_atomic_separate_from_filters_and_never_claims_physics() {
        let l = layers();
        let n = l.surface_resolution;
        let mut a = run(&l, &vec![255; n * n], &[rule()], &[]);
        let before = a.json().unwrap();
        let mask = a.character_mask.clone();
        let mut blocked = vec![0; n * n];
        blocked[0] = 1;
        blocked[1] = 2;
        assert!(a.exclude_decoration(&blocked, || false).is_err());
        assert_eq!(a.json().unwrap(), before);
        assert_eq!(a.character_mask, mask);
        blocked[1] = 1;
        assert!(!a.exclude_decoration(&blocked, || true).unwrap());
        assert_eq!(a.json().unwrap(), before);
        assert_eq!(a.character_mask, mask);
        assert!(a.exclude_decoration(&blocked, || false).unwrap());
        assert_eq!(a.character_rejected_decoration, Some(2));
        assert_eq!(a.character_candidate_cells, n * n - 2);
        assert_eq!(a.character_filter_cells, n * n);
        assert_eq!(&a.character_mask[..2], &[0, 0]);
        assert!(!a.physics_validated);
        assert!(a.exclude_decoration(&blocked, || false).is_err());
    }
}
