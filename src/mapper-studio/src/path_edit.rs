use papyrus_terrain::{monument_composition::Composed, road_network};
use serde::{Deserialize, Serialize};

const MAX_CHANGES: usize = 4096;
const MAX_ROUTES: usize = 4353;
const MAX_CONTROLS: usize = 262144 + 131072;
const MAX_BEND_SIDE: usize = 32;
const BEND_RADIUS: f32 = 65.;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Road,
    Trail,
    Rail,
}
impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Road => "Road",
            Self::Trail => "Trail",
            Self::Rail => "Rail",
        }
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Key {
    pub kind: Kind,
    pub index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Operation {
    Move([f32; 2]),
    Insert([f32; 2]),
    Remove,
    Bend {
        position: [f32; 2],
        first: usize,
        last: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub route: Key,
    pub control: usize,
    pub expected: [f32; 2],
    pub operation: Operation,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub key: Key,
    pub controls: Vec<[f32; 2]>,
    pub fixed: Vec<usize>,
}

impl Change {
    /// Zero displacement and zero slope at the two unmodified boundary points.
    pub fn bend_weight(&self, index: usize) -> f32 {
        let Operation::Bend { first, last, .. } = self.operation else {
            return if index == self.control { 1. } else { 0. };
        };
        if index <= first || index >= last {
            return 0.;
        }
        let t = if index <= self.control {
            (index - first) as f32 / (self.control - first) as f32
        } else {
            (last - index) as f32 / (last - self.control) as f32
        };
        t * t * (3. - 2. * t)
    }
}

pub fn validate(changes: &[Change], size: u32) -> Result<(), String> {
    if changes.len() > MAX_CHANGES || changes.iter().any(|change| !valid_change(change, size)) {
        return Err("Invalid spline changes or map bounds".into());
    }
    Ok(())
}

/// Validate a new edit when the existing history has already passed validation.
pub fn validate_append(len: usize, change: &Change, size: u32) -> Result<(), String> {
    if len >= MAX_CHANGES || !valid_change(change, size) {
        return Err("Invalid spline changes or map bounds".into());
    }
    Ok(())
}

fn valid_change(change: &Change, size: u32) -> bool {
    change.route.index < MAX_ROUTES
        && change.control < 65536
        && valid_point(change.expected, size)
        && match change.operation {
            Operation::Move(point) | Operation::Insert(point) => valid_point(point, size),
            Operation::Remove => true,
            Operation::Bend {
                position,
                first,
                last,
            } => {
                first < change.control
                    && change.control < last
                    && last < 65536
                    && change.control - first <= MAX_BEND_SIDE
                    && last - change.control <= MAX_BEND_SIDE
                    && valid_point(position, size)
            }
        }
}

fn valid_point(point: [f32; 2], size: u32) -> bool {
    let half = size as f32 * 0.5;
    point.into_iter().all(|axis| (-half..=half).contains(&axis))
}

pub fn validate_routes(routes: &[Route], size: u32) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    if size == 0
        || routes.len() > MAX_ROUTES
        || routes.iter().map(|r| r.controls.len()).sum::<usize>() > MAX_CONTROLS
        || routes.iter().any(|r| {
            r.key.index >= MAX_ROUTES
                || !seen.insert(r.key)
                || !(2..=65536).contains(&r.controls.len())
                || r.controls
                    .iter()
                    .flatten()
                    .any(|v| !v.is_finite() || v.abs() > 10000.)
                || r.fixed.iter().any(|&i| i >= r.controls.len())
                || r.fixed.first() != Some(&0)
                || r.fixed.last() != Some(&(r.controls.len() - 1))
                || r.fixed.windows(2).any(|v| v[0] >= v[1])
        })
    {
        return Err("Invalid map spline snapshot".into());
    }
    Ok(())
}

fn xz(p: [f32; 3]) -> [f32; 2] {
    [p[0], p[2]]
}

/// Return the segments whose endpoints or tangent-defining neighbors changed.
/// Exact coordinate keys keep unaffected spans stable across inserts and removals.
pub fn changed_segments(base: &[[f32; 2]], revised: &[[f32; 2]]) -> Vec<std::ops::Range<usize>> {
    fn segments(controls: &[[f32; 2]]) -> impl Iterator<Item = [[u32; 2]; 4]> + '_ {
        let last = controls.len().saturating_sub(1);
        let circular = controls.first() == controls.last();
        (0..last).map(move |i| {
            let before = if i == 0 && circular { last - 1 } else { i.saturating_sub(1) };
            let after = if i + 1 == last && circular { 1 } else { (i + 2).min(last) };
            [before, i, i + 1, after].map(|index| controls[index].map(f32::to_bits))
        })
    }
    let original: std::collections::HashSet<_> = segments(base).collect();
    let mut spans: Vec<std::ops::Range<usize>> = Vec::new();
    for (i, segment) in segments(revised).enumerate() {
        if !original.contains(&segment) {
            if let Some(span) = spans.last_mut().filter(|span| span.end == i) {
                span.end += 1;
            } else {
                spans.push(i..i + 1);
            }
        }
    }
    spans
}

