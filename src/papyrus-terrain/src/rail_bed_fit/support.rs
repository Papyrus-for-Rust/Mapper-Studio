//! Height intervals over the full-weight bed between neighbouring controls.
use crate::{erosion::decode, native_layers::Layers, path_geometry::Path, rail_bed::Shape};

pub(super) struct Support<'a> {
    base: &'a Layers,
    alpha: &'a [u8],
    shape: Shape,
    allowance: f64,
    maximum_grade: f64,
    spacing: f64,
    axis: Vec<(f32, usize)>,
}

/// Bound the horizontal derivative through the cubic Bezier derivative's convex
/// hull. Regrading preserves control XZ and tangent headings. The continuous
/// grade check bounds their horizontal unit-vector lengths and total step size.
fn span_bound(
    path: &Path,
    span: usize,
    horizontal_step: f64,
    grade: f64,
) -> Result<(f64, f64), String> {
    let ratio = (1. + grade * grade).sqrt();
    let minimum = horizontal_step / ratio;
    let maximum = f64::from(path.step_size()).max(horizontal_step * ratio);
    let tangent = |index: usize| -> Result<([f64; 2], f64), String> {
        let value = path.tangents()[index];
        let vector = [f64::from(value[0]), f64::from(value[2])];
        let length = vector[0].hypot(vector[1]);
        if length < 1e-12 {
            return Err("Rail support has a degenerate horizontal tangent".into());
        }
        Ok((
            vector.map(|v| v / length),
            length * f64::from(path.step_size()),
        ))
    };
    let (a, old_a) = tangent(span)?;
    let (b, old_b) = tangent(span + 1)?;
    let points = path.points();
    let chord = [0, 2].map(|axis| 3. * f64::from(points[span + 1][axis] - points[span][axis]));
    let mut speed = maximum;
    for left in [minimum, maximum] {
        for right in [minimum, maximum] {
            speed = speed.max(
                (chord[0] - left * a[0] - right * b[0])
                    .hypot(chord[1] - left * a[1] - right * b[1]),
            );
        }
    }
    let movement = [old_a, old_b]
        .into_iter()
        .map(|old| (old - minimum).abs().max((old - maximum).abs()) / 3.)
        .fold(0., f64::max);
    Ok((speed, movement))
}

impl<'a> Support<'a> {
    pub(super) fn new(
        base: &'a Layers,
        alpha: &'a [u8],
        shape: Shape,
        allowance: f64,
        maximum_grade: f64,
        spacing: f64,
    ) -> Self {
        let resolution = base.height_resolution;
        let n = base.surface_resolution;
        let size = base.size as f32;
        let axis = (0..resolution)
            .map(|i| {
                let world = (i as f32 / (resolution - 1) as f32 - 0.5) * size;
                let surface = ((world / size + 0.5) * n as f32).clamp(0., (n - 1) as f32) as usize;
                (world, surface)
            })
            .collect();
        Self {
            base,
            alpha,
            shape,
            allowance,
            maximum_grade,
            spacing,
            axis,
        }
    }

