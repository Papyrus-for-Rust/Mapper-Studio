//! Bake a conservative, terrain-based offshore circuit for the server plugin.
use std::fmt::Write;

pub const LAYER: &str = "papyrus.cargo-route";
const CELL: f32 = 16.;
const CLEARANCE: f32 = 200.;

pub fn bake(
    size: u32,
    resolution: usize,
    height: &[i16],
    cancelled: impl Fn() -> bool,
) -> Result<Option<Vec<u8>>, String> {
    if !(1000..=6000).contains(&size)
        || !(33..=4097).contains(&resolution)
        || height.len() != resolution * resolution
    {
        return Err("Invalid terrain for the cargo ship route".into());
    }
    let size_m = size as f32;
    let axis = (size_m / CELL).ceil() as usize;
    let padding = (CLEARANCE / CELL).ceil() as usize + 3;
    let side = axis + 2 * padding;
    let mut distance = vec![u16::MAX; side * side];
    // Track the shallowest point in every grid cell. The resulting chessboard
    // distance deliberately overestimates shoreline hazards along diagonals.
    let scale = size_m / (resolution - 1) as f32 / CELL;
    let sample = |i: usize| ((i as f32 * scale) as usize).min(axis - 1) + padding;
    for (z, row) in height.chunks_exact(resolution).enumerate() {
        if cancelled() {
            return Ok(None);
        }
        let line = sample(z) * side;
        for (x, &h) in row.iter().enumerate() {
            if h > 16_120 {
                distance[line + sample(x)] = 0;
            }
        }
    }
    for z in 1..side - 1 {
        for x in 1..side - 1 {
            let i = z * side + x;
            let neighbor = distance[i - 1]
                .min(distance[i - side - 1])
                .min(distance[i - side])
                .min(distance[i - side + 1]);
            distance[i] = distance[i].min(neighbor.saturating_add(1));
        }
    }
    for z in (1..side - 1).rev() {
        for x in (1..side - 1).rev() {
            let i = z * side + x;
            let neighbor = distance[i + 1]
                .min(distance[i + side - 1])
                .min(distance[i + side])
                .min(distance[i + side + 1]);
            distance[i] = distance[i].min(neighbor.saturating_add(1));
        }
    }
    let clear = |x: f32, z: f32| {
        let index = |p: f32| ((p + size_m * 0.5) / CELL).floor() as isize + padding as isize;
        let (x, z) = (index(x), index(z));
        x < 0
            || z < 0
            || x >= side as isize
            || z >= side as isize
            || distance[z as usize * side + x as usize] as usize >= padding - 1
    };
    let count = (size_m * std::f32::consts::TAU / 30.).ceil() as usize;
    let directions = (0..count)
        .map(|i| (i as f32 / count as f32 * std::f32::consts::TAU).sin_cos())
        .collect::<Vec<_>>();
    let mut radii = vec![size_m; count];
    for (radius, &(x, z)) in radii.iter_mut().zip(&directions) {
        if cancelled() {
            return Ok(None);
        }
        while *radius > size_m * 0.18 && clear(x * (*radius - 8.), z * (*radius - 8.)) {
            *radius -= 8.;
        }
    }
    // Only move nodes outwards to smooth the circuit. Verify the links too.
    for _ in 0..count {
        let mut changed = false;
        for i in 0..count {
            let previous = (i + count - 1) % count;
            let next = (i + 1) % count;
            let minimum = radii[previous].max(radii[next]) - 48.;
            if radii[i] < minimum {
                radii[i] = minimum;
                changed = true;
            }
        }
        if cancelled() {
            return Ok(None);
        }
        if !changed {
            break;
        }
    }
    let mut points: Vec<_> = directions
        .iter()
        .zip(radii)
        .map(|(&(x, z), radius)| [x * radius, z * radius])
        .collect();
    if points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .any(|(a, b)| {
            (0..=8).any(|step| {
                let mix = step as f32 / 8.;
                !clear(a[0] + (b[0] - a[0]) * mix, a[1] + (b[1] - a[1]) * mix)
            })
        })
    {
        // The source starts with this fully offshore ring. It encloses
        // the whole square with clearance at every supported map size.
        for (point, &(x, z)) in points.iter_mut().zip(&directions) {
            *point = [x * size_m, z * size_m];
        }
    }
    let mut xml = format!(
        "<SerializedPathList mapSize=\"{size}\" shoreDistance=\"200\" waterDepth=\"8\"><vectorData>"
    );
    for [x, z] in points {
        write!(xml, "<VectorData><x>{x}</x><y>0</y><z>{z}</z></VectorData>")
            .expect("writing to a String cannot fail");
    }
    xml.push_str("</vectorData></SerializedPathList>");
    Ok((!cancelled()).then(|| xml.into_bytes()))
}
