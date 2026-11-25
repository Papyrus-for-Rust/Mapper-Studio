//! Independent monument composition on private revisions of a native base.
//! This is a versioned editor algorithm, not official generation parity.
use crate::native_layers::{Layers, dimensions};
use std::borrow::Cow;
#[path = "monument_projection.rs"]
pub mod projection;

pub const VERSION: &str = "papyrus-monument-composition-1";
pub const WATER_VERSION: &str = "papyrus-monument-composition-water-2";
pub const SOURCE_TEXTURE_VERSION: &str = "papyrus-monument-texture-composition-water-3";
pub const SOURCE_CLIFF_VERSION: &str = "papyrus-monument-cliff-composition-water-4";
pub const SOURCE_RASTER_VERSION: &str = "papyrus-monument-raster-composition-water-5";
pub const SOURCE_HEIGHT_VERSION: &str = "papyrus-monument-height-composition-water-6";
pub const MAX_EARTHWORK_SAMPLES: u64 = 128_000_000;
pub fn supported(version: &str) -> bool {
    [
        VERSION,
        WATER_VERSION,
        SOURCE_TEXTURE_VERSION,
        SOURCE_CLIFF_VERSION,
        SOURCE_RASTER_VERSION,
        SOURCE_HEIGHT_VERSION,
    ]
    .contains(&version)
}
pub fn water_aware(version: Option<&str>) -> bool {
    matches!(
        version,
        Some(
            WATER_VERSION
                | SOURCE_TEXTURE_VERSION
                | SOURCE_CLIFF_VERSION
                | SOURCE_RASTER_VERSION
                | SOURCE_HEIGHT_VERSION
        )
    )
}
pub fn source_textures(version: Option<&str>) -> bool {
    matches!(
        version,
        Some(
            SOURCE_TEXTURE_VERSION
                | SOURCE_CLIFF_VERSION
                | SOURCE_RASTER_VERSION
                | SOURCE_HEIGHT_VERSION
        )
    )
}

/// RGBA8 images from the verified texture cache, with the first row at local +Z.
#[derive(Clone, Copy)]
pub struct Image<'a> {
    pub width: usize,
    pub height: usize,
    pub rgba: &'a [u8],
}
impl<'a> Image<'a> {
    fn source(self) -> crate::source_texture::Texture<'a> {
        crate::source_texture::Texture {
            width: self.width,
            height: self.height,
            rgba: self.rgba,
        }
    }
    fn channel_for(self, u: f64, v: f64, channel: usize, source: bool) -> f64 {
        if source {
            f64::from(
                self.source()
                    .channel(source_uv(u, v), channel)
                    .expect("validated source stamp texture"),
            )
        } else {
            self.channel(u, v, channel)
        }
    }
    fn rgba_for(self, u: f64, v: f64, mask: u8, source: bool) -> [f64; 4] {
        if source {
            self.source()
                .rgba_on_validated_stamp(source_uv(u, v), mask)
                .expect("validated source stamp texture")
                .map(f64::from)
        } else {
            std::array::from_fn(|c| {
                if mask & (1 << c) == 0 { 0. } else { self.channel(u, v, c) }
            })
        }
    }

    fn topology_for(self, u: f64, v: f64, source: bool) -> i32 {
        if source {
            self.source()
                .topology(source_uv(u, v))
                .expect("validated source stamp texture")
        } else {
            self.topology(u, v)
        }
    }
    fn valid(self) -> bool {
        (1..=4096).contains(&self.width)
            && (1..=4096).contains(&self.height)
            && self.rgba.len() == self.width * self.height * 4
    }
    fn pixel(self, x: usize, y: usize) -> &'a [u8] {
        &self.rgba[(y * self.width + x) * 4..][..4]
    }
    fn sample(self, u: f64, v: f64, value: impl Fn(&[u8]) -> f64) -> f64 {
        let x = u.clamp(0., 1.) * (self.width - 1) as f64;
        let y = v.clamp(0., 1.) * (self.height - 1) as f64;
        let a = x.floor() as usize;
        let b = y.floor() as usize;
        let c = (a + 1).min(self.width - 1);
        let d = (b + 1).min(self.height - 1);
        let lo = value(self.pixel(a, b)) * (1. - x.fract()) + value(self.pixel(c, b)) * x.fract();
        let hi = value(self.pixel(a, d)) * (1. - x.fract()) + value(self.pixel(c, d)) * x.fract();
        lo * (1. - y.fract()) + hi * y.fract()
    }
    fn channel(self, u: f64, v: f64, channel: usize) -> f64 {
        self.sample(u, v, |p| f64::from(p[channel]) / 255.)
    }
    fn topology(self, u: f64, v: f64) -> i32 {
        let x = (u.clamp(0., 1.) * (self.width - 1) as f64).round_ties_even() as usize;
        let y = (v.clamp(0., 1.) * (self.height - 1) as f64).round_ties_even() as usize;
        i32::from_le_bytes(self.pixel(x, y).try_into().unwrap())
    }
}