impl Route {
    fn from_points(key: Key, points: &[[f32; 3]], mut fixed: Vec<usize>) -> Self {
        fixed.extend([0, points.len() - 1]);
        fixed.sort_unstable();
        fixed.dedup();
        Self {
            key,
            controls: points.iter().copied().map(xz).collect(),
            fixed,
        }
    }

    pub fn is_fixed(&self, i: usize) -> bool {
        self.fixed.binary_search(&i).is_ok()
    }

    fn bend_boundary(&self, control: usize, step: isize, radius: f32) -> usize {
        let mut current = control;
        let mut length = 0.;
        for _ in 0..MAX_BEND_SIDE {
            let Some(next) = current
                .checked_add_signed(step)
                .filter(|&i| i < self.controls.len())
            else {
                break;
            };
            let a = self.controls[current];
            let b = self.controls[next];
            length += (b[0] - a[0]).hypot(b[1] - a[1]);
            current = next;
            if length >= radius || self.is_fixed(current) {
                break;
            }
        }
        current
    }

    /// Keep the sampled geometry and move a short span around the chosen handle.
    pub fn drag_change(&self, control: usize, position: [f32; 2]) -> Change {
        let operation = if self.is_fixed(control) {
            Operation::Move(position)
        } else {
            let origin = self.controls[control];
            let displacement = (position[0] - origin[0]).hypot(position[1] - origin[1]);
            // Give a longer pull more room to blend back into the original path.
            let radius = BEND_RADIUS.max(displacement * 3.);
            let first = self.bend_boundary(control, -1, radius);
            let last = self.bend_boundary(control, 1, radius);
            if last - first == 2 {
                Operation::Move(position)
            } else {
                Operation::Bend {
                    position,
                    first,
                    last,
                }
            }
        };
        Change {
            route: self.key,
            control,
            expected: self.controls[control],
            operation,
        }
    }

    pub fn apply(&mut self, change: Change) -> Result<(), String> {
        self.check(change)?;
        let i = change.control;
        match change.operation {
            Operation::Move(point) => self.controls[i] = point,
            Operation::Bend {
                position,
                first,
                last,
            } => {
                let origin = self.controls[i];
                for (j, point) in self
                    .controls
                    .iter_mut()
                    .enumerate()
                    .take(last)
                    .skip(first + 1)
                {
                    let weight = change.bend_weight(j);
                    for axis in 0..2 {
                        point[axis] += (position[axis] - origin[axis]) * weight;
                    }
                }
                self.controls[i] = position;
            }
            Operation::Insert(point) => self.controls.insert(i + 1, point),
            Operation::Remove => {
                self.controls.remove(i);
            }
        }
        for index in &mut self.fixed {
            *index = remap(*index, change);
        }
        Ok(())
    }

    fn check(&self, change: Change) -> Result<(), String> {
        let i = change.control;
        if change.route != self.key
            || self.controls.get(i).is_none_or(|p| {
                (p[0] - change.expected[0]).abs() > 0.01 || (p[1] - change.expected[1]).abs() > 0.01
            })
        {
            return Err(format!(
                "{} {} changed during generation. Discard the draft and select the current path.",
                self.key.kind.label(),
                self.key.index + 1
            ));
        }
        match change.operation {
            Operation::Move(_) | Operation::Remove if self.is_fixed(i) => {
                Err("This spline point is attached to a connection".into())
            }
            Operation::Bend { first, last, .. }
                if first >= i
                    || last <= i
                    || last >= self.controls.len()
                    || i - first > MAX_BEND_SIDE
                    || last - i > MAX_BEND_SIDE
                    || self
                        .fixed
                        .iter()
                        .any(|&fixed| first < fixed && fixed < last) =>
            {
                Err("A spline bend cannot move an attached point".into())
            }
            Operation::Insert(_)
                if i + 1 == self.controls.len() || self.controls.len() == 65536 =>
            {
                Err("Choose a segment to insert a spline point".into())
            }
            Operation::Remove
                if self.controls.len()
                    <= if self.controls.first() == self.controls.last() {
                        4
                    } else {
                        2
                    } =>
            {
                Err("This spline needs more control points".into())
            }
            _ => Ok(()),
        }
    }
}

