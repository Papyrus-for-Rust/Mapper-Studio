//! Bounded source-style path smoothing, resampling and cubic Hermite geometry.
//! This is a path primitive, not a validated rail network or train simulation.
use serde::Serialize;
type V = [f32; 3];
pub const VERSION: &str = "papyrus-source-path-1";
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub horizontal_iterations: u16,
    pub vertical_iterations: u16,
    pub resample_distance: f32,
    pub maximum_work: usize,
}
#[derive(Debug, PartialEq)]
pub enum Failure {
    Invalid,
    Budget,
}
#[derive(Clone, Debug, Serialize)]
pub struct Path {
    engine: &'static str,
    points: Vec<V>,
    tangents: Vec<V>,
    length: f32,
    step_size: f32,
    circular: bool,
    work: usize,
    min_index: usize,
    max_index: usize,
}
fn add(a: V, b: V) -> V {
    std::array::from_fn(|i| a[i] + b[i])
}
fn sub(a: V, b: V) -> V {
    std::array::from_fn(|i| a[i] - b[i])
}
fn mul(a: V, s: f32) -> V {
    a.map(|v| v * s)
}
fn magnitude(a: V) -> f32 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}
fn normal(a: V) -> V {
    let length = magnitude(a);
    if length > 0.00001 {
        a.map(|v| v / length)
    } else {
        [0.; 3]
    }
}
// Independent unit-vector interpolation. Exact antipodes still need a separate
// orientation policy. Near antipodes retain their unique interpolation plane.
fn spherical(a: V, b: V, t: f32) -> Result<V, Failure> {
    let dot = a
        .iter()
        .zip(b)
        .map(|(a, b)| a * b)
        .sum::<f32>()
        .clamp(-1., 1.);
    if magnitude(a) < 0.99 || magnitude(b) < 0.99 {
        return Err(Failure::Invalid);
    }
    if dot < -0.9995 {
        // The ordinary sin(theta) formula loses precision near pi. Normalize
        // in f64 and resolve the plane from b's component perpendicular to a.
        let mut start = a.map(f64::from);
        let mut end = b.map(f64::from);
        let start_length = start.iter().map(|v| v * v).sum::<f64>().sqrt();
        let end_length = end.iter().map(|v| v * v).sum::<f64>().sqrt();
        start.iter_mut().for_each(|v| *v /= start_length);
        end.iter_mut().for_each(|v| *v /= end_length);
        let cosine = start
            .iter()
            .zip(end)
            .map(|(a, b)| a * b)
            .sum::<f64>()
            .clamp(-1., 1.);
        let perpendicular: [f64; 3] = std::array::from_fn(|i| end[i] - start[i] * cosine);
        let sine = perpendicular.iter().map(|v| v * v).sum::<f64>().sqrt();
        if sine <= 1e-12 {
            return Err(Failure::Invalid);
        }
        let angle = sine.atan2(cosine) * f64::from(t);
        let (along_plane, along_start) = angle.sin_cos();
        return Ok(normal(std::array::from_fn(|i| {
            (start[i] * along_start + perpendicular[i] / sine * along_plane) as f32
        })));
    }
    Ok(if dot > 0.9995 {
        normal(add(mul(a, 1. - t), mul(b, t)))
    } else {
        let angle = dot.acos();
        normal(add(
            mul(a, ((1. - t) * angle).sin() / angle.sin()),
            mul(b, (t * angle).sin() / angle.sin()),
        ))
    })
}
fn visit(work: &mut usize, maximum: usize) -> Result<(), Failure> {
    if *work >= maximum {
        Err(Failure::Budget)
    } else {
        *work += 1;
        Ok(())
    }
}
/// Immutable inputs; cancellation or a budget failure cannot publish a partial path.
/// `weights` are per original control point and are used by both smoothing passes.
pub fn build(
    points: &[V],
    settings: Settings,
    weights: Option<&[f32]>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Path>, Failure> {
    if cancelled() {
        return Ok(None);
    }
    if !(2..=32768).contains(&points.len())
        || points
            .iter()
            .flatten()
            .any(|v| !v.is_finite() || v.abs() > 10000.)
        || settings.horizontal_iterations > 256
        || settings.vertical_iterations > 256
        || !settings.resample_distance.is_finite()
        || !(0.1..=100.).contains(&settings.resample_distance)
        || !(1..=32000000).contains(&settings.maximum_work)
        || weights.is_some_and(|w| {
            w.len() != points.len() || w.iter().any(|v| !v.is_finite() || !(0. ..=1.).contains(v))
        })
    {
        return Err(Failure::Invalid);
    }
    let circular = magnitude(sub(points[0], points[points.len() - 1])) < 0.1;
    let mut p = points.to_vec();
    let mut work = 0;
    // The source uses two in-place alternating-index sweeps, not a Jacobi pass.
    for (iterations, axes) in [
        (settings.horizontal_iterations, [1., 0., 1.]),
        (settings.vertical_iterations, [0., 1., 0.]),
    ] {
        for _ in 0..iterations {
            for parity in 0..2 {
                let start = usize::from(!circular) + parity;
                for i in (start..p.len() - 1).step_by(2) {
                    if i % 64 == start % 64 && cancelled() {
                        return Ok(None);
                    }
                    visit(&mut work, settings.maximum_work)?;
                    let previous = if i == 0 { p.len() - 2 } else { i - 1 };
                    let average = mul(add(add(add(p[previous], p[i]), p[i]), p[i + 1]), 0.25);
                    let weight = weights.map_or(1., |w| w[i]);
                    p[i] = std::array::from_fn(|axis| {
                        p[i][axis] + (average[axis] - p[i][axis]) * (axes[axis] * weight)
                    });
                    if i == 0 {
                        let last = p.len() - 1;
                        p[last] = p[0];
                    }
                }
            }
        }
    }
    let mut length = 0.;
    for pair in p.windows(2) {
        if cancelled() {
            return Ok(None);
        }
        visit(&mut work, settings.maximum_work)?;
        length += magnitude(sub(pair[1], pair[0]));
    }
    if !length.is_finite() {
        return Err(Failure::Invalid);
    }
    let count = (length / settings.resample_distance).round_ties_even();
    if count > 65536. {
        return Err(Failure::Budget);
    }
    if count >= 2. {
        let distance = length / (count - 1.);
        let mut carried = 0.;
        let mut sampled = Vec::with_capacity(count as usize + 1);
        for (i, pair) in p.windows(2).enumerate() {
            if cancelled() {
                return Ok(None);
            }
            visit(&mut work, settings.maximum_work)?;
            let mut a = pair[0];
            let b = pair[1];
            let mut remaining = magnitude(sub(b, a));
            if i == 0 {
                sampled.push(a);
            }
            while carried + remaining > distance {
                if cancelled() {
                    return Ok(None);
                }
                visit(&mut work, settings.maximum_work)?;
                let gap = distance - carried;
                // A previous segment can finish exactly on the next sample.
                // Source Resample emits its boundary at t=0, clears carried,
                // then advances normally on this segment. Rejecting gap==0
                // incorrectly discarded otherwise valid straight road paths.
                if remaining <= 0. || gap < 0. {
                    return Err(Failure::Invalid);
                }
                let t = (gap / remaining).clamp(0., 1.);
                a = add(a, mul(sub(b, a), t));
                sampled.push(a);
                carried = 0.;
                remaining -= gap;
                if sampled.len() > 65536 {
                    return Err(Failure::Budget);
                }
            }
            carried += remaining;
            if i + 2 == p.len() && carried > distance * 0.5 {
                sampled.push(b);
            }
            if sampled.len() > 65536 {
                return Err(Failure::Budget);
            }
        }
        if sampled.len() >= 2 {
            p = sampled;
        }
    }
    finish(p, circular, work, settings.maximum_work, cancelled)
}
fn finish(
    p: Vec<V>,
    circular: bool,
    mut work: usize,
    maximum_work: usize,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Path>, Failure> {
    let mut tangents = Vec::with_capacity(p.len());
    let mut length = 0.;
    for i in 0..p.len() {
        if i % 64 == 0 && cancelled() {
            return Ok(None);
        }
        visit(&mut work, maximum_work)?;
        let previous = if i == 0 {
            if circular { p.len() - 2 } else { 0 }
        } else {
            i - 1
        };
        let next = if i + 1 == p.len() {
            if circular { 1 } else { p.len() - 1 }
        } else {
            i + 1
        };
        tangents.push(normal(sub(p[next], p[previous])));
        if i + 1 < p.len() {
            length += magnitude(sub(p[i + 1], p[i]));
        }
    }
    if cancelled() {
        return Ok(None);
    }
    let max_index = p.len() - 1;
    Ok(Some(Path {
        engine: VERSION,
        step_size: length / p.len() as f32,
        length,
        points: p,
        tangents,
        circular,
        work,
        min_index: 0,
        max_index,
    }))
}
impl Path {
    /// Reattach open-road endpoints to moved graph junctions. Only endpoints and
    /// tangents whose neighbors changed are updated; interior straightened
    /// tangents remain intact. Final grade/terrain validation is a later pass.
    pub fn reattach_endpoints(
        &self,
        endpoints: [Option<V>; 2],
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, Failure> {
        if self.circular
            || self.range() != (0, self.points.len() - 1)
            || !(1..=32_000_000).contains(&maximum_work)
            || endpoints
                .iter()
                .flatten()
                .flatten()
                .any(|v| !v.is_finite() || v.abs() > 10000.)
        {
            return Err(Failure::Invalid);
        }
        if cancelled() {
            return Ok(None);
        }
        let mut work = self.points.len() * 2;
        if work > maximum_work {
            return Err(Failure::Budget);
        }
        let mut output = self.clone();
        let last = self.points.len() - 1;
        let mut tangents = std::collections::BTreeSet::new();
        for (side, index) in [0, last].into_iter().enumerate() {
            if let Some(point) = endpoints[side] {
                output.points[index] = point;
                tangents.insert(index);
                tangents.insert(if side == 0 { 1 } else { last - 1 });
            }
        }
        for (side, index) in [0, last].into_iter().enumerate() {
            let neighbor = if side == 0 { 1 } else { last - 1 };
            if endpoints[side].is_some()
                && magnitude(sub(output.points[index], output.points[neighbor])) <= 0.00001
            {
                return Err(Failure::Invalid);
            }
        }
        for index in tangents {
            if cancelled() {
                return Ok(None);
            }
            visit(&mut work, maximum_work)?;
            let tangent = normal(sub(
                output.points[(index + 1).min(last)],
                output.points[index.saturating_sub(1)],
            ));
            if magnitude(tangent) < 0.99 {
                return Err(Failure::Invalid);
            }
            output.tangents[index] = tangent;
        }
        let mut length = 0.;
        for pair in output.points.windows(2) {
            if cancelled() {
                return Ok(None);
            }
            visit(&mut work, maximum_work)?;
            length += magnitude(sub(pair[1], pair[0]));
        }
        if !length.is_finite() {
            return Err(Failure::Invalid);
        }
        output.length = length;
        output.step_size = length / output.points.len() as f32;
        output.work = work;
        Ok(Some(output))
    }
    /// Straighten ordered inclusive control spans without resampling. Source
    /// Straighten preserves endpoint/outside tangents and interpolates interior
    /// tangents spherically; RecalculateLength runs only after the final span.
    /// This independent spherical implementation rejects zero/antipodal tangent
    /// endpoints. It is not certified as Unity's native Vector3.Slerp routine.
    /// Input is immutable on cancellation, invalid spans and budget exhaustion.
    pub fn straighten(
        &self,
        spans: &[(usize, usize)],
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, Failure> {
        if spans.len() > 4096
            || !(1..=32_000_000).contains(&maximum_work)
            || spans
                .iter()
                .any(|&(a, b)| a >= b || a < self.min_index || b > self.max_index)
        {
            return Err(Failure::Invalid);
        }
        if cancelled() {
            return Ok(None);
        }
        let mut work = self.points.len() * 2;
        if work > maximum_work {
            return Err(Failure::Budget);
        }
        let mut output = self.clone();
        for &(start, end) in spans {
            if cancelled() {
                return Ok(None);
            }
            visit(&mut work, maximum_work)?;
            let a = output.points[start];
            let b = output.points[end];
            let ta = output.tangents[start];
            let tb = output.tangents[end];
            let inverse = 1. / (end - start) as f32;
            for i in start + 1..end {
                if cancelled() {
                    return Ok(None);
                }
                visit(&mut work, maximum_work)?;
                let t = (i - start) as f32 * inverse;
                output.points[i] = std::array::from_fn(|axis| a[axis] + (b[axis] - a[axis]) * t);
                output.tangents[i] = spherical(ta, tb, t)?;
            }
        }
        let mut length = 0.;
        for pair in output.points.windows(2) {
            if cancelled() {
                return Ok(None);
            }
            visit(&mut work, maximum_work)?;
            length += magnitude(sub(pair[1], pair[0]));
        }
        if !length.is_finite() {
            return Err(Failure::Invalid);
        }
        output.length = length;
        output.step_size = length / output.points.len() as f32;
        output.work = work;
        Ok(Some(output))
    }
    /// Validated controls without smoothing/resampling, for downstream passes
    /// that must retain source connector/junction indices.
    pub fn from_controls(
        points: &[V],
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, Failure> {
        if !(2..=65536).contains(&points.len())
            || !(1..=32000000).contains(&maximum_work)
            || points
                .iter()
                .flatten()
                .any(|v| !v.is_finite() || v.abs() > 10000.)
        {
            return Err(Failure::Invalid);
        }
        if cancelled() {
            return Ok(None);
        }
        finish(
            points.to_vec(),
            magnitude(sub(points[0], points[points.len() - 1])) < 0.1,
            0,
            maximum_work,
            cancelled,
        )
    }
    /// Replace vertical controls and run source-style Y smoothing without
    /// resampling. Control indices remain stable. Smoothing a circular path
    /// canonicalizes its final endpoint like the source; zero iterations leave
    /// all XZ controls intact. Used by terrain fitting passes.
    pub fn refit_heights(
        &self,
        heights: &[f32],
        iterations: u16,
        weights: &[f32],
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, Failure> {
        if heights.len() != self.points.len()
            || weights.len() != heights.len()
            || iterations > 256
            || self.range() != (0, self.points.len() - 1)
            || !(1..=32000000).contains(&maximum_work)
            || heights.iter().any(|v| !v.is_finite() || v.abs() > 10000.)
            || weights
                .iter()
                .any(|v| !v.is_finite() || !(0. ..=1.).contains(v))
        {
            return Err(Failure::Invalid);
        }
        if cancelled() {
            return Ok(None);
        }
        let mut p = self.points.clone();
        let mut work = 0;
        for (point, &height) in p.iter_mut().zip(heights) {
            visit(&mut work, maximum_work)?;
            point[1] = height;
        }
        for _ in 0..iterations {
            for parity in 0..2 {
                let start = usize::from(!self.circular) + parity;
                for i in (start..p.len() - 1).step_by(2) {
                    if cancelled() {
                        return Ok(None);
                    }
                    visit(&mut work, maximum_work)?;
                    let previous = if i == 0 { p.len() - 2 } else { i - 1 };
                    let average = (p[previous][1] + p[i][1] + p[i][1] + p[i + 1][1]) * 0.25;
                    p[i][1] += (average - p[i][1]) * weights[i];
                    if i == 0 {
                        let last = p.len() - 1;
                        p[last] = p[0];
                    }
                }
            }
        }
        finish(p, self.circular, work, maximum_work, cancelled)
    }
    pub fn points(&self) -> &[V] {
        &self.points
    }
    pub fn tangents(&self) -> &[V] {
        &self.tangents
    }
    pub fn length(&self) -> f32 {
        self.length
    }
    pub fn step_size(&self) -> f32 {
        self.step_size
    }
    pub fn is_circular(&self) -> bool {
        self.circular
    }
    pub fn work(&self) -> usize {
        self.work
    }
    pub fn range(&self) -> (usize, usize) {
        (self.min_index, self.max_index)
    }
    pub fn offsets(&self) -> (f32, f32) {
        // Keep the inspected getter's multiply-before-divide order. Moving
        // the division first shifts trimmed roadside sampling in f32.
        let denominator = (self.points.len() - 1) as f32;
        (
            self.length * self.min_index as f32 / denominator,
            self.length * (self.points.len() - 1 - self.max_index) as f32 / denominator,
        )
    }
    /// The source interpolates stored unit tangents spherically; this is not
    /// the derivative of its Hermite position curve. Opposing/zero tangents
    /// are rejected instead of inventing an orientation for object placement.
    pub fn tangent(&self, distance: f32) -> Result<V, Failure> {
        if !distance.is_finite() || self.length <= 0. {
            return Err(Failure::Invalid);
        }
        let coordinate = distance / self.length * (self.tangents.len() - 1) as f32;
        self.tangent_at(coordinate)
    }

    #[inline]
    fn tangent_at(&self, coordinate: f32) -> Result<V, Failure> {
        let tangent = if coordinate <= self.min_index as f32 {
            self.tangents[self.min_index]
        } else if coordinate >= self.max_index as f32 {
            self.tangents[self.max_index]
        } else {
            let i = coordinate as usize;
            let t = coordinate.fract();
            let a = self.tangents[i];
            let b = self.tangents[i + 1];
            spherical(a, b, t)?
        };
        if magnitude(tangent) < 0.99 {
            Err(Failure::Invalid)
        } else {
            Ok(tangent)
        }
    }

    /// Source distance parameters use full polyline length and evenly spaced
    /// control indices. They are not exact Hermite arc-length coordinates.
    pub fn point_linear(&self, distance: f32) -> Result<V, Failure> {
        if !distance.is_finite() {
            return Err(Failure::Invalid);
        }
        if self.length == 0. {
            return Ok(self.points[self.min_index]);
        }
        let coordinate = distance / self.length * (self.points.len() - 1) as f32;
        if coordinate <= self.min_index as f32 {
            return Ok(self.points[self.min_index]);
        }
        if coordinate >= self.max_index as f32 {
            return Ok(self.points[self.max_index]);
        }
        let index = coordinate as usize;
        let t = coordinate - index as f32;
        Ok(std::array::from_fn(|axis| {
            self.points[index][axis]
                + (self.points[index + 1][axis] - self.points[index][axis]) * t
        }))
    }

    /// Cubic Hermite interpolation uses the retained tangents and trim range.
    pub fn point(&self, distance: f32) -> Result<V, Failure> {
        if !distance.is_finite() {
            return Err(Failure::Invalid);
        }
        if self.length == 0. {
            return Ok(self.points[self.min_index]);
        }
        let coordinate = distance / self.length * (self.points.len() - 1) as f32;
        Ok(self.point_at(coordinate))
    }

    /// Sample both values for a surface sweep using the same control coordinate.
    pub(crate) fn surface_sample(&self, distance: f32) -> Result<(V, V), Failure> {
        if !distance.is_finite() || self.length <= 0. {
            return Err(Failure::Invalid);
        }
        let coordinate = distance / self.length * (self.points.len() - 1) as f32;
        Ok((self.point_at(coordinate), self.tangent_at(coordinate)?))
    }

    #[inline]
    fn point_at(&self, coordinate: f32) -> V {
        if coordinate <= self.min_index as f32 {
            return self.points[self.min_index];
        }
        if coordinate >= self.max_index as f32 {
            return self.points[self.max_index];
        }
        let i = coordinate as usize;
        let t = coordinate - i as f32;
        let t2 = t * t;
        let t3 = t * t2;
        let a = self.points[i];
        let b = self.points[i + 1];
        let ta = mul(self.tangents[i], self.step_size);
        let tb = mul(self.tangents[i + 1], self.step_size);
        add(
            add(
                add(mul(a, 2. * t3 - 3. * t2 + 1.), mul(ta, t3 - 2. * t2 + t)),
                mul(b, -2. * t3 + 3. * t2),
            ),
            mul(tb, t3 - t2),
        )
    }
    pub fn set_range(&mut self, min: usize, max: usize) -> Result<(), Failure> {
        if min >= max || max >= self.points.len() {
            return Err(Failure::Invalid);
        }
        self.min_index = min;
        self.max_index = max;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    #[test]
    fn exact_segment_boundary_samples_match_source_resample_and_remain_bounded() {
        let points = [[0., 0., 0.], [4., 0., 0.], [8., 0., 0.], [12., 0., 0.]];
        let settings = Settings {
            horizontal_iterations: 0,
            vertical_iterations: 0,
            resample_distance: 3.,
            maximum_work: 1000,
        };
        let path = build(&points, settings, None, || false).unwrap().unwrap();
        assert_eq!(path.points(), points);
        assert_eq!(path.length(), 12.);
        assert!(path.work() < 1000);
        assert!(matches!(
            build(
                &points,
                Settings {
                    maximum_work: 7,
                    ..settings
                },
                None,
                || false
            ),
            Err(Failure::Budget)
        ));
        let calls = std::cell::Cell::new(0);
        assert!(
            build(&points, settings, None, || {
                calls.set(calls.get() + 1);
                calls.get() > 5
            })
            .unwrap()
            .is_none()
        );
    }
    #[test]
    fn resample_matches_owned_method_float_bits_including_exact_boundaries() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/server-bd19c4ac-path-resample.json"
        ))
        .unwrap();
        assert_eq!(fixture["format"], "mapper-owned-path-resample-1");
        let rows = fixture["results"].as_array().unwrap();
        assert_eq!(rows.len(), 52);
        for (i, row) in rows.iter().enumerate() {
            let points: Vec<[f32; 3]> =
                serde_json::from_value(row["query"]["points"].clone()).unwrap();
            let expected: Vec<[i32; 3]> = serde_json::from_value(row["bits"].clone()).unwrap();
            let path = build(
                &points,
                Settings {
                    horizontal_iterations: 0,
                    vertical_iterations: 0,
                    resample_distance: row["query"]["spacing"].as_f64().unwrap() as f32,
                    maximum_work: 100_000,
                },
                None,
                || false,
            )
            .unwrap()
            .unwrap();
            let actual: Vec<_> = path
                .points()
                .iter()
                .map(|p| p.map(|v| v.to_bits() as i32))
                .collect();
            assert_eq!(actual, expected, "Owned resample case {i}");
        }
    }
    #[test]
    fn trimmed_offsets_match_owned_path_expression_order() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../compatibility/infrastructure/server-bd19c4ac-path-arithmetic.json"
        ))
        .unwrap();
        assert_eq!(fixture["format"], "mapper-owned-path-arithmetic-1");
        let mut count = 0;
        for result in fixture["results"].as_array().unwrap() {
            let query = &result["query"];
            if query["kind"] != "offsets" {
                continue;
            }
            let n = query["count"].as_u64().unwrap() as usize;
            let controls: Vec<_> = (0..n).map(|i| [i as f32, 0., 0.]).collect();
            let mut path = Path::from_controls(&controls, 10000, || false)
                .unwrap()
                .unwrap();
            // Isolate the inspected offset getters from length accumulation.
            path.length = serde_json::from_value(query["length"].clone()).unwrap();
            path.set_range(
                query["min"].as_u64().unwrap() as usize,
                query["max"].as_u64().unwrap() as usize,
            )
            .unwrap();
            let expected: [i32; 2] = serde_json::from_value(result["bits"].clone()).unwrap();
            let (start, end) = path.offsets();
            assert_eq!(
                [start.to_bits() as i32, end.to_bits() as i32],
                expected,
                "{query}"
            );
            count += 1;
        }
        assert_eq!(count, 32);
    }
    #[derive(Deserialize)]
    struct Fixture {
        reference: String,
        assembly_sha256: String,
        cases: Vec<Case>,
    }
    #[derive(Deserialize)]
    struct Case {
        input: Input,
        expected: Expected,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Input {
        name: String,
        points: Vec<V>,
        horizontal_iterations: u16,
        vertical_iterations: u16,
        resample_distance: f32,
        weights: Option<Vec<f32>>,
        trim: bool,
        #[serde(default)]
        refit: Option<Refit>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Refit {
        heights: Vec<f32>,
        iterations: u16,
        weights: Vec<f32>,
    }
    #[derive(Deserialize)]
    struct Expected {
        points: Vec<V>,
        tangents: Vec<V>,
        length: f32,
        step_size: f32,
        circular: bool,
        samples: Vec<Sample>,
    }
    #[derive(Deserialize)]
    struct Sample {
        fraction: f32,
        point: V,
    }
    fn settings() -> Settings {
        Settings {
            horizontal_iterations: 2,
            vertical_iterations: 3,
            resample_distance: 7.5,
            maximum_work: 1000000,
        }
    }
    #[test]
    fn near_antipodal_tangents_follow_the_unique_plane_at_constant_angular_speed() {
        fn angle(a: V, b: V) -> f64 {
            let a = a.map(f64::from);
            let b = b.map(f64::from);
            let cross = [
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            ];
            cross
                .iter()
                .map(|v| v * v)
                .sum::<f64>()
                .sqrt()
                .atan2(a.iter().zip(b).map(|(a, b)| a * b).sum())
        }
        for (start, end) in [
            (
                [-0.8078168, -0.07509443, -0.5846305],
                [0.81008756, 0.074800104, 0.58151793],
            ),
            ([1., 0., 0.], normal([-1., 0., 0.005])),
            (normal([1., 2., 3.]), normal([-1., -2.001, -3.])),
        ] {
            let full_angle = angle(start, end);
            for fraction in [0., 0.01, 0.036083, 0.25, 0.5, 0.75, 0.99, 1.] {
                let result = spherical(start, end, fraction).unwrap();
                assert!(result.iter().all(|v| v.is_finite()));
                assert!((magnitude(result) - 1.).abs() < 0.000001);
                assert!((angle(start, result) - full_angle * f64::from(fraction)).abs() < 0.000001);
                let reversed = spherical(end, start, 1. - fraction).unwrap();
                assert!(magnitude(sub(result, reversed)) < 0.000001);
            }
        }
        assert!(spherical([1., 0., 0.], [-1., 0., 0.], 0.5).is_err());
        assert!(spherical([0.; 3], [-1., 0., 0.], 0.5).is_err());
        assert!(spherical([1., 0., 0.], [0.; 3], 0.5).is_err());
    }
    #[test]
    fn sampled_tangents_follow_spherical_control_interpolation_and_trim_offsets() {
        let mut p = Path::from_controls(
            &[[0., 0., 0.], [10., 0., 0.], [10., 0., 10.], [20., 0., 10.]],
            1000,
            || false,
        )
        .unwrap()
        .unwrap();
        let t = p.tangent(p.length() / 6.).unwrap();
        let angle = 22.5_f32.to_radians();
        assert!((t[0] - angle.cos()).abs() < 1e-5);
        assert!((t[2] - angle.sin()).abs() < 1e-5);
        p.set_range(1, 2).unwrap();
        let (start, end) = p.offsets();
        assert!((start - p.length() / 3.).abs() < 1e-5);
        assert_eq!(start, end);
        assert_eq!(p.tangent(-100.).unwrap(), p.tangents()[1]);
        assert_eq!(p.tangent(10000.).unwrap(), p.tangents()[2]);
        assert!(p.tangent(f32::NAN).is_err());
        for i in -16..=80 {
            let distance = i as f32 * p.length() / 64.;
            let (point, tangent) = p.surface_sample(distance).unwrap();
            assert_eq!(
                point.map(f32::to_bits),
                p.point(distance).unwrap().map(f32::to_bits),
            );
            assert_eq!(
                tangent.map(f32::to_bits),
                p.tangent(distance).unwrap().map(f32::to_bits),
            );
        }
        assert!(p.surface_sample(f32::NAN).is_err());
        let mut q =
            Path::from_controls(&[[0., 0., 0.], [1., 0., 0.], [2., 0., 0.]], 1000, || false)
                .unwrap()
                .unwrap();
        q.tangents[1] = [-1., 0., 0.];
        assert!(q.tangent(q.length() / 4.).is_err());
        assert!(q.surface_sample(q.length() / 4.).is_err());
    }
    #[test]
    fn synthetic_curves_match_actual_installed_server_path_interpolator() {
        for raw in [
            include_str!("../tests/fixtures/path-interpolator-bd19c4ac.json"),
            include_str!("../tests/fixtures/path-refit-bd19c4ac.json"),
            include_str!("../tests/fixtures/path-refit-closure-bd19c4ac.json"),
        ] {
            let fixture: Fixture = serde_json::from_str(raw).unwrap();
            assert_eq!(fixture.reference, "installed-server-PathInterpolator");
            assert_eq!(fixture.assembly_sha256.len(), 64);
            for case in fixture.cases {
                let input = case.input;
                let expected = case.expected;
                let mut result = build(
                    &input.points,
                    Settings {
                        horizontal_iterations: input.horizontal_iterations,
                        vertical_iterations: input.vertical_iterations,
                        resample_distance: input.resample_distance,
                        ..settings()
                    },
                    input.weights.as_deref(),
                    || false,
                )
                .unwrap()
                .unwrap();
                if let Some(refit) = &input.refit {
                    // No-resampling constructor is exercised against these same
                    // source controls before the terrain fitting operation.
                    let controls = Path::from_controls(result.points(), 1000000, || false)
                        .unwrap()
                        .unwrap();
                    result = controls
                        .refit_heights(
                            &refit.heights,
                            refit.iterations,
                            &refit.weights,
                            1000000,
                            || false,
                        )
                        .unwrap()
                        .unwrap();
                }
                assert_eq!(
                    result.points.len(),
                    expected.points.len(),
                    "{} points",
                    input.name
                );
                assert_eq!(result.circular, expected.circular, "{} closure", input.name);
                let close = |a: f32, b: f32| {
                    assert!(
                        (a - b).abs() < 0.00005,
                        "{}: expected {b}, actual {a}",
                        input.name
                    )
                };
                close(result.length, expected.length);
                close(result.step_size, expected.step_size);
                for (a, b) in result
                    .points
                    .iter()
                    .flatten()
                    .zip(expected.points.iter().flatten())
                {
                    close(*a, *b);
                }
                for (a, b) in result
                    .tangents
                    .iter()
                    .flatten()
                    .zip(expected.tangents.iter().flatten())
                {
                    close(*a, *b);
                }
                if input.trim {
                    result.set_range(1, result.points.len() - 2).unwrap();
                }
                for sample in expected.samples {
                    let actual = result.point(sample.fraction * result.length).unwrap();
                    for (a, b) in actual.into_iter().zip(sample.point) {
                        close(a, b);
                    }
                }
            }
        }
    }
    #[test]
    fn deterministic_bounded_private_builds_preserve_inputs_on_cancellation_and_budget_failure() {
        let points = vec![[0., 0., 0.], [15., 8., 0.], [30., 4., 20.], [50., 1., 25.]];
        let before = points.clone();
        let a = build(&points, settings(), None, || false).unwrap().unwrap();
        let b = build(&points, settings(), None, || false).unwrap().unwrap();
        assert_eq!(
            serde_json::to_vec(&a).unwrap(),
            serde_json::to_vec(&b).unwrap()
        );
        assert!(matches!(
            build(
                &points,
                Settings {
                    maximum_work: 3,
                    ..settings()
                },
                None,
                || false
            ),
            Err(Failure::Budget)
        ));
        let calls = std::cell::Cell::new(0);
        assert!(
            build(&points, settings(), None, || {
                calls.set(calls.get() + 1);
                calls.get() > 4
            })
            .unwrap()
            .is_none()
        );
        assert_eq!(points, before);
        assert!(matches!(
            build(
                &[[0., 0., 0.], [10000., 0., 0.]],
                Settings {
                    resample_distance: 0.1,
                    ..settings()
                },
                None,
                || false
            ),
            Err(Failure::Budget)
        ));
    }
    #[test]
    fn refitting_is_transactional_bounded_and_keeps_control_indices() {
        let points = [[0., 0., 0.], [10., 0., 2.], [20., 0., 0.]];
        let path = Path::from_controls(&points, 1000, || false)
            .unwrap()
            .unwrap();
        let before = serde_json::to_vec(&path).unwrap();
        let heights = [2., 8., 3.];
        let fitted = path
            .refit_heights(&heights, 8, &[0., 1., 0.], 1000, || false)
            .unwrap()
            .unwrap();
        for (a, b) in fitted.points().iter().zip(points) {
            assert_eq!([a[0], a[2]], [b[0], b[2]]);
        }
        assert_eq!(fitted.points()[0][1], 2.);
        assert_eq!(fitted.points()[2][1], 3.);
        assert!(matches!(
            path.refit_heights(&heights, 8, &[1.; 3], 1, || false),
            Err(Failure::Budget)
        ));
        assert!(
            path.refit_heights(&heights, 8, &[1.; 3], 1000, || true)
                .unwrap()
                .is_none()
        );
        assert!(
            path.refit_heights(&[f32::NAN; 3], 0, &[1.; 3], 1000, || false)
                .is_err()
        );
        assert!(
            path.refit_heights(&heights, 0, &[1.; 2], 1000, || false)
                .is_err()
        );
        assert_eq!(serde_json::to_vec(&path).unwrap(), before);
    }
    #[test]
    fn straightening_keeps_endpoints_indices_trim_and_spherical_tangents() {
        let mut path = Path::from_controls(
            &[
                [-10., 0., -10.],
                [0., 2., 0.],
                [8., 5., 3.],
                [20., 8., 10.],
                [30., 12., 0.],
                [40., 20., 30.],
            ],
            1000,
            || false,
        )
        .unwrap()
        .unwrap();
        path.set_range(1, 4).unwrap();
        let original = serde_json::to_vec(&path).unwrap();
        let adjusted = path.straighten(&[(1, 4)], 1000, || false).unwrap().unwrap();
        assert_eq!(adjusted.range(), (1, 4));
        assert_eq!(adjusted.points.len(), path.points.len());
        for i in [0, 1, 4, 5] {
            assert_eq!(adjusted.points[i], path.points[i]);
            assert_eq!(adjusted.tangents[i], path.tangents[i]);
        }
        assert!((adjusted.points[2][0] - 10.).abs() < 0.00001);
        assert!((adjusted.points[2][1] - (2. + 10. / 3.)).abs() < 0.00001);
        assert_eq!(adjusted.points[2][2], 0.);
        assert!((adjusted.points[3][0] - 20.).abs() < 0.00001);
        for i in [2, 3] {
            let t = (i - 1) as f64 / 3.;
            let a = path.tangents[1].map(f64::from);
            let b = path.tangents[4].map(f64::from);
            let angle = a
                .iter()
                .zip(b)
                .map(|(a, b)| a * b)
                .sum::<f64>()
                .clamp(-1., 1.)
                .acos();
            let expected: [f64; 3] = std::array::from_fn(|axis| {
                (a[axis] * ((1. - t) * angle).sin() + b[axis] * (t * angle).sin()) / angle.sin()
            });
            for (actual, expected) in adjusted.tangents[i].iter().zip(expected) {
                assert!((f64::from(*actual) - expected).abs() < 0.00001);
            }
            assert!((magnitude(adjusted.tangents[i]) - 1.).abs() < 0.00001);
        }
        let length: f32 = adjusted
            .points
            .windows(2)
            .map(|p| magnitude(sub(p[1], p[0])))
            .sum();
        assert_eq!(adjusted.length, length);
        assert_eq!(adjusted.step_size, length / 6.);
        assert!(adjusted.length < path.length);
        assert_eq!(serde_json::to_vec(&path).unwrap(), original);
    }
    #[test]
    fn overlapping_straightening_follows_selected_order_and_preserves_closed_seam() {
        let path = Path::from_controls(
            &[
                [0., 0., 0.],
                [10., 0., 8.],
                [20., 0., -4.],
                [30., 0., 12.],
                [40., 0., 2.],
                [50., 0., -5.],
                [60., 0., 0.],
            ],
            1000,
            || false,
        )
        .unwrap()
        .unwrap();
        let spans = [(1, 4), (2, 5)];
        let adjusted = path.straighten(&spans, 1000, || false).unwrap().unwrap();
        let mut expected = path.points.clone();
        for (a, b) in spans {
            let start = expected[a];
            let end = expected[b];
            let inverse = 1. / (b - a) as f32;
            for (offset, p) in expected[a + 1..b].iter_mut().enumerate() {
                let t = (offset + 1) as f32 * inverse;
                *p = std::array::from_fn(|axis| start[axis] + (end[axis] - start[axis]) * t);
            }
        }
        assert_eq!(adjusted.points, expected);
        let reversed = path
            .straighten(&[(2, 5), (1, 4)], 1000, || false)
            .unwrap()
            .unwrap();
        assert_ne!(adjusted.points, reversed.points);
        let ring = Path::from_controls(
            &[
                [0., 0., 0.],
                [5., 0., 0.],
                [10., 0., 0.],
                [10., 0., 10.],
                [0., 0., 10.],
                [0., 0., 0.],
            ],
            1000,
            || false,
        )
        .unwrap()
        .unwrap();
        let adjusted = ring.straighten(&[(0, 2)], 1000, || false).unwrap().unwrap();
        assert!(adjusted.circular);
        assert_eq!(adjusted.points.first(), adjusted.points.last());
        assert_eq!(adjusted.tangents[0], ring.tangents[0]);
        assert_eq!(adjusted.tangents[5], ring.tangents[5]);
    }
    #[test]
    fn invalid_late_spans_tangents_work_and_cancellation_never_change_input() {
        let path = Path::from_controls(
            &[[0., 0., 0.], [10., 0., 2.], [20., 0., 4.], [30., 0., 0.]],
            1000,
            || false,
        )
        .unwrap()
        .unwrap();
        let original = serde_json::to_vec(&path).unwrap();
        for spans in [
            vec![(0, 2), (2, 9)],
            vec![(2, 1)],
            vec![(1, 1)],
            vec![(0, 1); 4097],
        ] {
            assert_eq!(
                path.straighten(&spans, 1000, || false).unwrap_err(),
                Failure::Invalid
            );
        }
        assert_eq!(
            path.straighten(&[(0, 3)], 9, || false).unwrap_err(),
            Failure::Budget
        );
        let calls = std::cell::Cell::new(0);
        assert!(
            path.straighten(&[(0, 3)], 1000, || {
                calls.set(calls.get() + 1);
                calls.get() > 3
            })
            .unwrap()
            .is_none()
        );
        let mut invalid = path.clone();
        invalid.tangents[3] = mul(invalid.tangents[0], -1.);
        assert_eq!(
            invalid.straighten(&[(0, 3)], 1000, || false).unwrap_err(),
            Failure::Invalid
        );
        invalid.tangents[3] = [0.; 3];
        assert_eq!(
            invalid.straighten(&[(0, 3)], 1000, || false).unwrap_err(),
            Failure::Invalid
        );
        assert_eq!(serde_json::to_vec(&path).unwrap(), original);
    }
    #[test]
    fn endpoint_reattachment_preserves_interior_controls_and_unaffected_tangents() {
        let path = Path::from_controls(
            &[
                [0., 0., 0.],
                [10., 0., 5.],
                [20., 0., 0.],
                [30., 0., 7.],
                [40., 0., 0.],
            ],
            1000,
            || false,
        )
        .unwrap()
        .unwrap();
        let original = serde_json::to_vec(&path).unwrap();
        let adjusted = path
            .reattach_endpoints([None, Some([42., 0., 10.])], 1000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(&adjusted.points[..4], &path.points[..4]);
        assert_eq!(&adjusted.tangents[..3], &path.tangents[..3]);
        assert_eq!(adjusted.points[4], [42., 0., 10.]);
        assert_eq!(adjusted.tangents[3], normal([22., 0., 10.]));
        assert_eq!(adjusted.tangents[4], normal([12., 0., 3.]));
        assert!(adjusted.length > path.length);
        assert_eq!(adjusted.step_size, adjusted.length / 5.);
        assert!(
            path.reattach_endpoints([None, Some(path.points[3])], 1000, || false)
                .is_err()
        );
        assert!(
            path.reattach_endpoints([Some([f32::NAN; 3]), None], 1000, || false)
                .is_err()
        );
        assert_eq!(
            path.reattach_endpoints([None, Some([42., 0., 10.])], 10, || false)
                .unwrap_err(),
            Failure::Budget
        );
        let calls = std::cell::Cell::new(0);
        assert!(
            path.reattach_endpoints([None, Some([42., 0., 10.])], 1000, || {
                calls.set(calls.get() + 1);
                calls.get() > 3
            })
            .unwrap()
            .is_none()
        );
        assert_eq!(serde_json::to_vec(&path).unwrap(), original);
        let two = Path::from_controls(&[[0., 0., 0.], [10., 0., 0.]], 1000, || false)
            .unwrap()
            .unwrap();
        assert_eq!(
            two.reattach_endpoints([Some([10., 0., 0.]), Some([20., 0., 0.])], 1000, || false)
                .unwrap()
                .unwrap()
                .points,
            vec![[10., 0., 0.], [20., 0., 0.]]
        );
    }
    #[test]
    fn malformed_geometry_and_sampling_ranges_are_rejected() {
        let points = [[0., 0., 0.], [100., 0., 0.]];
        for invalid in [
            vec![],
            vec![[0., 0., 0.]],
            vec![[f32::NAN, 0., 0.], [1., 0., 0.]],
        ] {
            assert!(matches!(
                build(&invalid, settings(), None, || false),
                Err(Failure::Invalid)
            ));
        }
        assert!(matches!(
            build(&points, settings(), Some(&[1.]), || false),
            Err(Failure::Invalid)
        ));
        assert!(matches!(
            build(
                &points,
                Settings {
                    resample_distance: f32::INFINITY,
                    ..settings()
                },
                None,
                || false
            ),
            Err(Failure::Invalid)
        ));
        let mut path = build(&points, settings(), None, || false).unwrap().unwrap();
        assert!(path.point(f32::NAN).is_err());
        let range = path.range();
        assert!(path.set_range(2, 1).is_err());
        assert_eq!(path.range(), range);
        assert_eq!(path.point(-100.).unwrap(), points[0]);
        assert_eq!(path.point(1000.).unwrap(), points[1]);
    }
}
