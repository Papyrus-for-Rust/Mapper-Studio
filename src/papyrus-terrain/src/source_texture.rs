//! Inspected TextureData sampling over decoded, top-row-first RGBA8 caches.
//! UVs use the source texture's bottom-left origin. These queries alone do not
//! certify stamp transforms, world placement, blending or a complete map.
pub const VERSION: &str = "papyrus-source-texture-sampling-1";
pub struct Texture<'a> {
    pub width: usize,
    pub height: usize,
    pub rgba: &'a [u8],
}
struct Bilinear<'a> {
    pixels: [&'a [u8]; 4],
    fraction: [f32; 2],
}

impl Bilinear<'_> {
    fn sample(&self, read: impl Fn(&[u8]) -> f32) -> f32 {
        let [a, b, c, d] = self.pixels.map(read);
        let [tx, ty] = self.fraction;
        let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
        lerp(lerp(a, b, tx), lerp(c, d, tx), ty)
    }
}

impl Texture<'_> {
    fn valid_shape(&self) -> bool {
        (3..=4096).contains(&self.width)
            && (3..=4096).contains(&self.height)
            && self.rgba.len() == self.width * self.height * 4
    }
    fn coordinates_with_valid_shape(&self, uv: [f32; 2]) -> Result<[f32; 2], &'static str> {
        if uv.iter().any(|v| !v.is_finite() || v.abs() > 10000.) {
            return Err("Invalid source texture or coordinates");
        }
        Ok([
            uv[0] * (self.width - 1) as u32 as f32,
            uv[1] * (self.height - 1) as u32 as f32,
        ])
    }
    fn pixel(&self, x: usize, y: usize) -> &[u8] {
        let index = ((self.height - 1 - y) * self.width + x) * 4;
        &self.rgba[index..index + 4]
    }
    fn bilinear(&self, uv: [f32; 2]) -> Result<Bilinear<'_>, &'static str> {
        debug_assert!(self.valid_shape());
        let [x, y] = self.coordinates_with_valid_shape(uv)?;
        let a = (x as i32).clamp(1, self.width as i32 - 2);
        let b = (y as i32).clamp(1, self.height as i32 - 2);
        let tx = (x - a as f32).clamp(0., 1.);
        let ty = (y - b as f32).clamp(0., 1.);
        let (a, b) = (a as usize, b as usize);
        let c = (a + 1).min(self.width - 2);
        let d = (b + 1).min(self.height - 2);
        Ok(Bilinear {
            pixels: [
                self.pixel(a, b),
                self.pixel(c, b),
                self.pixel(a, d),
                self.pixel(c, d),
            ],
            fraction: [tx, ty],
        })
    }

    fn interpolated(&self, uv: [f32; 2], read: impl Fn(&[u8]) -> f32) -> Result<f32, &'static str> {
        Ok(self.bilinear(uv)?.sample(read))
    }

    /// Reuse the clamped texels and fractions for all enabled paint channels.
    pub(crate) fn rgba_on_validated_stamp(
        &self,
        uv: [f32; 2],
        mask: u8,
    ) -> Result<[f32; 4], &'static str> {
        if mask == 0 {
            return Ok([0.; 4]);
        }
        let samples = self.bilinear(uv)?;
        Ok(std::array::from_fn(|c| {
            if mask & (1 << c) == 0 {
                0.
            } else {
                samples.sample(|p| p[c] as f32 * (1. / 255.))
            }
        }))
    }
    pub fn channel(&self, uv: [f32; 2], channel: usize) -> Result<f32, &'static str> {
        if channel >= 4 {
            return Err("Invalid source texture channel");
        }
        if !self.valid_shape() {
            return Err("Invalid source texture or coordinates");
        }
        self.interpolated(uv, |p| p[channel] as f32 * (1. / 255.))
    }

    /// Stamp preparation establishes the image dimensions and storage shape.
    pub(crate) fn alpha_on_validated_stamp(&self, uv: [f32; 2]) -> Result<f32, &'static str> {
        self.interpolated(uv, |p| p[3] as f32 * (1. / 255.))
    }

    /// Source GetHalf uses signed red/blue encoding, not IEEE binary16.
    pub fn height(&self, uv: [f32; 2]) -> Result<f32, &'static str> {
        if !self.valid_shape() {
            return Err("Invalid source texture or coordinates");
        }
        self.height_on_validated_stamp(uv)
    }

    pub(crate) fn height_on_validated_stamp(&self, uv: [f32; 2]) -> Result<f32, &'static str> {
        self.interpolated(uv, |p| {
            i16::from_le_bytes([p[0], p[2]]) as f32 * (1. / 32766.)
        })
    }
    pub fn topology(&self, uv: [f32; 2]) -> Result<i32, &'static str> {
        if !self.valid_shape() {
            return Err("Invalid source texture or coordinates");
        }
        let [x, y] = self.coordinates_with_valid_shape(uv)?;
        let x = (x.round_ties_even() as i32).clamp(1, self.width as i32 - 2) as usize;
        let y = (y.round_ties_even() as i32).clamp(1, self.height as i32 - 2) as usize;
        Ok(i32::from_le_bytes(self.pixel(x, y).try_into().unwrap()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stamp_sampling_keeps_source_interpolation_and_edges() {
        let pixels: Vec<_> = (0..7 * 5 * 4).map(|index| (index * 37) as u8).collect();
        let texture = Texture {
            width: 7,
            height: 5,
            rgba: &pixels,
        };
        let original = |[u, v]: [f32; 2], read: &dyn Fn(&[u8]) -> f32| {
            let x = u * (texture.width - 1) as f32;
            let y = v * (texture.height - 1) as f32;
            let a = (x as i32).clamp(1, texture.width as i32 - 2) as usize;
            let b = (y as i32).clamp(1, texture.height as i32 - 2) as usize;
            let c = (a + 1).min(texture.width - 2);
            let d = (b + 1).min(texture.height - 2);
            let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t.clamp(0., 1.);
            let low = lerp(
                read(texture.pixel(a, b)),
                read(texture.pixel(c, b)),
                x - a as f32,
            );
            let high = lerp(
                read(texture.pixel(a, d)),
                read(texture.pixel(c, d)),
                x - a as f32,
            );
            lerp(low, high, y - b as f32).to_bits()
        };
        for uv in [
            [-1., 2.],
            [0., 0.],
            [0.3, 0.41],
            [0.5, 0.5],
            [0.999, 0.1],
            [1., 1.],
        ] {
            assert_eq!(
                texture.alpha_on_validated_stamp(uv).unwrap().to_bits(),
                original(uv, &|p| p[3] as f32 * (1. / 255.))
            );
            assert_eq!(
                texture.height_on_validated_stamp(uv).unwrap().to_bits(),
                original(uv, &|p| i16::from_le_bytes([p[0], p[2]]) as f32
                    * (1. / 32766.))
            );
            for mask in 0..16 {
                let sampled = texture.rgba_on_validated_stamp(uv, mask).unwrap();
                let expected = std::array::from_fn(|c| {
                    if mask & (1 << c) != 0 {
                        original(uv, &|p| p[c] as f32 * (1. / 255.))
                    } else {
                        0
                    }
                });
                assert_eq!(sampled.map(f32::to_bits), expected);
            }
        }
    }

    #[test]
    fn source_border_is_excluded_and_storage_rows_are_flipped_once() {
        let mut pixels = vec![255; 5 * 5 * 4];
        for y in 1..4 {
            for x in 1..4 {
                let i = ((4 - y) * 5 + x) * 4;
                pixels[i..i + 4].copy_from_slice(&[x as u8, y as u8, 0, 0]);
            }
        }
        let texture = Texture {
            width: 5,
            height: 5,
            rgba: &pixels,
        };
        assert_eq!(texture.channel([-1., -1.], 0).unwrap(), 1. / 255.);
        assert_eq!(texture.channel([2., 2.], 1).unwrap(), 3. * (1. / 255.));
        assert_eq!(texture.channel([0.5, 0.25], 1).unwrap(), 1. / 255.);
        assert_eq!(
            texture.topology([0.375, 0.625]).unwrap(),
            i32::from_le_bytes([2, 2, 0, 0])
        );
    }
    #[test]
    fn source_queries_reject_unsupported_small_images_and_invalid_values() {
        let texture = Texture {
            width: 2,
            height: 2,
            rgba: &[0; 16],
        };
        assert!(texture.height([0.; 2]).is_err());
        let texture = Texture {
            width: 3,
            height: 3,
            rgba: &[0; 36],
        };
        assert!(texture.channel([f32::NAN, 0.], 0).is_err());
        assert!(texture.channel([0.; 2], 4).is_err());
        assert!(texture.topology([10001., 0.]).is_err());
    }
}