fn remap(index: usize, change: Change) -> usize {
    if index <= change.control {
        return index;
    }
    match change.operation {
        Operation::Insert(_) => index + 1,
        Operation::Remove => index - 1,
        Operation::Move(_) | Operation::Bend { .. } => index,
    }
}

fn apply_3d(route: &mut Route, points: &mut Vec<[f32; 3]>, change: Change) -> Result<(), String> {
    route.apply(change)?;
    let i = change.control;
    match change.operation {
        Operation::Move([x, z]) => points[i] = [x, points[i][1], z],
        Operation::Bend { first, last, .. } => {
            for (point, &[x, z]) in points[first + 1..last]
                .iter_mut()
                .zip(&route.controls[first + 1..last])
            {
                *point = [x, point[1], z];
            }
        }
        Operation::Insert([x, z]) => {
            let y = (points[i][1] + points[i + 1][1]) * 0.5;
            points.insert(i + 1, [x, y, z]);
        }
        Operation::Remove => {
            points.remove(i);
        }
    }
    Ok(())
}

fn road_keys(report: &road_network::Report) -> Vec<Option<Key>> {
    let mut keys = vec![None; report.roads.len()];
    for family in &report.families {
        let kind = if family.hierarchy == 2 {
            Kind::Trail
        } else {
            Kind::Road
        };
        for i in family.first_road
            ..family
                .first_road
                .saturating_add(family.road_count)
                .min(keys.len())
        {
            keys[i] = Some(Key { kind, index: i });
        }
    }
    keys
}

fn road_connections(report: &road_network::Report) -> Vec<Vec<usize>> {
    use road_network::Destination;
    let mut fixed = vec![Vec::new(); report.roads.len()];
    for road in &report.roads {
        for end in [road.from, road.to] {
            let (parent, point) = match end {
                Destination::Network { road, point } => (road, point),
                Destination::RawNetwork(raw) => (raw.parent_road(), raw.reference_parent_point()),
                _ => continue,
            };
            if let Some(points) = fixed.get_mut(parent) {
                points.push(point);
            }
        }
    }
    fixed
}

pub fn capture(project: &crate::Project, maps: &Composed) -> Vec<Route> {
    let mut routes = Vec::new();
    if let Some(report) = &maps.roads
        && project
            .roads
            .as_ref()
            .and_then(|r| r.staged.as_ref())
            .is_some_and(|r| crate::road_generation::is_source_engine(&r.engine))
    {
        for ((road, key), fixed) in report
            .roads
            .iter()
            .zip(road_keys(report))
            .zip(road_connections(report))
        {
            if let Some(key) = key {
                routes.push(Route::from_points(key, &road.points, fixed));
            }
        }
    }
    if let Some(rails) = &maps.rails {
        routes.extend(rails.tracks.iter().map(|track| {
            Route::from_points(
                Key {
                    kind: Kind::Rail,
                    index: track.id as usize - 1,
                },
                track.path.points(),
                Vec::new(),
            )
        }));
    }
    routes
}

#[derive(Default)]
pub(crate) struct RoadEdits {
    pub(crate) points: std::collections::BTreeMap<usize, Vec<[f32; 3]>>,
    index_changes: std::collections::HashMap<usize, Vec<Change>>,
}

impl RoadEdits {
    pub(crate) fn commit(self, report: &mut road_network::Report) {
        use road_network::Destination;
        for (index, points) in self.points {
            report.roads[index].points = points;
        }
        if self.index_changes.is_empty() {
            return;
        }
        for road in &mut report.roads {
            for end in [&mut road.from, &mut road.to] {
                let (parent, point) = match *end {
                    Destination::Network { road, point } => (road, point),
                    Destination::RawNetwork(raw) => (raw.parent_road(), raw.reference_parent_point()),
                    _ => continue,
                };
                let Some(changes) = self.index_changes.get(&parent) else { continue; };
                let point = changes.iter().fold(point, |point, &change| remap(point, change));
                *end = match *end {
                    Destination::RawNetwork(raw) => Destination::RawNetwork(raw.with_reference_parent_point(point)),
                    _ => Destination::Network { road: parent, point },
                };
            }
        }
    }
}