pub struct Stamp<'a> {
    /// Explicit recipe revision, never inferred from texture contents.
    pub source_texture_sampling: bool,
    pub source_cliff_generation: bool,
    pub source_quad_raster: bool,
    /// Preserve the inspected scalar height writer's f32 evaluation order.
    pub source_height_blending: bool,
    pub position: [f64; 3],
    /// Studio convention: +X rotates toward +Z; negate Unity yaw at the boundary.
    pub yaw_degrees: f64,
    pub size: [f64; 3],
    pub offset: [f64; 3],
    pub radius: f64,
    pub fade: f64,
    pub height: Option<Image<'a>>,
    pub blend: Option<Image<'a>>,
    pub splat: Option<[Image<'a>; 2]>,
    pub splat_mask: u8,
    pub alpha: Option<Image<'a>>,
    pub biome: Option<Image<'a>>,
    pub biome_mask: u8,
    pub topology: Option<Image<'a>>,
    pub topology_mask: i32,
    pub replace_topology: bool,
    pub auto_cliff_splat: bool,
    pub auto_cliff_topology: bool,
}
fn source_uv(u: f64, v: f64) -> [f32; 2] {
    // Outside-border queries clamp to the same interior texels in the source.
    [(u as f32).clamp(-1., 2.), ((1. - v) as f32).clamp(-1., 2.)]
}
pub fn source_byte(value: f32) -> u8 {
    let value = f32::from_bits(value.to_bits() & !255);
    (value * 255. + 0.5) as u8
}
pub fn source_mixed_weights<const N: usize>(
    old: [u8; N],
    source: [f32; N],
    opacity: f32,
) -> [u8; N] {
    let sum = source.into_iter().fold(0., |a, b| a + b).clamp(0., 1.);
    if opacity == 0. || sum == 0. {
        return old;
    }
    let keep = 1. - opacity * sum;
    std::array::from_fn(|c| {
        source_byte(if keep == 0. && opacity == 1. {
            source[c]
        } else {
            old[c] as f32 * (1. / 255.) * keep + source[c] * opacity
        })
    })
}
fn mixed_weights_for<const N: usize>(
    old: [u8; N],
    values: [f64; N],
    opacity: f64,
    source: bool,
) -> [u8; N] {
    if source {
        source_mixed_weights(old, values.map(|v| v as f32), opacity as f32)
    } else {
        mixed_weights(old, values, opacity)
    }
}
fn alpha_for(old: u8, value: f64, opacity: f64, source: bool) -> u8 {
    if source {
        source_alpha(old, value as f32, opacity as f32)
    } else {
        (f64::from(old) * (1. - opacity) + value * 255. * opacity)
            .round()
            .clamp(0., 255.) as u8
    }
}
pub fn source_alpha(old: u8, value: f32, opacity: f32) -> u8 {
    let from = old as f32 * (1. / 255.);
    source_byte(from + (value - from) * opacity.clamp(0., 1.))
}
/// Inspected SetSplat target assignment. Unlike opacity painting, this can
/// reduce existing rock. The fully-rock early return is intentional source behavior.
pub fn source_set_rock(old: [u8; 8], target: f32) -> [u8; 8] {
    let previous = old[3] as f32 * (1. / 255.);
    if previous >= 1. {
        return old;
    }
    let ratio = (1. - target) / (1. - previous);
    let encode = |value: f32| {
        let value = f32::from_bits(value.to_bits() & !255);
        // Source unchecked float->byte conversion wraps positive scaled values.
        // SetSplat can exceed one for an existing non-normalized source cell.
        ((value * 255. + 0.5) as u32) as u8
    };
    std::array::from_fn(|c| {
        encode(if c == 3 {
            target
        } else {
            ratio * (old[c] as f32 * (1. / 255.))
        })
    })
}
fn source_slope(
    size: u32,
    hr: usize,
    sr: usize,
    x: usize,
    z: usize,
    read: impl FnMut(usize, usize) -> Option<i16>,
) -> f32 {
    let normal = if sr.is_power_of_two() && hr - 1 == 2 * sr {
        let norm_y = size as f32 / 1000. / hr as f32;
        crate::terrain_normal::normal_at_vertex(hr, norm_y, 2 * x + 1, 2 * z + 1, read)
    } else {
        let uv = [(x as f32 + 0.5) / sr as f32, (z as f32 + 0.5) / sr as f32];
        crate::terrain_normal::sample_with(size, hr, uv, read)
    }
    .expect("validated source cliff height grid");
    let length = normal.iter().fold(0., |sum, v| sum + v * v).sqrt();
    (normal[1] / length).clamp(-1., 1.).acos() * (180. / std::f32::consts::PI)
}
pub(crate) fn source_rock_sample(
    n: usize,
    x: usize,
    z: usize,
    mut read: impl FnMut(usize, usize) -> u8,
) -> f32 {
    let [px, pz] = [x, z].map(|v| ((v as f32 + 0.5) / n as f32) * (n - 1) as f32);
    let a = px as usize;
    let b = pz as usize;
    let c = (a + 1).min(n - 1);
    let d = (b + 1).min(n - 1);
    let mut at = |x, z| read(x, z) as f32 * (1. / 255.);
    let [aa, bb, cc, dd] = [at(a, b), at(c, b), at(a, d), at(c, d)];
    let lo = aa + (bb - aa) * (px - a as f32);
    let hi = cc + (dd - cc) * (px - a as f32);
    lo + (hi - lo) * (pz - b as f32)
}
fn splat_channels(images: [Image<'_>; 2], u: f64, v: f64, mask: u8, source: bool) -> [f64; 8] {
    let rgba = [
        images[0].rgba_for(u, v, mask & 15, source),
        images[1].rgba_for(u, v, mask >> 4, source),
    ];
    std::array::from_fn(|c| rgba[c / 4][c % 4])
}

fn source_splat_cell(
    s: &Stamp<'_>,
    mut splat: [u8; 8],
    topology: i32,
    [u, v]: [f64; 2],
    opacity: f64,
    slope: impl FnOnce() -> f32,
) -> [u8; 8] {
    if let Some(images) = s.splat.filter(|_| s.splat_mask != 0) {
        if s.auto_cliff_splat && topology & 8_389_632 == 0 {
            let slope = slope();
            if slope > 30. {
                splat = source_set_rock(splat, ((slope - 30.) / 20.).clamp(0., 1.));
            }
        }
        if opacity > 0. {
            let values = splat_channels(images, u, v, s.splat_mask, true);
            splat = mixed_weights_for(splat, values, opacity, true);
        }
    }
    splat
}

fn source_material_cell(
    s: &Stamp<'_>,
    mut out: projection::Cell,
    local: [f64; 2],
    slope: impl FnOnce() -> f32,
) -> projection::Cell {
    if !s.source_quad_raster && local.iter().any(|v| v.abs() > s.radius + 1e-9) {
        return out;
    }
    let u = local[0] / s.size[0] + 0.5;
    let v = 0.5 - local[1] / s.size[2];
    let opacity = radial(s, local);
    out.splat = source_splat_cell(s, out.splat, out.topology, [u, v], opacity, slope);
    if opacity > 0. {
        if let Some(alpha) = s.alpha {
            out.alpha = alpha_for(out.alpha, alpha.channel_for(u, v, 3, true), opacity, true);
        }
        if let Some(biome) = s.biome.filter(|_| s.biome_mask != 0) {
            let rgba = biome.rgba_for(u, v, 15, true);
            let fifth = fifth_for(rgba, true);
            let values = std::array::from_fn(|c| {
                if s.biome_mask & (1 << c) == 0 {
                    0.
                } else if c < 4 {
                    rgba[c]
                } else {
                    fifth
                }
            });
            out.biome = mixed_weights_for(out.biome, values, opacity, true);
        }
    }
    out
}
fn source_topology_cell(
    s: &Stamp<'_>,
    mut topology: i32,
    local: [f64; 2],
    slope: impl FnOnce() -> f32,
    rock: impl FnOnce() -> f32,
) -> i32 {
    let Some(texture) = s.topology.filter(|_| s.topology_mask != 0) else {
        return topology;
    };
    if !s.source_quad_raster && local.iter().any(|v| v.abs() > s.radius + 1e-9) {
        return topology;
    }
    let bits = texture.topology_for(local[0] / s.size[0] + 0.5, 0.5 - local[1] / s.size[2], true)
        & s.topology_mask;
    if bits != 0 && s.replace_topology {
        topology &= !s.topology_mask;
    }
    if s.auto_cliff_topology && topology & 8_389_632 == 0 {
        if slope() > 30. || rock() > 0.4 {
            topology |= 2;
        } else {
            topology &= !2;
        }
    }
    topology |= bits;
    topology
}
fn fifth_for(rgba: [f64; 4], source: bool) -> f64 {
    if source {
        f64::from((1. - rgba.into_iter().fold(0_f32, |a, b| a + b as f32)).max(0.))
    } else {
        (1. - rgba.iter().sum::<f64>()).max(0.)
    }
}

pub struct Composed {
    /// Prefabs admitted together with their baked standalone terrain stamps.
    pub custom_baked: Vec<crate::prefab_records::Record>,
    pub rivers: Option<std::sync::Arc<crate::river_pipeline::World>>,
    pub rails: Option<crate::rail_scene::Scene>,
    /// Partial late road contribution; never replaces the immutable terrain grid.
    pub gameplay_height: Option<crate::gameplay_height::Output>,
    pub objects: Option<crate::object_scene::Scene>,
    pub dungeons: Option<crate::dungeon_base::Scene>,
    pub tunnels: Option<crate::tunnel_scene::Scene>,
    pub offshore: Option<crate::offshore_search::Scene>,
    pub clutter_topology: Option<serde_json::Value>,
    pub cliff_topology: Option<serde_json::Value>,
    pub topology_before_overrides: Option<Vec<i32>>,
    pub topology_overrides: Option<crate::topology_paint::Report>,
    pub spawn_audit: Option<crate::spawn::Audit>,
    pub decoration: Option<crate::decor::Report>,
    pub roads: Option<crate::road_network::Report>,
    pub layers: Layers,
    /// Like native surface arrays: rows advance toward world +Z.
    pub alpha: Vec<u8>,
    pub clamped_height_samples: usize,
    /// Present only for the explicitly selected water-aware revision.
    pub ocean: Option<crate::ocean::Ocean>,
}

/// Reclassify the ocean from final composed heights. Enclosed basins remain unfilled.
/// This does not invent local water bodies from unused monument texture references.
pub fn compose_with_water(
    base: &Layers,
    base_alpha: &[u8],
    stamps: &[Stamp<'_>],
    cancelled: impl Fn() -> bool,
) -> Result<Option<Composed>, String> {
    let Some(mut result) = compose(base, base_alpha, stamps, &cancelled)? else {
        return Ok(None);
    };
    let Some(ocean) = crate::ocean::reconstruct(
        &result.layers.height,
        result.layers.height_resolution,
        &result.layers.topology,
        result.layers.surface_resolution,
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    result.layers.topology.clone_from(&ocean.topology);
    result.ocean = Some(ocean);
    Ok((!cancelled()).then_some(result))
}

struct Transform {
    sin: f64,
    cos: f64,
    center: [f64; 2],
}
impl Transform {
    fn quad(
        &self,
        size: u32,
        n: usize,
        half: [f64; 2],
    ) -> Result<crate::source_stamp_raster::Quad, String> {
        let world = [
            [-half[0], -half[1]],
            [half[0], -half[1]],
            [-half[0], half[1]],
            [half[0], half[1]],
        ]
        .map(|[x, z]| {
            [
                (self.center[0] + self.cos * x - self.sin * z) as f32,
                (self.center[1] + self.sin * x + self.cos * z) as f32,
            ]
        });
        crate::source_stamp_raster::Quad::from_world(
            n,
            [-(size as f32) * 0.5; 2],
            [1. / size as f32; 2],
            world,
        )
    }
    fn new(s: &Stamp<'_>) -> Self {
        let (sin, cos) = s.yaw_degrees.rem_euclid(360.).to_radians().sin_cos();
        Self {
            sin,
            cos,
            center: [
                s.position[0] + cos * s.offset[0] - sin * s.offset[2],
                s.position[2] + sin * s.offset[0] + cos * s.offset[2],
            ],
        }
    }
    fn local(&self, x: f64, z: f64) -> [f64; 2] {
        let dx = x - self.center[0];
        let dz = z - self.center[1];
        [
            self.cos * dx + self.sin * dz,
            -self.sin * dx + self.cos * dz,
        ]
    }
    fn bounds(&self, size: f64, n: usize, half: [f64; 2]) -> [usize; 4] {
        let extent = [
            self.cos.abs() * half[0] + self.sin.abs() * half[1],
            self.sin.abs() * half[0] + self.cos.abs() * half[1],
        ];
        let mut result = [0; 4];
        for axis in 0..2 {
            let cell = |w: f64| (w / size + 0.5) * n as f64 - 0.5;
            result[axis * 2] = (cell(self.center[axis] - extent[axis]) - 1e-9)
                .ceil()
                .clamp(0., n as f64) as usize;
            result[axis * 2 + 1] = ((cell(self.center[axis] + extent[axis]) + 1e-9).floor() + 1.)
                .clamp(0., n as f64) as usize;
        }
        result
    }
}

fn radial(s: &Stamp<'_>, local: [f64; 2]) -> f64 {
    let distance = local[0].hypot(local[1]);
    if s.fade == 0. {
        if distance <= s.radius { 1. } else { 0. }
    } else {
        ((s.radius - distance) / s.fade).clamp(0., 1.)
    }
}

fn weights<const N: usize>(
    data: &mut [u8],
    cells: usize,
    cell: usize,
    source: [f64; N],
    opacity: f64,
    source_sampling: bool,
) {
    let old = std::array::from_fn(|c| data[c * cells + cell]);
    let values = mixed_weights_for(old, source, opacity, source_sampling);
    for c in 0..N {
        data[c * cells + cell] = values[c];
    }
}

fn mixed_weights<const N: usize>(old: [u8; N], source: [f64; N], opacity: f64) -> [u8; N] {
    let sum = source.iter().sum::<f64>();
    if sum <= 0. || opacity <= 0. {
        return old;
    }
    let keep = 1. - opacity * sum.min(1.);
    let mixed: [f64; N] =
        std::array::from_fn(|c| f64::from(old[c]) / 255. * keep + source[c] * opacity);
    let total = mixed.iter().sum::<f64>();
    let mut values: [u8; N] = std::array::from_fn(|c| (mixed[c] / total * 255.).floor() as u8);
    let mut remainder = 255 - values.iter().map(|&v| u32::from(v)).sum::<u32>();
    // Largest remainder with channel-index tie breaking keeps weights exactly 255.
    let mut order: [usize; N] = std::array::from_fn(|i| i);
    order.sort_by(|&a, &b| {
        (mixed[b] / total * 255. - f64::from(values[b]))
            .total_cmp(&(mixed[a] / total * 255. - f64::from(values[a])))
            .then(a.cmp(&b))
    });
    for &c in &order {
        if remainder == 0 {
            break;
        }
        values[c] += 1;
        remainder -= 1;
    }
    values
}

fn slope(layers: &Layers, x: usize, z: usize) -> f64 {
    let sr = layers.surface_resolution;
    let hr = layers.height_resolution;
    let hx = x * (hr - 1) / (sr - 1);
    let hz = z * (hr - 1) / (sr - 1);
    let left = hx.saturating_sub(1);
    let right = (hx + 1).min(hr - 1);
    let down = hz.saturating_sub(1);
    let up = (hz + 1).min(hr - 1);
    let at = |x, z| f64::from(layers.height[z * hr + x]) / 32766. * 1000.;
    let dx =
        (at(right, hz) - at(left, hz)) / ((right - left) as f64 * layers.size as f64 / hr as f64);
    let dz = (at(hx, up) - at(hx, down)) / ((up - down) as f64 * layers.size as f64 / hr as f64);
    dx.hypot(dz).atan().to_degrees()
}

fn validate_stamp(s: &Stamp<'_>) -> Result<(), String> {
    if s.source_height_blending && !s.source_quad_raster {
        return Err("Source height blending requires source raster rules".into());
    }
    if s.source_quad_raster && !s.source_cliff_generation {
        return Err("Source raster requires source cliff rules".into());
    }
    if s.source_cliff_generation && !s.source_texture_sampling {
        return Err("Source cliffs require source texture sampling".into());
    }
    let images = s
        .height
        .into_iter()
        .chain(s.blend)
        .chain(s.splat.into_iter().flatten())
        .chain(s.alpha)
        .chain(s.biome)
        .chain(s.topology);
    if images
        .into_iter()
        .any(|i| !i.valid() || (s.source_texture_sampling && (i.width < 3 || i.height < 3)))
        || !s.yaw_degrees.is_finite()
        || s.position
            .iter()
            .chain(&s.offset)
            .any(|v| !v.is_finite() || v.abs() > 65536.)
        || s.size
            .iter()
            .any(|v| !v.is_finite() || !(0.1..=8192.).contains(v))
        || !s.radius.is_finite()
        || !(0.1..=8192.).contains(&s.radius)
        || !s.fade.is_finite()
        || !(0. ..=s.radius).contains(&s.fade)
        || s.biome_mask > 31
    {
        return Err("Invalid monument composition texture or transform".into());
    }
    Ok(())
}

/// Exact same height kernel used by composition and candidate assessment.
fn height_value(
    s: &Stamp<'_>,
    height: Image<'_>,
    local: [f64; 2],
    x: usize,
    z: usize,
    hr: usize,
    old: i16,
) -> Option<(i16, bool)> {
    let u = local[0] / s.size[0] + 0.5;
    let v = 0.5 - local[1] / s.size[2];
    let mut opacity = if let Some(blend) = s.blend {
        if !s.source_quad_raster
            && (!(-1e-12..=1. + 1e-12).contains(&u) || !(-1e-12..=1. + 1e-12).contains(&v))
        {
            return None;
        }
        if s.source_texture_sampling {
            f64::from(blend.source().alpha_on_validated_stamp(source_uv(u, v))
                .expect("validated source stamp texture"))
        } else {
            blend.channel(u, v, 3)
        }
    } else {
        radial(s, local)
    };
    if opacity == 0. {
        return None;
    }
    let edge = x.min(z).min(hr - 1 - x).min(hr - 1 - z);
    // Past the ten-cell edge fade, its multiplier is exactly one.
    if edge < 10 {
        let border = edge as f64 / 10.;
        opacity = if s.source_height_blending {
            f64::from(opacity as f32 * source_smoothstep(0., 1., border as f32))
        } else {
            opacity * (border * border * (3. - 2. * border))
        };
    } else if s.source_height_blending {
        opacity = f64::from(opacity as f32);
    }
    if opacity == 0. {
        return None;
    }
    if s.source_texture_sampling {
        let relative = height
            .source()
            .height_on_validated_stamp(source_uv(u, v))
            .expect("validated source stamp texture");
        let world = (s.position[1] as f32 + s.offset[1] as f32) + relative * s.size[1] as f32;
        let target = ((world + 500.) * (1. / 1000.)).clamp(0., 1.);
        if s.source_height_blending {
            return Some((
                source_height_blend(old, target, opacity as f32),
                !(-500. ..=500.).contains(&world),
            ));
        }
        let t = (opacity as f32).clamp(0., 1.);
        let blend = t * t * (3. - 2. * t);
        let from = old as f32 * (1. / 32766.);
        let value = from + (target - from) * blend;
        return Some((
            ((value * 32766. + 0.5) as i32).clamp(0, 32766) as i16,
            !(-500. ..=500.).contains(&world),
        ));
    }
    let relative = height.sample(u, v, |p| {
        f64::from(i16::from_le_bytes([p[0], p[2]])) / 32766.
    });
    let world = s.position[1] + s.offset[1] + relative * s.size[1];
    let target = ((world + 500.) / 1000.).clamp(0., 1.) * 32766.;
    let blend = opacity * opacity * (3. - 2. * opacity);
    Some((
        (f64::from(old) * (1. - blend) + target * blend)
            .round()
            .clamp(0., 32766.) as i16,
        !(-500. ..=500.).contains(&world),
    ))
}

fn source_smoothstep(from: f32, to: f32, t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    let t = -2. * t * t * t + 3. * t * t;
    to * t + from * (1. - t)
}

/// Inspected height writer on validated normalized inputs. This deliberately
/// keeps Unity's scalar evaluation order through quantization; algebraically
/// equivalent lerps can produce a different encoded terrain height.
pub fn source_height_blend(old: i16, target: f32, opacity: f32) -> i16 {
    let value = source_smoothstep(old as f32 * 3.051944E-5, target, opacity);
    ((value * 32766. + 0.5) as i32).clamp(0, 32766) as i16
}

/// Full-resolution stamp assessment without allocating or modifying terrain.
#[derive(Debug, Default, PartialEq)]
pub struct HeightAssessment {
    pub samples: usize,
    pub maximum_change: f64,
    pub cut_volume: f64,
    pub fill_volume: f64,
    pub submerged_samples: usize,
    pub clamped_samples: usize,
}

/// Height checks needed for candidate acceptance and scoring.
#[derive(Debug, Default, PartialEq)]
pub struct PlacementAssessment {
    pub samples: usize,
    pub maximum_change: f64,
    pub submerged_samples: usize,
    pub clamped_samples: usize,
}

/// Validates the source once; candidate loops inspect only their affected cells.
pub struct HeightTerrain<'a> {
    heights: &'a [i16],
    resolution: usize,
    size: u32,
    axis: Vec<f64>,
}

fn height_axis(size: u32, resolution: usize) -> Vec<f64> {
    (0..resolution)
        .map(|cell| ((cell as f64 + 0.5) / resolution as f64 - 0.5) * size as f64)
        .collect()
}
/// Lazy height-only view using the exact committed stamp raster/rounding. Four
/// projected native cells are interpolated per query; no full-world copy is made.
pub struct ProjectedHeight<'a, 'b, 'c> {
    terrain: &'a HeightTerrain<'a>,
    stamp: &'b Stamp<'c>,
    transform: Transform,
    bounds: [usize; 4],
    quad: Option<crate::source_stamp_raster::Quad>,
}
impl ProjectedHeight<'_, '_, '_> {
    pub fn sample_source_with_support(
        &self,
        position: [f32; 3],
        origin_y: f32,
        vertical_span: f32,
    ) -> Result<(f32, [usize; 4]), String> {
        let n = self.terrain.resolution;
        let mut indices = [0; 4];
        let mut count = 0;
        let height = crate::path_terrain::sample_height_with(
            crate::path_terrain::Layout {
                size: self.terrain.size as f32,
                resolution: n,
                origin_y,
                vertical_span,
            },
            position,
            |i| {
                *indices.get_mut(count)? = i;
                count += 1;
                Some(self.cell(i % n, i / n))
            },
        )?;
        if count != 4 {
            return Err("Unexpected source-height support count".into());
        }
        Ok((height, indices))
    }
    pub fn sample_source(
        &self,
        position: [f32; 3],
        origin_y: f32,
        vertical_span: f32,
    ) -> Result<f32, String> {
        let n = self.terrain.resolution;
        crate::path_terrain::sample_height_with(
            crate::path_terrain::Layout {
                size: self.terrain.size as f32,
                resolution: n,
                origin_y,
                vertical_span,
            },
            position,
            |i| Some(self.cell(i % n, i / n)),
        )
    }
    fn cell(&self, x: usize, z: usize) -> i16 {
        let n = self.terrain.resolution;
        let old = self.terrain.heights[z * n + x];
        // Boat probes cover the whole map while the stamp affects a small
        // rectangle. Skip its polygon tests for samples outside that rectangle.
        let Some(h) = self.stamp.height.filter(|_| {
            x >= self.bounds[0] && x < self.bounds[1] && z >= self.bounds[2] && z < self.bounds[3]
        }) else {
            return old;
        };
        if self.quad.is_some_and(|q| !q.contains(x, z)) {
            return old;
        }
        let local = self.transform.local(self.terrain.axis[x], self.terrain.axis[z]);
        height_value(self.stamp, h, local, x, z, n, old).map_or(old, |v| v.0)
    }
    pub fn sample(&self, u: f32, v: f32) -> Option<f32> {
        if !u.is_finite() || !v.is_finite() || !(0. ..=1.).contains(&u) || !(0. ..=1.).contains(&v)
        {
            return None;
        }
        let n = self.terrain.resolution;
        let x = u * (n - 1) as f32;
        let z = v * (n - 1) as f32;
        let (a, b) = (x.floor() as usize, z.floor() as usize);
        let at = |x, z| crate::erosion::decode(self.cell(x, z));
        let c = (a + 1).min(n - 1);
        let d = (b + 1).min(n - 1);
        let lo = at(a, b) * (1. - x.fract()) + at(c, b) * x.fract();
        let hi = at(a, d) * (1. - x.fract()) + at(c, d) * x.fract();
        Some(lo * (1. - z.fract()) + hi * z.fract())
    }
}
fn stamp_row(
    quad: Option<&crate::source_stamp_raster::Quad>,
    bounds: [usize; 4],
    z: usize,
) -> crate::source_stamp_raster::QuadRow {
    quad.map_or_else(
        || crate::source_stamp_raster::QuadRow::full(bounds[0], bounds[1]),
        |quad| quad.row(z),
    )
}

