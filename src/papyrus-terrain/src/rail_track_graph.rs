//! Deterministic geometric track graph with source-bound node tolerances.
//! Splitting/ordering are independent of Unity object enumeration. This is not
//! runtime collation, switch selection, collider or train traversal validation.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
pub const VERSION: &str = "papyrus-track-endpoint-graph-1";
type V = [f32; 3];
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub engine: StringTag,
    pub maximum_distance_squared: f32,
    pub maximum_angle_degrees: f32,
}
// A typed token keeps the serializable policy Copy and rejects guessed engines.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub enum StringTag {
    #[serde(rename = "papyrus-track-endpoint-graph-1")]
    V1,
}
impl Policy {
    pub fn validate(self) -> Result<(), String> {
        if !self.maximum_distance_squared.is_finite()
            || !(0.000001..=1.).contains(&self.maximum_distance_squared)
            || !self.maximum_angle_degrees.is_finite()
            || !(0.1..=45.).contains(&self.maximum_angle_degrees)
        {
            return Err("Invalid track connection policy".into());
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub maximum_work: usize,
}
impl Settings {
    pub fn validate(self) -> Result<(), String> {
        if !(1..=32000000).contains(&self.maximum_work) {
            return Err("Invalid track graph work allowance".into());
        }
        Ok(())
    }
}
#[derive(Serialize)]
pub struct Track {
    pub id: u64,
    pub generated: bool,
    pub points: Vec<V>,
    pub tangents: Vec<V>,
}
#[derive(Debug, Serialize)]
pub struct Span {
    pub id: usize,
    pub track_id: u64,
    pub first_control: usize,
    pub last_control: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Endpoint {
    pub span: usize,
    pub end: bool,
}
#[derive(Debug, Serialize)]
pub struct Connection {
    pub a: Endpoint,
    pub b: Endpoint,
    pub reverse: bool,
    pub gap: f32,
    pub angle_degrees: f32,
}
#[derive(Debug, Serialize)]
pub struct Graph {
    pub engine: &'static str,
    pub work: usize,
    pub split_controls: usize,
    pub spans: Vec<Span>,
    pub connections: Vec<Connection>,
    pub components: Vec<Vec<usize>>,
    pub terminal_endpoints: Vec<Endpoint>,
}
fn magnitude(v: V) -> f32 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}
/// Endpoint orientation is handled by the caller. Zero directions are rejected
/// before graph construction rather than treated as usable rail geometry.
pub fn matches(a: V, b: V, ta: V, tb: V, policy: Policy) -> Option<(f32, f32)> {
    if policy.validate().is_err()
        || a.iter()
            .chain(&b)
            .chain(&ta)
            .chain(&tb)
            .any(|v| !v.is_finite() || v.abs() > 10000.)
    {
        return None;
    }
    let d: V = std::array::from_fn(|i| a[i] - b[i]);
    let sq = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
    if sq >= policy.maximum_distance_squared {
        return None;
    }
    let squared = |v: V| v[0] * v[0] + v[1] * v[1] + v[2] * v[2];
    let denom = (squared(ta) * squared(tb)).sqrt();
    if denom <= 0.000000000000001 {
        return None;
    }
    let dot = (ta[0] * tb[0] + ta[1] * tb[1] + ta[2] * tb[2]) / denom;
    let angle = dot.clamp(-1., 1.).acos().to_degrees();
    (angle < policy.maximum_angle_degrees).then_some((sq.sqrt(), angle))
}
fn root(p: &mut [usize], mut i: usize) -> usize {
    while p[i] != i {
        p[i] = p[p[i]];
        i = p[i];
    }
    i
}
pub fn build(
    tracks: &[Track],
    policy: Policy,
    settings: Settings,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Graph>, String> {
    policy.validate()?;
    settings.validate()?;
    let mut ids = BTreeSet::new();
    let mut count = 0;
    if tracks.is_empty() || tracks.len() > 8192 {
        return Err("Invalid track graph roster".into());
    }
    for t in tracks {
        count += t.points.len();
        if t.id == 0
            || !ids.insert(t.id)
            || !(2..=65536).contains(&t.points.len())
            || t.points.len() != t.tangents.len()
            || count > 131072
            || t.points
                .iter()
                .chain(&t.tangents)
                .flatten()
                .any(|v| !v.is_finite() || v.abs() > 10000.)
            || t.tangents.iter().any(|&v| magnitude(v) <= 0.00001)
        {
            return Err("Invalid track graph controls or directions".into());
        }
    }
    let mut work = 0;
    let mut visit = || -> Result<bool, String> {
        if cancelled() {
            return Ok(false);
        }
        if work >= settings.maximum_work {
            return Err("Track graph work allowance reached".into());
        }
        work += 1;
        Ok(true)
    };
    let mut ordered: Vec<_> = tracks.iter().collect();
    ordered.sort_by_key(|t| t.id);
    let mut spans = Vec::new();
    let mut split_controls = 0;
    for track in &ordered {
        let mut cuts = vec![0];
        if track.generated {
            for i in 1..track.points.len() - 1 {
                let mut split = false;
                for other in &ordered {
                    if other.id == track.id {
                        continue;
                    }
                    for j in [0, other.points.len() - 1] {
                        if !visit()? {
                            return Ok(None);
                        }
                        let tangent = other.tangents[j];
                        if matches(
                            track.points[i],
                            other.points[j],
                            track.tangents[i],
                            tangent,
                            policy,
                        )
                        .is_some()
                            || matches(
                                track.points[i],
                                other.points[j],
                                track.tangents[i],
                                tangent.map(|v| -v),
                                policy,
                            )
                            .is_some()
                        {
                            split = true;
                        }
                    }
                }
                if split {
                    cuts.push(i);
                    split_controls += 1;
                }
            }
        }
        cuts.push(track.points.len() - 1);
        for c in cuts.windows(2) {
            spans.push(Span {
                id: spans.len(),
                track_id: track.id,
                first_control: c[0],
                last_control: c[1],
            });
        }
        if spans.len() > 16384 {
            return Err("Track graph span allowance exceeded".into());
        }
    }
    let by_id: BTreeMap<_, _> = ordered.iter().map(|t| (t.id, *t)).collect();
    let pose = |span: &Span, end: bool| {
        let t = by_id[&span.track_id];
        let i = if end {
            span.last_control
        } else {
            span.first_control
        };
        (t.points[i], t.tangents[i])
    };
    let mut connections = Vec::new();
    let mut parent: Vec<_> = (0..spans.len()).collect();
    let mut joined = BTreeSet::new();
    for i in 0..spans.len() {
        for j in i..spans.len() {
            for (end_a, end_b) in [(false, false), (false, true), (true, false), (true, true)] {
                if i == j && (end_a || !end_b) {
                    continue;
                }
                if !visit()? {
                    return Ok(None);
                }
                let (a, ta) = pose(&spans[i], end_a);
                let (b, tb) = pose(&spans[j], end_b);
                let reverse = end_a == end_b;
                if let Some((gap, angle)) =
                    matches(a, b, ta, if reverse { tb.map(|v| -v) } else { tb }, policy)
                {
                    let a = Endpoint {
                        span: i,
                        end: end_a,
                    };
                    let b = Endpoint {
                        span: j,
                        end: end_b,
                    };
                    connections.push(Connection {
                        a,
                        b,
                        reverse,
                        gap,
                        angle_degrees: angle,
                    });
                    if connections.len() > 262144 {
                        return Err("Track graph connection allowance exceeded".into());
                    }
                    joined.insert(a);
                    joined.insert(b);
                    let ar = root(&mut parent, i);
                    let br = root(&mut parent, j);
                    parent[ar.max(br)] = ar.min(br);
                }
            }
        }
    }
    let mut components: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut terminals = Vec::new();
    for i in 0..spans.len() {
        if !visit()? {
            return Ok(None);
        }
        components.entry(root(&mut parent, i)).or_default().push(i);
        for end in [false, true] {
            let e = Endpoint { span: i, end };
            if !joined.contains(&e) {
                terminals.push(e);
            }
        }
    }
    Ok(Some(Graph {
        engine: VERSION,
        work,
        split_controls,
        spans,
        connections,
        components: components.into_values().collect(),
        terminal_endpoints: terminals,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy() -> Policy {
        Policy {
            engine: StringTag::V1,
            maximum_distance_squared: 0.010000001,
            maximum_angle_degrees: 10.,
        }
    }
    fn track(id: u64, generated: bool, points: Vec<V>, tangents: Vec<V>) -> Track {
        Track {
            id,
            generated,
            points,
            tangents,
        }
    }
    #[test]
    fn internal_generated_joins_split_and_keep_authored_directions_and_terminals() {
        let tracks = [
            track(
                1,
                true,
                vec![[0., 0., 0.], [10., 0., 0.], [20., 0., 0.]],
                vec![[1., 0., 0.]; 3],
            ),
            track(
                2,
                false,
                vec![[10., 0., 0.], [10., 0., 10.]],
                vec![[-1., 0., 0.], [0., 0., 1.]],
            ),
        ];
        let g = build(
            &tracks,
            policy(),
            Settings {
                maximum_work: 10000,
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(g.split_controls, 1);
        assert_eq!(g.spans.len(), 3);
        assert_eq!(g.components.len(), 1);
        assert!(g.connections.iter().any(|c| c.reverse));
        assert_eq!(g.terminal_endpoints.len(), 3);
        assert!(build(&tracks, policy(), Settings { maximum_work: 1 }, || false).is_err());
        assert!(
            build(
                &tracks,
                policy(),
                Settings {
                    maximum_work: 10000
                },
                || true
            )
            .unwrap()
            .is_none()
        );
    }
    #[test]
    fn closed_loops_and_disconnected_components_are_explicit_and_deterministic() {
        let tracks = [
            track(
                3,
                true,
                vec![[0., 0., 0.], [10., 0., 0.], [0., 0., 0.]],
                vec![[1., 0., 0.]; 3],
            ),
            track(
                1,
                false,
                vec![[100., 0., 0.], [110., 0., 0.]],
                vec![[1., 0., 0.]; 2],
            ),
        ];
        let g = build(
            &tracks,
            policy(),
            Settings {
                maximum_work: 10000,
            },
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(g.components.len(), 2);
        assert!(g.connections.iter().any(|c| c.a.span == c.b.span));
        let mut reversed = tracks;
        reversed.reverse();
        assert_eq!(
            serde_json::to_value(&g).unwrap(),
            serde_json::to_value(
                build(
                    &reversed,
                    policy(),
                    Settings {
                        maximum_work: 10000
                    },
                    || false
                )
                .unwrap()
                .unwrap()
            )
            .unwrap()
        );
    }
    #[test]
    fn node_predicate_matches_the_actual_server_on_distance_and_angle_boundaries() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/track-nodes-bd19c4ac.json"))
                .unwrap();
        assert_eq!(
            fixture["reference"],
            "installed-server-CollateTrainTracks-NodesConnect"
        );
        for case in fixture["cases"].as_array().unwrap() {
            let input = &case["input"];
            let v = |key: &str| -> V { serde_json::from_value(input[key].clone()).unwrap() };
            assert_eq!(
                matches(v("A"), v("B"), v("TangentA"), v("TangentB"), policy()).is_some(),
                case["expected"].as_bool().unwrap(),
                "{}",
                input["Name"]
            );
        }
    }
    #[test]
    fn mismatched_angles_distances_and_invalid_directions_do_not_connect() {
        assert!(matches([0.; 3], [0.1, 0., 0.], [1., 0., 0.], [1., 0., 0.], policy()).is_none());
        assert!(
            matches(
                [0.; 3],
                [0.099, 0., 0.],
                [1., 0., 0.],
                [1., 0., 0.],
                policy()
            )
            .is_some()
        );
        assert!(matches([0.; 3], [0.; 3], [1., 0., 0.], [0., 0., 1.], policy()).is_none());
        assert!(
            matches(
                [0.; 3],
                [0.11, 0., 0.],
                [1., 0., 0.],
                [1., 0., 0.],
                policy()
            )
            .is_none()
        );
        let t = [track(
            1,
            false,
            vec![[0.; 3], [1., 0., 0.]],
            vec![[0.; 3]; 2],
        )];
        assert!(
            build(
                &t,
                policy(),
                Settings {
                    maximum_work: 10000
                },
                || false
            )
            .is_err()
        );
    }
}