pub(crate) fn prepare_road_edits(
    report: &road_network::Report,
    changes: &[Change],
    cancelled: &impl Fn() -> bool,
) -> Result<Option<RoadEdits>, String> {
    let keys = road_keys(report);
    let connections = road_connections(report);
    let mut routes = std::collections::HashMap::new();
    let mut edits = RoadEdits::default();
    for &change in changes.iter().filter(|c| c.route.kind != Kind::Rail) {
        if cancelled() {
            return Ok(None);
        }
        let index = change.route.index;
        if keys.get(index).copied().flatten() != Some(change.route) {
            return Err("The edited road or trail is missing from the new layout".into());
        }
        let points = edits.points.entry(index).or_insert_with(|| report.roads[index].points.clone());
        let route = routes.entry(index).or_insert_with(||
            Route::from_points(change.route, points, connections[index].clone()));
        apply_3d(route, points, change)?;
        if matches!(change.operation, Operation::Insert(_) | Operation::Remove) {
            edits.index_changes.entry(index).or_default().push(change);
        }
    }
    Ok(Some(edits))
}

/// The downstream road pipeline may straighten a roadside monument span.
/// Reject publication if that would replace any of the requested positions.
pub(crate) fn verify_road_edit(
    report: &road_network::Report,
    changes: &[Change],
) -> Result<(), String> {
    verify_edits(
        changes.iter().filter(|c| c.route.kind != Kind::Rail),
        |key| {
            report
                .roads
                .get(key.index)
                .map(|road| road.points.as_slice())
        },
    )
}

pub(crate) fn verify_rail_edit(
    tracks: &[papyrus_terrain::rail_bed::FittedTrack],
    changes: &[Change],
) -> Result<(), String> {
    verify_edits(
        changes.iter().filter(|c| c.route.kind == Kind::Rail),
        |key| {
            tracks
                .get(key.index)
                .filter(|track| track.id as usize == key.index + 1)
                .map(|track| track.path.points())
        },
    )
}