impl<'a> HeightTerrain<'a> {
    pub fn projected<'b, 'c>(
        &'a self,
        stamp: &'b Stamp<'c>,
    ) -> Result<ProjectedHeight<'a, 'b, 'c>, String> {
        validate_stamp(stamp)?;
        let transform = Transform::new(stamp);
        let half = if stamp.blend.is_some() {
            [stamp.size[0] / 2., stamp.size[2] / 2.]
        } else {
            [stamp.radius; 2]
        };
        let quad = stamp
            .source_quad_raster
            .then(|| transform.quad(self.size, self.resolution, half))
            .transpose()?;
        let bounds = quad.as_ref().map_or_else(
            || transform.bounds(self.size as f64, self.resolution, half),
            crate::source_stamp_raster::Quad::bounds,
        );
        Ok(ProjectedHeight {
            terrain: self,
            stamp,
            transform,
            bounds,
            quad,
        })
    }
    pub fn new(heights: &'a [i16], resolution: usize, size: u32) -> Result<Self, String> {
        let (expected, _) = dimensions(size)?;
        if resolution != expected
            || heights.len() != resolution * resolution
            || heights.iter().any(|h| !(0..=32766).contains(h))
        {
            return Err("Invalid full-resolution assessment terrain".into());
        }
        Ok(Self::from_validated(heights, resolution, size))
    }

    fn from_validated(heights: &'a [i16], resolution: usize, size: u32) -> Self {
        Self {
            heights,
            resolution,
            size,
            axis: height_axis(size, resolution),
        }
    }
    pub fn assess(
        &self,
        s: &Stamp<'_>,
        work: &mut u64,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<HeightAssessment>, String> {
        self.assess_inner::<true>(s, work, None, MAX_EARTHWORK_SAMPLES, false, cancelled)
    }
    /// Early rejection returns a sufficient failure report; accepted reports cover every cell.
    pub fn assess_for_placement(
        &self,
        s: &Stamp<'_>,
        work: &mut u64,
        maximum_change: f64,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<HeightAssessment>, String> {
        if !maximum_change.is_finite() || maximum_change <= 0. {
            return Err("Invalid cut/fill limit".into());
        }
        self.assess_inner::<true>(
            s,
            work,
            Some(maximum_change),
            MAX_EARTHWORK_SAMPLES,
            false,
            cancelled,
        )
    }
    /// Per-target budget for the balanced search. Acceptance still checks every cell.
    pub fn assess_for_placement_with_budget(
        &self,
        s: &Stamp<'_>,
        work: &mut u64,
        maximum_change: f64,
        budget: u64,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<HeightAssessment>, String> {
        if !maximum_change.is_finite()
            || maximum_change <= 0.
            || budget == 0
            || budget > MAX_EARTHWORK_SAMPLES
        {
            return Err("Invalid bounded earthwork assessment limits".into());
        }
        self.assess_inner::<true>(s, work, Some(maximum_change), budget, false, cancelled)
    }
    /// Coastal placement permits seabed samples, so they cannot terminate a scan.
    /// Cut/fill and clamping still reject early; acceptance covers the entire stamp.
    pub fn assess_for_coastal_placement_with_budget(
        &self,
        s: &Stamp<'_>,
        work: &mut u64,
        maximum_change: f64,
        budget: u64,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<HeightAssessment>, String> {
        if !maximum_change.is_finite()
            || maximum_change <= 0.
            || budget == 0
            || budget > MAX_EARTHWORK_SAMPLES
        {
            return Err("Invalid bounded earthwork assessment limits".into());
        }
        self.assess_inner::<true>(s, work, Some(maximum_change), budget, true, cancelled)
    }
    /// Apply the same bounded placement checks without summing proposal volumes.
    pub fn check_placement(
        &self,
        stamp: &Stamp<'_>,
        work: &mut u64,
        maximum_change: f64,
        budget: u64,
        allow_submerged: bool,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<PlacementAssessment>, String> {
        if !maximum_change.is_finite() || maximum_change <= 0.
            || budget == 0 || budget > MAX_EARTHWORK_SAMPLES
        {
            return Err("Invalid bounded earthwork assessment limits".into());
        }
        Ok(self.assess_inner::<false>(
            stamp, work, Some(maximum_change), budget, allow_submerged, cancelled,
        )?.map(|assessment| PlacementAssessment {
            samples: assessment.samples,
            maximum_change: assessment.maximum_change,
            submerged_samples: assessment.submerged_samples,
            clamped_samples: assessment.clamped_samples,
        }))
    }

    fn assess_inner<const MEASURE_VOLUME: bool>(
        &self,
        s: &Stamp<'_>,
        work: &mut u64,
        limit: Option<f64>,
        budget: u64,
        allow_submerged: bool,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<HeightAssessment>, String> {
        validate_stamp(s)?;
        let t = Transform::new(s);
        let half = if s.blend.is_some() {
            [s.size[0] / 2., s.size[2] / 2.]
        } else {
            [s.radius; 2]
        };
        // All height and surface modifier bounds must fit, including rotated offset.
        let half = [half[0].max(s.radius), half[1].max(s.radius)];
        let ex = t.cos.abs() * half[0] + t.sin.abs() * half[1];
        let ez = t.sin.abs() * half[0] + t.cos.abs() * half[1];
        if t.center[0].abs() + ex > self.size as f64 / 2.
            || t.center[1].abs() + ez > self.size as f64 / 2.
        {
            return Err("Authored modifier reaches outside world bounds".into());
        }
        let hr = self.resolution;
        let quad = s
            .source_quad_raster
            .then(|| {
                t.quad(
                    self.size,
                    hr,
                    if s.blend.is_some() {
                        [s.size[0] / 2., s.size[2] / 2.]
                    } else {
                        [s.radius; 2]
                    },
                )
            })
            .transpose()?;
        let hb = quad.as_ref().map_or_else(
            || t.bounds(self.size as f64, hr, half),
            crate::source_stamp_raster::Quad::bounds,
        );
        let mut report = HeightAssessment {
            samples: 0,
            maximum_change: 0.,
            cut_volume: 0.,
            fill_volume: 0.,
            submerged_samples: 0,
            clamped_samples: 0,
        };
        // Surface-only modifiers do no height work. Their complete transform and
        // world bounds still pass the same validation above.
        let Some(height) = s.height else {
            return Ok((!cancelled()).then_some(report));
        };
        let area = (self.size as f64 / hr as f64).powi(2);
        let mut maximum_encoded_change = 0;
        let row_width = (hb[1] - hb[0]) as u64;
        for (row, &world_z) in self.axis[hb[2]..hb[3]].iter().enumerate() {
            if cancelled() {
                return Ok(None);
            }
            let z = hb[2] + row;
            // Budgeting counts the whole bounding rectangle, including the
            // uncovered cells before a rejection or exhausted sample allowance.
            let admitted = row_width.min(budget.saturating_sub(*work));
            let end = hb[0] + admitted as usize;
            for x in stamp_row(quad.as_ref(), hb, z)
                .indices()
                .take_while(|&x| x < end)
            {
                let world_x = self.axis[x];
                let old = self.heights[z * hr + x];
                let local = t.local(world_x, world_z);
                if let Some((value, clamped)) = height_value(s, height, local, x, z, hr, old) {
                    let encoded_delta = value - old;
                    report.samples += 1;
                    if encoded_delta.abs() > maximum_encoded_change {
                        maximum_encoded_change = encoded_delta.abs();
                        report.maximum_change = f64::from(maximum_encoded_change) / 32766. * 1000.;
                    }
                    if MEASURE_VOLUME {
                        let delta = f64::from(encoded_delta) / 32766. * 1000.;
                        report.cut_volume += (-delta).max(0.) * area;
                        report.fill_volume += delta.max(0.) * area;
                    }
                    // The validated 0..32766 transport encodes sea level at its midpoint.
                    report.submerged_samples += usize::from(old.min(value) <= 16383);
                    report.clamped_samples += usize::from(clamped);
                    if limit.is_some_and(|l| {
                        report.maximum_change > l
                            || (!allow_submerged && report.submerged_samples > 0)
                            || report.clamped_samples > 0
                    }) {
                        *work += (x - hb[0] + 1) as u64;
                        return Ok(Some(report));
                    }
                }
            }
            *work += admitted;
            if admitted < row_width {
                *work = work.checked_add(1).ok_or("Earthwork sample budget overflow")?;
                if budget != MAX_EARTHWORK_SAMPLES {
                    return Err("Per-monument earthwork sample allowance reached".into());
                }
                return Err("Earthwork search exceeds 128 million samples; reduce targets or use exact preferences".into());
            }
        }
        Ok(Some(report))
    }
}

/// Compose all stamps in caller-supplied stable order. Cancellation discards the
/// entire private revision. Old stamped output must never be passed as the base.
pub fn compose(
    base: &Layers,
    base_alpha: &[u8],
    stamps: &[Stamp<'_>],
    cancelled: impl Fn() -> bool,
) -> Result<Option<Composed>, String> {
    compose_streamed(base, base_alpha, &cancelled, |apply| {
        for stamp in stamps {
            if !apply(stamp)? {
                return Ok(false);
            }
        }
        Ok(true)
    })
}

/// Feed one prepared stamp at a time; textures may be released after each call.
/// All mutations stay private. Feed errors, limits or cancellation discard the candidate.
pub fn compose_streamed(
    base: &Layers,
    base_alpha: &[u8],
    cancelled: impl Fn() -> bool,
    feed: impl FnOnce(&mut dyn FnMut(&Stamp<'_>) -> Result<bool, String>) -> Result<bool, String>,
) -> Result<Option<Composed>, String> {
    compose_candidate(
        Cow::Borrowed(base),
        Cow::Borrowed(base_alpha),
        cancelled,
        feed,
    )
}

/// Stamp an exclusively owned terrain revision in place. The input is
/// consumed, so a failed or cancelled feed cannot expose a partially stamped map.
pub fn compose_owned_streamed(
    base: Layers,
    base_alpha: Vec<u8>,
    cancelled: impl Fn() -> bool,
    feed: impl FnOnce(&mut dyn FnMut(&Stamp<'_>) -> Result<bool, String>) -> Result<bool, String>,
) -> Result<Option<Composed>, String> {
    compose_candidate(
        Cow::Owned(base),
        Cow::Owned(base_alpha),
        cancelled,
        feed,
    )
}

fn compose_candidate(
    base: Cow<'_, Layers>,
    base_alpha: Cow<'_, [u8]>,
    cancelled: impl Fn() -> bool,
    feed: impl FnOnce(&mut dyn FnMut(&Stamp<'_>) -> Result<bool, String>) -> Result<bool, String>,
) -> Result<Option<Composed>, String> {
    let size = base.size;
    let (hr, sr) = dimensions(size)?;
    let cells = sr * sr;
    if base.height_resolution != hr
        || base.surface_resolution != sr
        || base.height.len() != hr * hr
        || base.splat.len() != cells * 8
        || base.biome.len() != cells * 5
        || base.topology.len() != cells
        || base_alpha.len() != cells
        || base.height.iter().any(|h| !(0..=32766).contains(h))
    {
        return Err("Invalid composition base dimensions or heights".into());
    }
    let mut out = Composed {
        custom_baked: Vec::new(),
        rivers: None,
        rails: None,
        gameplay_height: None,
        objects: None,
        dungeons: None, tunnels: None,
        offshore: None,
        clutter_topology: None,
        cliff_topology: None,
        topology_before_overrides: None,
        topology_overrides: None,
        spawn_audit: None,
        decoration: None,
        roads: None,
        layers: base.into_owned(),
        alpha: base_alpha.into_owned(),
        clamped_height_samples: 0,
        ocean: None,
    };
    let mut work = 0_u64;
    let mut count = 0;
    let mut height_coordinates = None;
    let mut surface_coordinates = None;
    let mut apply = |s: &Stamp<'_>| -> Result<bool, String> {
        if cancelled() {
            return Ok(false);
        }
        count += 1;
        if count > 128 {
            return Err("Too many monument stamps".into());
        }
        validate_stamp(s)?;
        let t = Transform::new(s);
        let half = if s.blend.is_some() {
            [s.size[0] / 2., s.size[2] / 2.]
        } else {
            [s.radius; 2]
        };
        let height_quad = s
            .source_quad_raster
            .then(|| t.quad(size, hr, half))
            .transpose()?;
        let surface_quad = s
            .source_quad_raster
            .then(|| t.quad(size, sr, [s.radius; 2]))
            .transpose()?;
        let hb = height_quad.as_ref().map_or_else(
            || t.bounds(size as f64, hr, half),
            crate::source_stamp_raster::Quad::bounds,
        );
        let sb = surface_quad.as_ref().map_or_else(
            || t.bounds(size as f64, sr, [s.radius; 2]),
            crate::source_stamp_raster::Quad::bounds,
        );
        for bounds in [hb, sb] {
            work +=
                (bounds[1].saturating_sub(bounds[0]) * bounds[3].saturating_sub(bounds[2])) as u64;
        }
        if s.source_cliff_generation && s.topology.is_some() && s.topology_mask != 0 {
            work += (sb[1].saturating_sub(sb[0]) * sb[3].saturating_sub(sb[2])) as u64;
        }
        if work > 128_000_000 {
            return Err("Monument composition sample budget exceeded".into());
        }
        if let Some(height) = s.height {
            let axis = height_coordinates.get_or_insert_with(|| height_axis(size, hr));
            for z in hb[2]..hb[3] {
                if cancelled() {
                    return Ok(false);
                }
                for x in stamp_row(height_quad.as_ref(), hb, z).indices() {
                    let local = t.local(axis[x], axis[z]);
                    let old = out.layers.height[z * hr + x];
                    if let Some((value, clamped)) = height_value(s, height, local, x, z, hr, old) {
                        out.clamped_height_samples += usize::from(clamped);
                        out.layers.height[z * hr + x] = value;
                    }
                }
            }
        }
        let axis = surface_coordinates.get_or_insert_with(|| height_axis(size, sr));
        if s.source_cliff_generation {
            let at = |layers: &Layers, alpha: &[u8], x: usize, z: usize| projection::Cell {
                splat: std::array::from_fn(|c| layers.splat[c * cells + z * sr + x]),
                biome: std::array::from_fn(|c| layers.biome[c * cells + z * sr + x]),
                alpha: alpha[z * sr + x],
                topology: layers.topology[z * sr + x],
            };
            let local = |x: usize, z: usize| t.local(axis[x], axis[z]);
            // Complete material/alpha/biome writes before topology reads nearby
            // splat cells. A combined scan would depend on traversal direction.
            for z in sb[2]..sb[3] {
                if cancelled() {
                    return Ok(false);
                }
                for x in stamp_row(surface_quad.as_ref(), sb, z).indices() {
                    let value = source_material_cell(
                        s,
                        at(&out.layers, &out.alpha, x, z),
                        local(x, z),
                        || {
                            source_slope(size, hr, sr, x, z, |hx, hz| {
                                Some(out.layers.height[hz * hr + hx])
                            })
                        },
                    );
                    for c in 0..8 {
                        out.layers.splat[c * cells + z * sr + x] = value.splat[c];
                    }
                    for c in 0..5 {
                        out.layers.biome[c * cells + z * sr + x] = value.biome[c];
                    }
                    out.alpha[z * sr + x] = value.alpha;
                }
            }
            if s.topology.is_some() && s.topology_mask != 0 {
                for z in sb[2]..sb[3] {
                    if cancelled() {
                        return Ok(false);
                    }
                    for x in stamp_row(surface_quad.as_ref(), sb, z).indices() {
                        out.layers.topology[z * sr + x] = source_topology_cell(
                            s,
                            out.layers.topology[z * sr + x],
                            local(x, z),
                            || {
                                source_slope(size, hr, sr, x, z, |hx, hz| {
                                    Some(out.layers.height[hz * hr + hx])
                                })
                            },
                            || {
                                source_rock_sample(sr, x, z, |sx, sz| {
                                    out.layers.splat[3 * cells + sz * sr + sx]
                                })
                            },
                        );
                    }
                }
            }
            return Ok(true);
        }
        for z in sb[2]..sb[3] {
            if cancelled() {
                return Ok(false);
            }
            for x in sb[0]..sb[1] {
                let local = t.local(axis[x], axis[z]);
                let opacity = radial(s, local);
                if local.iter().any(|v| v.abs() > s.radius + 1e-9) {
                    continue;
                }
                let u = local[0] / s.size[0] + 0.5;
                let v = 0.5 - local[1] / s.size[2];
                let cell = z * sr + x;
                if s.auto_cliff_splat && out.layers.topology[cell] & 8_389_632 == 0 {
                    let cliff = ((slope(&out.layers, x, z) - 30.) / 20.).clamp(0., 1.);
                    let mut rock = [0.; 8];
                    rock[3] = 1.;
                    weights(
                        &mut out.layers.splat,
                        cells,
                        cell,
                        rock,
                        cliff,
                        s.source_texture_sampling,
                    );
                }
                if opacity > 0. {
                    if let Some(splat) = s.splat {
                        let values = splat_channels(splat, u, v, s.splat_mask, s.source_texture_sampling);
                        weights(
                            &mut out.layers.splat,
                            cells,
                            cell,
                            values,
                            opacity,
                            s.source_texture_sampling,
                        );
                    }
                    if let Some(alpha) = s.alpha {
                        out.alpha[cell] = alpha_for(
                            out.alpha[cell],
                            alpha.channel_for(u, v, 3, s.source_texture_sampling),
                            opacity,
                            s.source_texture_sampling,
                        );
                    }
                    if let Some(biome) = s.biome {
                        let rgba = biome.rgba_for(u, v, 15, s.source_texture_sampling);
                        let fifth = fifth_for(rgba, s.source_texture_sampling);
                        let values: [f64; 5] = std::array::from_fn(|c| {
                            if s.biome_mask & (1 << c) == 0 {
                                0.
                            } else if c < 4 {
                                rgba[c]
                            } else {
                                fifth
                            }
                        });
                        weights(
                            &mut out.layers.biome,
                            cells,
                            cell,
                            values,
                            opacity,
                            s.source_texture_sampling,
                        );
                    }
                }
                if s.auto_cliff_topology && out.layers.topology[cell] & 8_389_632 == 0 {
                    if slope(&out.layers, x, z) > 30.
                        || f64::from(out.layers.splat[3 * cells + cell]) / 255. > 0.4
                    {
                        out.layers.topology[cell] |= 2;
                    } else {
                        out.layers.topology[cell] &= !2;
                    }
                }
                if let Some(topology) = s.topology {
                    let bits =
                        topology.topology_for(u, v, s.source_texture_sampling) & s.topology_mask;
                    if bits != 0 {
                        if s.replace_topology {
                            out.layers.topology[cell] &= !s.topology_mask;
                        }
                        out.layers.topology[cell] |= bits;
                    }
                }
            }
        }
        Ok(true)
    };
    if !feed(&mut apply)? {
        return Ok(None);
    }
    Ok((!cancelled()).then_some(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quantized_heights_match_owned_smoothstep_and_height_writer() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/server-bd19c4ac-height-blend.json"
        ))
        .unwrap();
        assert_eq!(reference["format"], "mapper-owned-height-blend-1");
        let cases = reference["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 128);
        let mut legacy_differences = 0;
        for case in cases {
            let old = case["old"].as_i64().unwrap() as i16;
            let target: f32 = serde_json::from_value(case["target"].clone()).unwrap();
            let opacity: f32 = serde_json::from_value(case["opacity"].clone()).unwrap();
            let expected = case["expected"].as_i64().unwrap() as i16;
            assert_eq!(
                source_height_blend(old, target, opacity),
                expected,
                "{case}"
            );
            let t = opacity * opacity * (3. - 2. * opacity);
            let from = old as f32 * (1. / 32766.);
            let legacy = ((from + (target - from) * t) * 32766. + 0.5) as i16;
            legacy_differences += usize::from(legacy != expected);
        }
        assert_eq!(legacy_differences, 36);
    }

    #[test]
    fn source_stamp_height_keeps_the_full_ten_cell_border_fade() {
        let mut stamp = stamp();
        stamp.source_texture_sampling = true;
        stamp.source_cliff_generation = true;
        stamp.source_quad_raster = true;
        stamp.source_height_blending = true;
        let blend = [0, 0, 0, 170].repeat(9);
        let pixels = [0; 36];
        stamp.blend = Some(image(&blend, 3));
        let height = image(&pixels, 3);
        let old = 16_383;
        let target = ((stamp.position[1] as f32 + stamp.offset[1] as f32) + 500.)
            * (1. / 1000.);
        for edge in 0..=11 {
            let opacity = (170. * (1. / 255.))
                * source_smoothstep(0., 1., (edge as f64 / 10.).clamp(0., 1.) as f32);
            let expected = (edge != 0).then(|| (source_height_blend(old, target, opacity), false));
            assert_eq!(height_value(&stamp, height, [0.; 2], edge, 30, 65, old), expected);
            assert_eq!(height_value(&stamp, height, [0.; 2], 30, 64 - edge, 65, old), expected);
        }
        stamp.blend = Some(image(&pixels, 3));
        assert_eq!(height_value(&stamp, height, [0.; 2], 30, 30, 65, old), None);
    }

    #[test]
    fn source_cliff_target_can_reduce_rock_and_respects_source_stage_gates() {
        let old = projection::Cell {
            splat: [0, 0, 0, 204, 51, 0, 0, 0],
            biome: [255, 0, 0, 0, 0],
            alpha: 255,
            topology: 0,
        };
        assert_eq!(
            source_set_rock(old.splat, 0.25),
            [0, 0, 0, 64, 191, 0, 0, 0]
        );
        assert_eq!(
            source_set_rock([0, 0, 0, 255, 0, 0, 0, 0], 0.25),
            [0, 0, 0, 255, 0, 0, 0, 0]
        );
        let pixels = [0_u8; 100];
        let tile = Image {
            width: 5,
            height: 5,
            rgba: &pixels,
        };
        let mut s = stamp();
        s.source_texture_sampling = true;
        s.source_cliff_generation = true;
        s.height = None;
        s.blend = None;
        s.alpha = None;
        s.biome = None;
        s.topology = None;
        s.splat = Some([tile; 2]);
        s.splat_mask = 8;
        s.auto_cliff_splat = true;
        s.auto_cliff_topology = true;
        let painted = source_material_cell(&s, old, [0.; 2], || 35.);
        assert_eq!(painted.splat, source_set_rock(old.splat, 0.25));
        s.splat_mask = 0;
        assert_eq!(
            source_material_cell(&s, old, [0.; 2], || panic!("Disabled splat sampled slope")),
            old
        );
        s.splat_mask = 8;
        s.splat = None;
        assert_eq!(
            source_material_cell(&s, old, [0.; 2], || panic!("Missing splat sampled slope")),
            old
        );
        assert_eq!(
            source_topology_cell(
                &s,
                old.topology,
                [0.; 2],
                || panic!("Missing topology sampled slope"),
                || panic!("Missing topology sampled rock")
            ),
            old.topology
        );
        let bits = [16_u8, 0, 0, 0].repeat(25);
        s.topology = Some(Image {
            width: 5,
            height: 5,
            rgba: &bits,
        });
        s.topology_mask = 1024 | 16;
        s.replace_topology = true;
        assert_eq!(
            source_topology_cell(&s, 1024 | 2, [0.; 2], || 0., || 0.),
            16,
            "Authored removal must precede cliff filtering"
        );
    }
    #[test]
    fn source_cliff_topology_samples_neighboring_materials_instead_of_one_cell() {
        let mut reads = 0;
        let rock = source_rock_sample(4, 1, 1, |x, z| {
            reads += 1;
            if x == 1 && z == 1 { 100 } else { 255 }
        });
        assert_eq!(reads, 4);
        assert!(rock > 0.4);
    }
    #[test]
    fn streamed_stamps_release_inputs_and_discard_failed_or_cancelled_feeds() {
        let b = base();
        let alpha = vec![255; b.surface_resolution.pow(2)];
        let original = b.height.clone();
        let result = compose_streamed(
            &b,
            &alpha,
            || false,
            |apply| {
                for yaw in [17., 90.] {
                    let pixels = [0, 0, 0, 255];
                    let mut s = stamp();
                    s.height = Some(Image {
                        width: 1,
                        height: 1,
                        rgba: &pixels,
                    });
                    s.yaw_degrees = yaw;
                    if !apply(&s)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            },
        )
        .unwrap()
        .unwrap();
        assert_ne!(result.layers.height, original);
        assert!(
            compose_streamed(
                &b,
                &alpha,
                || false,
                |apply| {
                    apply(&stamp())?;
                    Err("second texture failed".into())
                }
            )
            .is_err()
        );
        assert!(
            compose_streamed(
                &b,
                &alpha,
                || false,
                |apply| {
                    apply(&stamp())?;
                    Ok(false)
                }
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(b.height, original);
        assert!(
            compose_streamed(
                &b,
                &alpha,
                || false,
                |apply| {
                    for _ in 0..129 {
                        apply(&stamp())?;
                    }
                    Ok(true)
                }
            )
            .is_err()
        );
    }
    #[test]
    fn owned_stamps_reuse_private_rasters_and_match_borrowed_composition() {
        let layers = base();
        let alpha = vec![255; layers.surface_resolution.pow(2)];
        let height = layers.height.as_ptr();
        let splat = layers.splat.as_ptr();
        let topology = layers.topology.as_ptr();
        let alpha_bytes = alpha.as_ptr();
        let stamps = [stamp()];
        let expected = compose(&layers, &alpha, &stamps, || false).unwrap().unwrap();
        let owned = compose_owned_streamed(layers, alpha, || false, |apply| apply(&stamps[0]))
            .unwrap()
            .unwrap();
        assert_eq!(owned.layers.height.as_ptr(), height);
        assert_eq!(owned.layers.splat.as_ptr(), splat);
        assert_eq!(owned.layers.topology.as_ptr(), topology);
        assert_eq!(owned.alpha.as_ptr(), alpha_bytes);
        assert_eq!(owned.layers.height, expected.layers.height);
        assert_eq!(owned.layers.splat, expected.layers.splat);
        assert_eq!(owned.layers.biome, expected.layers.biome);
        assert_eq!(owned.layers.topology, expected.layers.topology);
        assert_eq!(owned.alpha, expected.alpha);
        assert_eq!(owned.clamped_height_samples, expected.clamped_height_samples);
    }
    #[test]
    fn water_revision_reclassifies_final_terrain_and_preserves_legacy_output() {
        let mut base = base();
        base.height.fill(crate::erosion::encode(-5.));
        let alpha = vec![255; base.surface_resolution.pow(2)];
        let mut s = stamp();
        s.offset = [0.; 3];
        s.size = [120., 1., 200.];
        s.height = Some(image(&[0, 0, 0, 255], 1));
        s.yaw_degrees = 37.;
        let legacy = compose(&base, &alpha, &[s], || false).unwrap().unwrap();
        assert!(legacy.ocean.is_none());
        let mut s = stamp();
        s.offset = [0.; 3];
        s.size = [120., 1., 200.];
        s.height = Some(image(&[0, 0, 0, 255], 1));
        let result = compose_with_water(&base, &alpha, &[s], || false)
            .unwrap()
            .unwrap();
        let center = 256 * 512 + 256;
        let ocean = result.ocean.as_ref().unwrap();
        assert_eq!(ocean.classification[center], crate::ocean::LAND);
        assert_eq!(ocean.classification[0], crate::ocean::OCEAN);
        assert_eq!(result.layers.topology[center] & crate::ocean::OCEAN_BIT, 0);
        assert_eq!(ocean.topology, result.layers.topology);
        assert!(ocean.local_water.iter().all(|&h| h == 0));
        assert!(
            base.height
                .iter()
                .all(|&h| h == crate::erosion::encode(-5.))
        );
        for (&old, &new) in legacy.layers.topology.iter().zip(&result.layers.topology) {
            assert_eq!(
                old & !crate::ocean::OCEAN_BIT,
                new & !crate::ocean::OCEAN_BIT
            );
        }
    }

    #[test]
    fn water_revision_opens_a_rotated_channel_and_cancellation_discards_everything() {
        let mut base = base();
        let hr = base.height_resolution;
        base.height.fill(crate::erosion::encode(20.));
        for z in hr * 9 / 10..hr {
            base.height[z * hr..(z + 1) * hr].fill(crate::erosion::encode(-10.));
        }
        let basin_z = ((180. / 1000. + 0.5) * (hr - 1) as f32) as usize;
        for z in basin_z - 10..=basin_z + 10 {
            for x in hr / 2 - 10..=hr / 2 + 10 {
                base.height[z * hr + x] = crate::erosion::encode(-10.);
            }
        }
        let before = crate::ocean::reconstruct(&base.height, hr, &base.topology, 512, || false)
            .unwrap()
            .unwrap();
        let center = (basin_z * 511 / (hr - 1)) * 512 + 256;
        assert_eq!(before.classification[center], crate::ocean::ENCLOSED);
        let alpha = vec![255; 512 * 512];
        let mut s = stamp();
        s.position = [0., -10., 180.];
        s.offset = [0.; 3];
        s.size = [40., 1., 600.];
        s.height = Some(image(&[0, 0, 0, 255], 1));
        let result = compose_with_water(&base, &alpha, &[s], || false)
            .unwrap()
            .unwrap();
        assert_eq!(
            result.ocean.as_ref().unwrap().classification[center],
            crate::ocean::OCEAN
        );
        let calls = std::cell::Cell::new(0);
        let cancelled = compose_with_water(&base, &alpha, &[], || {
            calls.set(calls.get() + 1);
            calls.get() > 20
        })
        .unwrap();
        assert!(cancelled.is_none());
        assert_eq!(
            base.height[basin_z * hr + hr / 2],
            crate::erosion::encode(-10.)
        );
    }
    fn image(rgba: &[u8], width: usize) -> Image<'_> {
        Image {
            width,
            height: rgba.len() / 4 / width,
            rgba,
        }
    }
    pub(super) fn base() -> Layers {
        let (hr, sr) = dimensions(1000).unwrap();
        let cells = sr * sr;
        let mut biome = vec![0; cells * 5];
        biome[..cells].fill(255);
        let mut splat = vec![0; cells * 8];
        splat[4 * cells..5 * cells].fill(255);
        Layers {
            size: 1000,
            terrain_seed: 123,
            biome_seed: 456,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![16383; hr * hr],
            biome,
            splat,
            topology: vec![128; cells],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        }
    }
    pub(super) fn stamp() -> Stamp<'static> {
        Stamp {
            source_cliff_generation: false,
            source_quad_raster: false,
            source_height_blending: false,
            source_texture_sampling: false,
            position: [0., 20., 0.],
            yaw_degrees: 37.,
            size: [200., 600., 200.],
            offset: [0., -300., 0.],
            radius: 100.,
            fade: 20.,
            height: Some(image(&[255, 0, 63, 1], 1)),
            blend: Some(image(&[0, 0, 0, 255], 1)),
            splat: Some([
                image(&[0, 255, 0, 0, 0, 255, 0, 0], 2),
                image(&[0, 0, 0, 0], 1),
            ]),
            splat_mask: 2,
            alpha: Some(image(&[0, 0, 0, 0], 1)),
            biome: Some(image(&[0, 255, 0, 0], 1)),
            biome_mask: 2,
            topology: Some(image(&[32, 0, 0, 128], 1)),
            topology_mask: i32::MIN | 32,
            replace_topology: false,
            auto_cliff_splat: false,
            auto_cliff_topology: false,
        }
    }
    #[test]
    fn projected_height_matches_committed_rotated_raster_and_boat_reachability() {
        let mut b = base();
        b.height.fill(crate::erosion::encode(-10.));
        let alpha = vec![255; b.surface_resolution.pow(2)];
        let terrain = HeightTerrain::new(&b.height, b.height_resolution, b.size).unwrap();
        for yaw in [0., 37., 90., 271.] {
            let mut s = stamp();
            s.yaw_degrees = yaw;
            s.offset = [0.; 3];
            s.position = [12., 0., -19.];
            s.size = [120., 1., 180.];
            s.height = Some(image(&[0, 0, 0, 255], 1));
            s.splat = None;
            s.alpha = None;
            s.biome = None;
            s.topology = None;
            let projection = terrain.projected(&s).unwrap();
            let out = compose(&b, &alpha, std::slice::from_ref(&s), || false)
                .unwrap()
                .unwrap();
            for z in 0..67 {
                for x in 0..67 {
                    let u = x as f32 / 66.;
                    let v = z as f32 / 66.;
                    assert_eq!(
                        projection.sample(u, v).unwrap(),
                        crate::spawn::height(&out.layers, u, v)
                    );
                    let p = [(u - 0.5) * b.size as f32, 0., (v - 0.5) * b.size as f32];
                    let grid = crate::path_terrain::Grid {
                        size: b.size as f32,
                        resolution: b.height_resolution,
                        origin_y: -500.,
                        vertical_span: 1000.,
                        height: &out.layers.height,
                    };
                    assert_eq!(
                        projection.sample_source(p, -500., 1000.).unwrap().to_bits(),
                        crate::path_terrain::sample_height(grid, p)
                            .unwrap()
                            .to_bits()
                    );
                    let (traced, support) = projection
                        .sample_source_with_support(p, -500., 1000.)
                        .unwrap();
                    assert_eq!(
                        traced.to_bits(),
                        crate::path_terrain::sample_height(grid, p)
                            .unwrap()
                            .to_bits()
                    );
                    assert!(
                        support
                            .iter()
                            .all(|i| *i < b.height_resolution * b.height_resolution)
                    );
                }
            }
            let projected = crate::boat_navigation::Navigation::build_sampled(
                b.size,
                7.5,
                4.,
                |u, v| projection.sample(u, v),
                |_, _| Some(0.),
                || false,
            )
            .unwrap()
            .unwrap();
            let committed = crate::boat_navigation::Navigation::build(
                &out.layers,
                7.5,
                4.,
                |_, _| Some(0.),
                || false,
            )
            .unwrap()
            .unwrap();
            assert_eq!(projected.deep_cells, committed.deep_cells);
            assert_eq!(projected.reachable_cells, committed.reachable_cells);
            let projected_source = crate::boat_navigation::Navigation::build_source_sampled(
                b.size,
                7.5,
                4.,
                |p| projection.sample_source(p, -500., 1000.).ok(),
                |_, _| Some(0.),
                || false,
            )
            .unwrap()
            .unwrap();
            let committed_source = crate::boat_navigation::Navigation::build_source(
                &out.layers,
                7.5,
                4.,
                |_, _| Some(0.),
                || false,
            )
            .unwrap()
            .unwrap();
            assert_eq!(projected_source.deep_cells, committed_source.deep_cells);
            assert_eq!(
                projected_source.reachable_cells,
                committed_source.reachable_cells
            );
            for z in 0..31 {
                for x in 0..31 {
                    let p = [
                        (x as f32 / 30. - 0.5) * b.size as f32,
                        0.,
                        (z as f32 / 30. - 0.5) * b.size as f32,
                    ];
                    assert_eq!(
                        projected_source.reaches_edge(p),
                        committed_source.reaches_edge(p)
                    );
                }
            }
            assert!(!projected.reaches_edge([12., 0., -19.]));
            assert!(projected.reaches_edge([400., 0., 400.]));
        }
        assert!(b.height.iter().all(|h| *h == crate::erosion::encode(-10.)));
    }
    #[test]
    fn all_channels_compose_and_moving_or_deleting_restores_the_base() {
        let base = base();
        let cells = base.surface_resolution.pow(2);
        let alpha = vec![255; cells];
        let s = stamp();
        let result = compose(&base, &alpha, &[s], || false).unwrap().unwrap();
        let hi = 512 * 1025 + 512;
        let cell = 256 * 512 + 256;
        assert_eq!(result.layers.height[hi], 17038);
        assert_eq!(result.layers.splat[cells + cell], 255);
        assert_eq!(result.layers.splat[4 * cells + cell], 0);
        assert_eq!(result.layers.biome[cells + cell], 255);
        assert_eq!(result.alpha[cell], 0);
        assert_eq!(result.layers.topology[cell], i32::MIN | 160);
        let mut moved = stamp();
        moved.position = [300., 20., 300.];
        let changed = compose(&base, &alpha, &[moved], || false).unwrap().unwrap();
        assert_eq!(changed.layers.height[hi], base.height[hi]);
        assert_eq!(changed.layers.splat[4 * cells + cell], 255);
        assert_eq!(changed.alpha[cell], 255);
        assert_eq!(changed.layers.topology[cell], 128);
        let removed = compose(&base, &alpha, &[], || false).unwrap().unwrap();
        assert_eq!(removed.layers.height, base.height);
        assert_eq!(removed.layers.splat, base.splat);
        assert_eq!(removed.layers.biome, base.biome);
        assert_eq!(removed.layers.topology, base.topology);
        assert_eq!(removed.alpha, alpha);
        assert_eq!(base.height[hi], 16383);
    }
    #[test]
    fn overlap_order_is_explicit_and_preserves_manually_authored_base_holes() {
        let base = base();
        let mut alpha = vec![255; 512 * 512];
        alpha[0] = 0;
        let mut second = stamp();
        second.position[1] = 40.;
        second.alpha = Some(image(&[0, 0, 0, 255], 1));
        let result = compose(&base, &alpha, &[stamp(), second], || false)
            .unwrap()
            .unwrap();
        assert_eq!(result.alpha[0], 0);
        assert_eq!(result.alpha[256 * 512 + 256], 255);
        assert_eq!(result.layers.height[512 * 1025 + 512], 17694);
    }
    #[test]
    fn packed_height_interpolation_does_not_interpolate_encoded_color_bytes() {
        let tex = image(&[255, 0, 0, 1, 0, 0, 1, 1], 2);
        let value = tex.sample(0.5, 0., |p| f64::from(i16::from_le_bytes([p[0], p[2]])));
        assert_eq!(value, 255.5);
    }
    #[test]
    fn rotated_topology_does_not_paint_the_entire_axis_aligned_bounds() {
        let base = base();
        let alpha = vec![255; 512 * 512];
        let mut s = stamp();
        s.yaw_degrees = 45.;
        s.height = None;
        s.splat = None;
        s.biome = None;
        s.alpha = None;
        let result = compose(&base, &alpha, &[s], || false).unwrap().unwrap();
        // World (123,123) lies inside the AABB but outside the rotated square.
        assert_eq!(result.layers.topology[319 * 512 + 319], 128);
        assert_ne!(result.layers.topology[256 * 512 + 256], 128);
    }
    #[test]
    fn weights_remain_normalized_and_unselected_channels_are_retained() {
        let mut data = vec![64, 0, 0, 0, 191, 0, 0, 0];
        weights(
            &mut data,
            1,
            0,
            [0., 0.3, 0., 0., 0., 0., 0., 0.],
            0.6,
            false,
        );
        assert_eq!(data.iter().map(|&v| u32::from(v)).sum::<u32>(), 255);
        assert!(data[0] > 0 && data[4] > 0 && data[1] > 0);
    }
    #[test]
    fn arbitrary_offset_rotation_is_consistent_and_full_turns_match() {
        let mut s = stamp();
        s.offset[0] = 30.;
        s.offset[2] = -10.;
        let t = Transform::new(&s);
        let point = t.local(t.center[0], t.center[1]);
        assert_eq!(point, [0., 0.]);
        s.yaw_degrees = 397.;
        let turn = Transform::new(&s);
        assert_eq!(t.center, turn.center);
        let world_x = t.center[0] + t.cos * 10.;
        let world_z = t.center[1] + t.sin * 10.;
        let local = t.local(world_x, world_z);
        assert!((local[0] - 10.).abs() < 1e-10 && local[1].abs() < 1e-10);
    }
    #[test]
    fn invalid_input_and_cancellation_do_not_publish_partial_terrain() {
        let base = base();
        let alpha = vec![255; 512 * 512];
        let mut invalid = stamp();
        invalid.fade = f64::NAN;
        assert!(compose(&base, &alpha, &[invalid], || false).is_err());
        let calls = std::cell::Cell::new(0);
        let result = compose(&base, &alpha, &[stamp()], || {
            calls.set(calls.get() + 1);
            calls.get() > 5
        })
        .unwrap();
        assert!(result.is_none());
        assert_eq!(base.height[512 * 1025 + 512], 16383);
        assert!(compose(&base, &alpha[..20], &[], || false).is_err());
    }

    #[test]
    fn coastal_acceptance_scans_past_seabed_and_detects_later_earthwork_failures() {
        let mut base = base();
        base.height.fill(crate::erosion::encode(-10.));
        let terrain = HeightTerrain::new(&base.height, base.height_resolution, base.size).unwrap();
        let mut full_work = 0;
        let full = terrain
            .assess(&stamp(), &mut full_work, || false)
            .unwrap()
            .unwrap();
        assert!(full.submerged_samples > 100);
        let mut coastal_work = 0;
        let coastal = terrain
            .assess_for_coastal_placement_with_budget(
                &stamp(),
                &mut coastal_work,
                64.,
                full_work,
                || false,
            )
            .unwrap()
            .unwrap();
        assert_eq!(coastal, full);
        assert_eq!(coastal_work, full_work);
        assert!(
            terrain
                .assess_for_coastal_placement_with_budget(
                    &stamp(),
                    &mut 0,
                    64.,
                    full_work - 1,
                    || false,
                )
                .is_err()
        );
        assert!(terrain.assess_for_coastal_placement_with_budget(
            &stamp(), &mut 0, 64., full_work, || true,
        ).unwrap().is_none());

        // An early permitted seabed sample must not hide a later excessive cut.
        let h = base.height_resolution;
        base.height[(h / 2) * h + h / 2] = crate::erosion::encode(200.);
        let terrain = HeightTerrain::new(&base.height, h, base.size).unwrap();
        let rejected = terrain
            .assess_for_coastal_placement_with_budget(&stamp(), &mut 0, 64., full_work, || false)
            .unwrap()
            .unwrap();
        assert!(rejected.submerged_samples > 0);
        assert!(rejected.maximum_change > 64.);
        assert!(rejected.samples < full.samples);
    }

    #[test]
    fn per_target_assessment_budget_never_accepts_a_partial_stamp() {
        let mut base = base();
        base.height.fill(crate::erosion::encode(25.));
        let terrain = HeightTerrain::new(&base.height, base.height_resolution, base.size).unwrap();
        let mut full_work = 0;
        let full = terrain
            .assess_for_placement(&stamp(), &mut full_work, 24., || false)
            .unwrap()
            .unwrap();
        assert!(full_work > 2);
        let mut spent = 0;
        assert!(
            terrain
                .assess_for_placement_with_budget(&stamp(), &mut spent, 24., full_work - 1, || {
                    false
                })
                .is_err()
        );
        assert_eq!(spent, full_work);
        let bounded = terrain
            .assess_for_placement_with_budget(&stamp(), &mut 0, 24., full_work, || false)
            .unwrap()
            .unwrap();
        assert_eq!(bounded.samples, full.samples);
        assert_eq!(bounded.maximum_change, full.maximum_change);
        assert!(
            terrain
                .assess_for_placement_with_budget(&stamp(), &mut 0, 24., 0, || false)
                .is_err()
        );
        assert!(
            terrain
                .assess_for_placement_with_budget(&stamp(), &mut 0, 24., full_work, || true)
                .unwrap()
                .is_none()
        );
        assert!(
            base.height
                .iter()
                .all(|&h| h == crate::erosion::encode(25.))
        );
    }
    #[test]
    fn placement_check_matches_detailed_limits_and_work() {
        let mut layers = base();
        let n = layers.height_resolution;
        for (z, row) in layers.height.chunks_exact_mut(n).enumerate() {
            row.fill(if z < n / 2 { 16383 } else { 16420 });
        }
        let terrain = HeightTerrain::new(&layers.height, n, layers.size).unwrap();
        let mut stamp = stamp();
        stamp.yaw_degrees = 49.;
        let budget = MAX_EARTHWORK_SAMPLES;
        for change in [1., 12., 96.] {
            for allow_submerged in [false, true] {
                let mut detailed_work = 0;
                let detailed = if allow_submerged {
                    terrain.assess_for_coastal_placement_with_budget(
                        &stamp, &mut detailed_work, change, budget, || false,
                    )
                } else {
                    terrain.assess_for_placement_with_budget(
                        &stamp, &mut detailed_work, change, budget, || false,
                    )
                }.unwrap().unwrap();
                let mut fast_work = 0;
                let fast = terrain.check_placement(
                    &stamp, &mut fast_work, change, budget, allow_submerged, || false,
                ).unwrap().unwrap();
                assert_eq!(fast_work, detailed_work);
                assert_eq!(fast.samples, detailed.samples);
                assert_eq!(fast.maximum_change.to_bits(), detailed.maximum_change.to_bits());
                assert_eq!(fast.submerged_samples, detailed.submerged_samples);
                assert_eq!(fast.clamped_samples, detailed.clamped_samples);
            }
        }
    }
    #[test]
    fn full_resolution_assessment_matches_composed_cut_fill_at_arbitrary_rotations() {
        let mut base = base();
        base.height.fill(crate::erosion::encode(25.));
        let terrain = HeightTerrain::new(&base.height, base.height_resolution, base.size).unwrap();
        let source_height = [255, 0, 63, 1].repeat(9);
        let source_blend = [0, 0, 0, 255].repeat(9);
        for raster in [false, true] {
            for yaw in [0., 17., 90., 137., 270.] {
                let mut s = stamp();
                if raster {
                    s.source_texture_sampling = true;
                    s.source_cliff_generation = true;
                    s.source_quad_raster = true;
                    s.height = Some(Image {
                        width: 3,
                        height: 3,
                        rgba: &source_height,
                    });
                    s.blend = Some(Image {
                        width: 3,
                        height: 3,
                        rgba: &source_blend,
                    });
                    s.size[0] = 120.; // Height blend rectangle differs from surface radius.
                    s.splat = None;
                    s.alpha = None;
                    s.biome = None;
                    s.topology = None;
                }
                s.yaw_degrees = yaw;
                s.offset[0] = 11.;
                s.offset[2] = -7.;
                let report = terrain.assess(&s, &mut 0, || false).unwrap().unwrap();
                let composed = compose(
                    &base,
                    &vec![255; base.surface_resolution.pow(2)],
                    &[s],
                    || false,
                )
                .unwrap()
                .unwrap();
                let mut cut = 0.;
                let mut fill = 0.;
                let mut max = 0_f64;
                let area = (base.size as f64 / base.height_resolution as f64).powi(2);
                for (&old, &new) in base.height.iter().zip(&composed.layers.height) {
                    let delta = f64::from(new - old) / 32766. * 1000.;
                    cut += (-delta).max(0.) * area;
                    fill += delta.max(0.) * area;
                    max = max.max(delta.abs());
                }
                assert!((report.cut_volume - cut).abs() < 1e-6);
                assert!((report.fill_volume - fill).abs() < 1e-6);
                assert_eq!(report.maximum_change, max);
                assert_eq!(report.clamped_samples, composed.clamped_height_samples);
                assert_eq!(report.submerged_samples, 0);
            }
        }
        assert!(
            base.height
                .iter()
                .all(|&v| v == crate::erosion::encode(25.))
        );
    }

    #[test]
    fn surface_only_modifiers_assess_zero_earthwork_and_preserve_height() {
        let base = base();
        let alpha = vec![255; base.surface_resolution.pow(2)];
        let terrain = HeightTerrain::new(&base.height, base.height_resolution, base.size).unwrap();
        let topology: Vec<u8> = [32_u32; 9].into_iter().flat_map(u32::to_le_bytes).collect();
        for source in [false, true] {
            let mut stamp = stamp();
            stamp.source_texture_sampling = source;
            stamp.source_cliff_generation = source;
            stamp.source_quad_raster = source;
            stamp.source_height_blending = source;
            stamp.height = None;
            stamp.blend = None;
            stamp.splat = None;
            stamp.splat_mask = 0;
            stamp.alpha = None;
            stamp.biome = None;
            stamp.biome_mask = 0;
            stamp.topology = Some(Image {
                width: 3,
                height: 3,
                rgba: &topology,
            });
            stamp.topology_mask = 32;
            let mut work = 7;
            let assessment = terrain
                .assess_for_coastal_placement_with_budget(&stamp, &mut work, 24., 7, || false)
                .unwrap()
                .unwrap();
            assert_eq!(work, 7);
            assert_eq!(
                assessment,
                HeightAssessment {
                    samples: 0,
                    maximum_change: 0.,
                    cut_volume: 0.,
                    fill_volume: 0.,
                    submerged_samples: 0,
                    clamped_samples: 0,
                }
            );
            assert!(
                terrain
                    .assess(&stamp, &mut work, || true)
                    .unwrap()
                    .is_none()
            );
            let projection = terrain.projected(&stamp).unwrap();
            for point in [[0.; 3], [30., 0., -20.], [200., 0., 0.]] {
                assert_eq!(projection.sample_source(point, -500., 1000.).unwrap(), 0.);
            }
            let composed = compose(&base, &alpha, std::slice::from_ref(&stamp), || false)
                .unwrap()
                .unwrap();
            assert_eq!(composed.layers.height, base.height);
            assert_eq!(composed.layers.splat, base.splat);
            assert_eq!(composed.layers.biome, base.biome);
            assert_eq!(composed.alpha, alpha);
            assert!(composed.layers.topology.iter().any(|mask| mask & 32 != 0));
            stamp.position[0] = 490.;
            assert!(
                terrain
                    .assess(&stamp, &mut work, || false)
                    .unwrap_err()
                    .contains("bounds")
            );
            stamp.size[0] = f64::NAN;
            assert!(terrain.assess(&stamp, &mut work, || false).is_err());
        }
    }
    #[test]
    fn assessment_checks_water_limits_bounds_invalid_stamps_and_cancellation() {
        let base = base();
        let terrain = HeightTerrain::new(&base.height, base.height_resolution, base.size).unwrap();
        let report = terrain.assess(&stamp(), &mut 0, || false).unwrap().unwrap();
        assert!(report.submerged_samples > 0);
        let mut edge = stamp();
        edge.position[0] = 490.;
        assert!(
            terrain
                .assess(&edge, &mut 0, || false)
                .unwrap_err()
                .contains("bounds")
        );
        let mut invalid = stamp();
        invalid.size[0] = f64::NAN;
        assert!(terrain.assess(&invalid, &mut 0, || false).is_err());
        assert!(
            terrain
                .assess(&stamp(), &mut 128_000_000, || false)
                .is_err()
        );
        let calls = std::cell::Cell::new(0);
        assert!(
            terrain
                .assess(&stamp(), &mut 0, || {
                    calls.set(calls.get() + 1);
                    calls.get() > 4
                })
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn early_rejection_saves_work_but_accepted_reports_cover_the_full_stamp() {
        let mut base = base();
        base.height.fill(crate::erosion::encode(25.));
        let terrain = HeightTerrain::new(&base.height, base.height_resolution, base.size).unwrap();
        let mut full_work = 0;
        let full = terrain
            .assess(&stamp(), &mut full_work, || false)
            .unwrap()
            .unwrap();
        let mut accepted_work = 0;
        let accepted = terrain
            .assess_for_placement(&stamp(), &mut accepted_work, 24., || false)
            .unwrap()
            .unwrap();
        assert_eq!(full, accepted);
        assert_eq!(full_work, accepted_work);
        let mut rejected_work = 0;
        let rejected = terrain
            .assess_for_placement(&stamp(), &mut rejected_work, 1., || false)
            .unwrap()
            .unwrap();
        assert!(rejected.maximum_change > 1.);
        assert!(rejected_work < full_work / 10);
    }
}