    pub(super) fn bounds(
        &self,
        path: &Path,
        control: usize,
        horizontal_step: f64,
        work: &mut usize,
        maximum_work: usize,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Option<(f64, f64)>, String> {
        let point = path.points()[control];
        let height = f64::from(crate::spawn::height(
            self.base,
            point[0] / self.base.size as f32 + 0.5,
            point[2] / self.base.size as f32 + 0.5,
        ));
        let mut lower = height - self.allowance;
        let mut upper = height + self.allowance;
        let count = path.points().len();
        let resolution = self.base.height_resolution;
        let surface_resolution = self.base.surface_resolution;
        let grid =
            |position: f64| (position / f64::from(self.base.size) + 0.5) * (resolution - 1) as f64;
        for span in control.saturating_sub(1)..=control.min(count - 2) {
            let (from, to) = if span < control { (0.5, 1.) } else { (0., 0.5) };
            let (speed, movement) = span_bound(path, span, horizontal_step, self.maximum_grade)?;
            // Cover changed sampling phase and the bounded XZ handle movement.
            let radius = f64::from(self.shape.width * 0.5 + self.shape.outer_padding)
                + self.spacing
                + movement;
            let radius_squared = radius * radius;
            let steps = (((to - from) * f64::from(path.length())
                / (count - 1) as f64
                / self.spacing)
                .ceil() as usize)
                .max(1);
            if steps > 100000 {
                return Err("Rail support sampling exceeds its allowance".into());
            }
            let sample = |fraction: f64| {
                path.point(
                    ((span as f64 + fraction) / (count - 1) as f64 * f64::from(path.length()))
                        as f32,
                )
                .map_err(|error| format!("Rail support sampling failed: {error:?}"))
            };
            let mut a = sample(from)?;
            for step in 0..steps {
                if cancelled() {
                    return Ok(None);
                }
                if *work >= maximum_work {
                    return Err("Connected rail height bounds reached their work allowance".into());
                }
                *work += 1;
                let begin = from + (to - from) * step as f64 / steps as f64;
                let end = from + (to - from) * (step + 1) as f64 / steps as f64;
                let b = sample(end)?;
                let excursion = (span as f64 + begin - control as f64)
                    .abs()
                    .max((span as f64 + end - control as f64).abs())
                    * speed
                    * self.maximum_grade;
                let delta = [0, 2].map(|axis| f64::from(b[axis] - a[axis]));
                let squared = delta[0] * delta[0] + delta[1] * delta[1];
                let minimum = [0, 2].map(|axis| {
                    grid(f64::from(a[axis].min(b[axis])) - radius)
                        .floor()
                        .clamp(0., (resolution - 1) as f64) as usize
                });
                let maximum = [0, 2].map(|axis| {
                    grid(f64::from(a[axis].max(b[axis])) + radius)
                        .ceil()
                        .clamp(0., (resolution - 1) as f64) as usize
                });
                // The allowance and excursion are constant over this sweep.
                // Encoded extrema give the same final lower and upper bounds.
                let mut lowest = i16::MAX;
                let mut highest = i16::MIN;
                let width = maximum[0] - minimum[0] + 1;
                for z in minimum[1]..=maximum[1] {
                    if cancelled() {
                        return Ok(None);
                    }
                    let charged = width.min(maximum_work.saturating_sub(*work));
                    *work += charged;
                    if charged != width {
                        return Err(
                            "Connected rail height bounds reached their work allowance".into()
                        );
                    }
                    let (world_z, surface_z) = self.axis[z];
                    let relative_z = f64::from(world_z - a[2]);
                    let surface_row = surface_z * surface_resolution;
                    let height_row = z * resolution;
                    for x in minimum[0]..=maximum[0] {
                        let (world_x, surface_x) = self.axis[x];
                        let relative_x = f64::from(world_x - a[0]);
                        let fraction = if squared > 1e-15 {
                            ((relative_x * delta[0] + relative_z * delta[1]) / squared)
                                .clamp(0., 1.)
                        } else {
                            0.
                        };
                        let distance_squared = (relative_x - fraction * delta[0]).powi(2)
                            + (relative_z - fraction * delta[1]).powi(2);
                        if distance_squared > radius_squared {
                            continue;
                        }
                        let cell = surface_row + surface_x;
                        if self.alpha[cell] < 128
                            || self.base.topology[cell] as u32 & self.shape.preserve_topology != 0
                        {
                            continue;
                        }
                        let height = self.base.height[height_row + x];
                        lowest = lowest.min(height);
                        highest = highest.max(height);
                    }
                }
                if lowest <= highest {
                    lower = lower.max(f64::from(decode(highest)) - self.allowance + excursion);
                    upper = upper.min(f64::from(decode(lowest)) + self.allowance - excursion);
                }
                a = b;
            }
        }
        Ok(Some((lower, upper)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derivative_bound_covers_short_flat_control_sequences() {
        for count in [2, 3, 10] {
            let controls: Vec<_> = (0..count)
                .map(|index| [index as f32 * 32., 0., 0.])
                .collect();
            let path = Path::from_controls(&controls, 10000, || false)
                .unwrap()
                .unwrap();
            for span in 0..count - 1 {
                let (speed, _) =
                    span_bound(&path, span, f64::from(path.step_size()), 0.15).unwrap();
                let chord = f64::from(controls[span + 1][0] - controls[span][0]);
                // At the span midpoint, dP/dt = 1.5 * chord - 0.25 * (Ta + Tb).
                let midpoint_speed = 1.5 * chord - 0.5 * f64::from(path.step_size());
                assert!(speed >= midpoint_speed);
            }
        }
    }
}