fn verify_edits<'a>(
    changes: impl DoubleEndedIterator<Item = &'a Change>,
    points: impl Fn(Key) -> Option<&'a [[f32; 3]]>,
) -> Result<(), String> {
    use std::collections::HashMap;
    let mut current = HashMap::<Key, Vec<[f32; 2]>>::new();
    for &change in changes.rev() {
        let points = match current.entry(change.route) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => entry.insert(
                points(change.route)
                    .ok_or_else(|| {
                        format!(
                            "The edited {} is missing from the fitted layout",
                            change.route.kind.label().to_lowercase()
                        )
                    })?
                    .iter()
                    .copied()
                    .map(xz)
                    .collect(),
            ),
        };
        let i = change.control;
        let matches = |i, expected: [f32; 2]| {
            points.get(i).is_some_and(|p: &[f32; 2]| {
                (p[0] - expected[0]).abs() <= 0.01 && (p[1] - expected[1]).abs() <= 0.01
            })
        };
        match change.operation {
            Operation::Move(position) if matches(i, position) => points[i] = change.expected,
            Operation::Bend {
                position,
                first,
                last,
            } if first < i && i < last && last < points.len() && matches(i, position) => {
                for (index, p) in points.iter_mut().enumerate().take(last).skip(first + 1) {
                    let weight = change.bend_weight(index);
                    for axis in 0..2 {
                        p[axis] -= (position[axis] - change.expected[axis]) * weight;
                    }
                }
                points[i] = change.expected;
            }
            Operation::Insert(position) if matches(i + 1, position) => {
                points.remove(i + 1);
            }
            Operation::Remove if i <= points.len() && !matches(i, change.expected) => {
                points.insert(i, change.expected);
            }
            _ => {
                return Err(format!(
                    "{} fitting moved an edited point. Adjust the draft away from connections.",
                    change.route.kind.label()
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn edit_rail(
    index: usize,
    path: papyrus_terrain::path_geometry::Path,
    changes: &[Change],
    cancelled: &impl Fn() -> bool,
) -> Result<Option<papyrus_terrain::path_geometry::Path>, String> {
    let key = Key {
        kind: Kind::Rail,
        index,
    };
    let mut edits = changes
        .iter()
        .copied()
        .filter(|c| c.route == key)
        .peekable();
    if edits.peek().is_none() {
        return Ok(Some(path));
    }
    let mut points = path.points().to_vec();
    let mut route = Route::from_points(key, &points, Vec::new());
    for change in edits {
        if cancelled() {
            return Ok(None);
        }
        apply_3d(&mut route, &mut points, change)?;
    }
    papyrus_terrain::path_geometry::Path::from_controls(&points, 32_000_000, cancelled)
        .map_err(|error| format!("Invalid edited rail spline: {error:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use papyrus_terrain::road_network::{Destination, Family, Report, Road};

    #[test]
    fn larger_drags_blend_across_more_of_the_path_without_moving_anchors() {
        let route = Route {
            key: Key { kind: Kind::Road, index: 0 },
            controls: (0..=80).map(|i| [i as f32 * 10., 0.]).collect(),
            fixed: vec![0, 30, 80],
        };
        assert!(matches!(route.drag_change(40, [400., 10.]).operation,
            Operation::Bend { first: 33, last: 47, .. }));
        assert!(matches!(route.drag_change(40, [400., 50.]).operation,
            Operation::Bend { first: 30, last: 55, .. }));
        assert!(matches!(route.drag_change(40, [400., 500.]).operation,
            Operation::Bend { first: 30, last: 72, .. }));
    }

    #[test]
    fn fitted_edits_reject_the_old_rail_or_a_missing_track() {
        let key = Key {
            kind: Kind::Rail,
            index: 0,
        };
        let points = [[-25., 12., 0.], [0., 12., 0.], [25., 12., 0.]];
        let change = Change {
            route: key,
            control: 1,
            expected: [0., 0.],
            operation: Operation::Move([0., 10.]),
        };
        let mut fitted = points;
        assert!(verify_edits([&change].into_iter(), |_| None).is_err());
        assert!(verify_edits([&change].into_iter(), |_| Some(&points)).is_err());
        fitted[1][2] = 10.;
        verify_edits([&change].into_iter(), |_| Some(&fitted)).unwrap();
    }

    #[test]
    fn inserts_and_removes_controls_without_repointing_attachment_geometry() {
        let points = vec![[0., 5., 0.], [10., 5., 0.], [20., 5., 0.], [30., 5., 0.]];
        let key = Key {
            kind: Kind::Road,
            index: 0,
        };
        let child_attachment = Destination::Network { road: 0, point: 2 };
        let mut report = Report {
            roads: vec![
                Road {
                    from: Destination::Loop,
                    to: Destination::Loop,
                    points,
                },
                Road {
                    from: child_attachment,
                    to: Destination::Loop,
                    points: vec![[20., 5., 0.], [20., 5., 10.]],
                },
            ],
            families: vec![Family {
                infrastructure_type: 1,
                hierarchy: 0,
                seed: 1,
                first_road: 0,
                road_count: 2,
                socket_count: 0,
                unresolved: Vec::new(),
                width: 10.,
                splat_channel: 1,
            }],
            ..Default::default()
        };
        let insert = Change {
            route: key,
            control: 1,
            expected: [10., 0.],
            operation: Operation::Insert([14., 2.]),
        };
        let later_move = Change {
            route: key,
            control: 2,
            expected: [14., 2.],
            operation: Operation::Move([15., 2.]),
        };
        let remove = Change {
            route: key,
            control: 1,
            expected: [10., 0.],
            operation: Operation::Remove,
        };
        let edits = [insert, later_move, remove];
        let prepared = prepare_road_edits(&report, &edits, &|| false).unwrap().unwrap();
        assert_eq!(prepared.points.keys().copied().collect::<Vec<_>>(), [0]);
        assert_eq!(report.roads[0].points[1], [10., 5., 0.]);
        let untouched = report.roads[1].points.as_ptr();
        prepared.commit(&mut report);
        assert_eq!(report.roads[1].points.as_ptr(), untouched);
        assert_eq!(report.roads[1].from, child_attachment);
        assert_eq!(report.roads[0].points[2], [20., 5., 0.]);
        assert_eq!(report.roads[0].points[1], [15., 5., 2.]);
        verify_road_edit(&report, &edits).unwrap();
        let attached = Change {
            route: key,
            control: 2,
            expected: [20., 0.],
            operation: Operation::Remove,
        };
        assert!(prepare_road_edits(&report, &[attached], &|| false).is_err());
        let displaced = Change {
            route: key,
            control: 1,
            expected: [16., 2.],
            operation: Operation::Remove,
        };
        assert!(prepare_road_edits(&report, &[displaced], &|| false).is_err());
        report.roads[0].points[1][0] += 1.;
        assert!(verify_road_edit(&report, &edits).is_err());
    }
}
