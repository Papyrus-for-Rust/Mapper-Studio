//! Runtime-derived wire spans from ordered, fully transformed prefab nodes.
//! The caller supplies scene registration order and actual transform-root names.
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

pub const VERSION: &str = "papyrus-powerline-wires-1";

#[derive(Clone, Debug, Serialize)]
pub struct Connection {
    pub incoming: [f32; 3],
    pub outgoing: [f32; 3],
    pub radius: f32,
    pub hidden: bool,
}

#[derive(Clone, Copy)]
pub struct Node<'a> {
    pub instance: u64,
    pub component: i64,
    pub root: &'a str,
    pub wire: &'a str,
    pub position: [f32; 3],
    pub maximum_distance: f32,
    /// Connection endpoints have already passed through the full node transform.
    pub connections: Option<&'a [Connection]>,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Settings {
    pub minimum_chain_nodes: usize,
    pub maximum_nodes: usize,
    pub maximum_groups: usize,
    pub maximum_chains: usize,
    pub maximum_spans: usize,
    pub maximum_work: usize,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            minimum_chain_nodes: 3,
            maximum_nodes: 65_536,
            maximum_groups: 4096,
            maximum_chains: 65_536,
            maximum_spans: 262_144,
            maximum_work: 32_000_000,
        }
    }
}
impl Settings {
    fn validate(self) -> Result<(), Failure> {
        if !(2..=16).contains(&self.minimum_chain_nodes)
            || !(1..=200_000).contains(&self.maximum_nodes)
            || !(1..=4096).contains(&self.maximum_groups)
            || !(1..=200_000).contains(&self.maximum_chains)
            || !(1..=1_000_000).contains(&self.maximum_spans)
            || !(1..=128_000_000).contains(&self.maximum_work)
        {
            return Err(Failure::Invalid("Invalid wire assembly limits"));
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct Group {
    pub root: String,
    pub nodes: Vec<usize>,
}
#[derive(Debug, Serialize)]
pub struct Chain {
    pub group: usize,
    pub signature_node: usize,
    pub wire: String,
    pub nodes: Vec<usize>,
    pub span_range: [usize; 2],
}
#[derive(Debug, Serialize)]
pub struct Span {
    pub chain: usize,
    pub start_node: usize,
    pub end_node: usize,
    pub connection: usize,
    pub start: [f32; 3],
    pub end: [f32; 3],
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
    /// Retained from the chain's first helper. The reviewed prefab uses unit XY scale.
    pub radius: f32,
}
#[derive(Default, Debug, Serialize)]
pub struct Counts {
    pub nodes: usize,
    pub missing_helpers: usize,
    pub short_chains: usize,
    pub wire_splits: usize,
    pub connection_splits: usize,
    pub distance_splits: usize,
    pub hidden_connections: usize,
    pub degenerate_connections: usize,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub settings: Settings,
    pub groups: Vec<Group>,
    pub chains: Vec<Chain>,
    pub spans: Vec<Span>,
    pub counts: Counts,
    pub work: usize,
}

enum Failure {
    Cancelled,
    Invalid(&'static str),
}
struct Meter<'a, F> {
    cancel: &'a F,
    work: usize,
    maximum: usize,
}
impl<F: Fn() -> bool> Meter<'_, F> {
    fn spend(&mut self, amount: usize) -> Result<(), Failure> {
        if (self.cancel)() {
            return Err(Failure::Cancelled);
        }
        self.work = self
            .work
            .checked_add(amount)
            .filter(|work| *work <= self.maximum)
            .ok_or(Failure::Invalid("Wire assembly work allowance exhausted"))?;
        Ok(())
    }
}
fn vector(value: &[f32; 3]) -> bool {
    value
        .iter()
        .all(|value| value.is_finite() && value.abs() <= 1_000_000.)
}
fn text(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
}
fn squared_distance(start: [f32; 3], end: [f32; 3]) -> f32 {
    start.iter().zip(end).map(|(a, b)| (b - a) * (b - a)).sum()
}

/// Assemble privately. Cancellation, malformed data or exhausted limits publish no report.
pub fn assemble(
    nodes: &[Node<'_>],
    settings: Settings,
    cancel: impl Fn() -> bool,
) -> Result<Option<Report>, String> {
    let mut meter = Meter {
        cancel: &cancel,
        work: 0,
        maximum: settings.maximum_work,
    };
    match build(nodes, settings, &mut meter) {
        Ok(mut report) => {
            report.work = meter.work;
            Ok(Some(report))
        }
        Err(Failure::Cancelled) => Ok(None),
        Err(Failure::Invalid(message)) => Err(message.into()),
    }
}
fn build<F: Fn() -> bool>(
    nodes: &[Node<'_>],
    settings: Settings,
    meter: &mut Meter<'_, F>,
) -> Result<Report, Failure> {
    meter.spend(0)?;
    settings.validate()?;
    if nodes.len() > settings.maximum_nodes {
        return Err(Failure::Invalid("Wire node allowance exhausted"));
    }
    let mut groups: Vec<Group> = Vec::new();
    let mut by_root = BTreeMap::new();
    let mut identities = BTreeSet::new();
    for (index, node) in nodes.iter().enumerate() {
        meter.spend(1)?;
        if node.component == 0
            || !identities.insert((node.instance, node.component))
            || !text(node.root, 1024)
            || !text(node.wire, 128)
            || !vector(&node.position)
            || !node.maximum_distance.is_finite()
            || node.maximum_distance.abs() > 10000.
            || node
                .connections
                .is_some_and(|connections| connections.len() > 256)
        {
            return Err(Failure::Invalid("Invalid or duplicate wire node"));
        }
        for connection in node.connections.unwrap_or_default() {
            meter.spend(1)?;
            if !vector(&connection.incoming)
                || !vector(&connection.outgoing)
                || !connection.radius.is_finite()
                || connection.radius.abs() > 1000.
            {
                return Err(Failure::Invalid("Invalid wire connection"));
            }
        }
        let group = if let Some(&group) = by_root.get(node.root) {
            group
        } else {
            if groups.len() == settings.maximum_groups {
                return Err(Failure::Invalid("Wire root-group allowance exhausted"));
            }
            let group = groups.len();
            groups.push(Group {
                root: node.root.into(),
                nodes: vec![],
            });
            by_root.insert(node.root, group);
            group
        };
        groups[group].nodes.push(index);
    }
    let mut report = Report {
        engine: VERSION,
        settings,
        groups: vec![],
        chains: vec![],
        spans: vec![],
        counts: Counts {
            nodes: nodes.len(),
            ..Counts::default()
        },
        work: 0,
    };
    for (group_id, group) in groups.iter().enumerate() {
        let mut chain = Vec::new();
        let mut signature = None;
        for &index in &group.nodes {
            meter.spend(1)?;
            let node = &nodes[index];
            let Some(connections) = node.connections else {
                report.counts.missing_helpers += 1;
                continue;
            };
            if let Some(&previous) = chain.last() {
                let previous: &Node<'_> = &nodes[previous];
                let signature_node =
                    signature.ok_or(Failure::Invalid("Missing wire group signature"))?;
                let first: &Node<'_> = &nodes[signature_node];
                let split = if node.wire != first.wire {
                    report.counts.wire_splits += 1;
                    true
                } else if connections.len() != first.connections.unwrap_or_default().len() {
                    report.counts.connection_splits += 1;
                    true
                } else if squared_distance(previous.position, node.position)
                    > node.maximum_distance * node.maximum_distance
                {
                    report.counts.distance_splits += 1;
                    true
                } else {
                    false
                };
                if split {
                    finish_chain(
                        nodes,
                        group_id,
                        signature_node,
                        std::mem::take(&mut chain),
                        &mut report,
                        meter,
                    )?;
                }
            } else {
                // The reviewed runtime retains this signature after a split.
                signature = Some(index);
            }
            chain.push(index);
        }
        if let Some(signature) = signature {
            finish_chain(nodes, group_id, signature, chain, &mut report, meter)?;
        }
    }
    report.groups = groups;
    meter.spend(0)?;
    Ok(report)
}
fn finish_chain<F: Fn() -> bool>(
    nodes: &[Node<'_>],
    group: usize,
    signature: usize,
    chain: Vec<usize>,
    report: &mut Report,
    meter: &mut Meter<'_, F>,
) -> Result<(), Failure> {
    if chain.is_empty() {
        return Ok(());
    }
    meter.spend(1)?;
    if report.chains.len() == report.settings.maximum_chains {
        return Err(Failure::Invalid("Wire chain allowance exhausted"));
    }
    let first = &nodes[chain[0]];
    let connections = first.connections.unwrap_or_default();
    let start_span = report.spans.len();
    if chain.len() < report.settings.minimum_chain_nodes {
        report.counts.short_chains += 1;
    } else {
        for pair in chain.windows(2) {
            meter.spend(1)?;
            let start = nodes[pair[0]].connections.unwrap_or_default();
            let end = nodes[pair[1]].connections.unwrap_or_default();
            if start.len() < connections.len() || end.len() < connections.len() {
                return Err(Failure::Invalid(
                    "Wire chain has incompatible connection coverage",
                ));
            }
            for (index, definition) in connections.iter().enumerate() {
                meter.spend(1)?;
                if start[index].hidden || end[index].hidden {
                    report.counts.hidden_connections += 1;
                    continue;
                }
                let a = start[index].outgoing;
                let b = end[index].incoming;
                let length = squared_distance(a, b).sqrt();
                if length <= 1e-6 {
                    report.counts.degenerate_connections += 1;
                    continue;
                }
                if report.spans.len() == report.settings.maximum_spans {
                    return Err(Failure::Invalid("Wire span allowance exhausted"));
                }
                let direction: [f32; 3] = std::array::from_fn(|axis| b[axis] - a[axis]);
                let horizontal = direction[0].hypot(direction[2]);
                let yaw = if horizontal == 0. {
                    0.
                } else {
                    direction[0].atan2(direction[2]).to_degrees()
                };
                let rotation = crate::decor_fit::euler([
                    -direction[1].atan2(horizontal).to_degrees(),
                    yaw,
                    0.,
                ]);
                report.spans.push(Span {
                    chain: report.chains.len(),
                    start_node: pair[0],
                    end_node: pair[1],
                    connection: index,
                    start: a,
                    end: b,
                    position: std::array::from_fn(|axis| (a[axis] + b[axis]) * 0.5),
                    rotation,
                    scale: [1., 1., length],
                    radius: definition.radius,
                });
            }
        }
    }
    report.chains.push(Chain {
        group,
        signature_node: signature,
        wire: nodes[signature].wire.into(),
        nodes: chain,
        span_range: [start_span, report.spans.len()],
    });
    Ok(())
}

#[cfg(test)]
mod tests;
