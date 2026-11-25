//! Bounded connector/grid proposal guidance, never placement or road acceptance.
use crate::{
    coastal_placement, monument_attributes::Attributes, monument_source::Recipe,
    placement_preview::RawProposal,
};
use papyrus_terrain::{native_layers::Layers, placement::Position};

pub(crate) struct Context<'a> {
    pub layers: &'a Layers,
    pub attributes: &'a Attributes,
    pub profile: &'a coastal_placement::Profile,
    pub recipe: &'a Recipe,
    pub canonical: &'a Position,
    pub raw: RawProposal,
    pub connector: i64,
}
pub(crate) struct Proposal {
    pub raw: RawProposal,
    pub audit: serde_json::Value,
}
const MAX_ITERATIONS: usize = 8;
const MAX_POSE_CALLS: usize = 32;
fn distance(a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

fn solve(
    start: [f32; 2],
    goal: [f32; 2],
    cell: f32,
    mut evaluate: impl FnMut([f32; 2]) -> Result<[f32; 2], String>,
) -> Result<([f32; 2], [f32; 2], usize), String> {
    if !cell.is_finite() || cell <= 0. || start.iter().chain(&goal).any(|v| !v.is_finite()) {
        return Err("Invalid connector guidance frame".into());
    }
    let mut calls = 0;
    let mut sample = |p| -> Result<[f32; 2], String> {
        if calls == MAX_POSE_CALLS {
            return Err("Connector pose guidance allowance reached".into());
        }
        calls += 1;
        let result = evaluate(p)?;
        if result.iter().any(|v| !v.is_finite()) {
            return Err("Nonfinite connector guidance pose".into());
        }
        Ok(result)
    };
    let mut position = start;
    for _ in 0..MAX_ITERATIONS {
        let current = sample(position)?;
        let residual = [goal[0] - current[0], goal[1] - current[1]];
        if distance(current, goal) <= cell * 0.005 {
            return Ok((position, current, calls));
        }
        let epsilon = cell * 0.125;
        let x = sample([position[0] + epsilon, position[1]])?;
        let z = sample([position[0], position[1] + epsilon])?;
        let [a, c] = [(x[0] - current[0]) / epsilon, (x[1] - current[1]) / epsilon];
        let [b, d] = [(z[0] - current[0]) / epsilon, (z[1] - current[1]) / epsilon];
        let determinant = a * d - b * c;
        if !determinant.is_finite() || determinant.abs() < 1e-4 {
            return Err("Singular connector pose response".into());
        }
        let delta = [
            (d * residual[0] - b * residual[1]) / determinant,
            (-c * residual[0] + a * residual[1]) / determinant,
        ];
        let mut proposed = [
            position[0] + delta[0].clamp(-cell * 0.5, cell * 0.5),
            position[1] + delta[1].clamp(-cell * 0.5, cell * 0.5),
        ];
        let length = distance(proposed, start);
        if length > cell {
            for i in 0..2 {
                proposed[i] = start[i] + (proposed[i] - start[i]) * cell / length;
            }
        }
        position = proposed;
    }
    let current = sample(position)?;
    if distance(current, goal) > cell * 0.02 {
        return Err("Connector grid proposal did not converge within local bounds".into());
    }
    Ok((position, current, calls))
}

// Reuse accepted evaluations and backtrack within the same pose-call budget.
// Clamping a Newton step alone does not make it a descent step when the source
// terrain normal changes quickly underneath a long authored connector arm.
fn solve_safeguarded(
    start: [f32; 2],
    goal: [f32; 2],
    cell: f32,
    mut evaluate: impl FnMut([f32; 2]) -> Result<[f32; 2], String>,
) -> Result<([f32; 2], [f32; 2], usize), String> {
    if !cell.is_finite() || cell <= 0. || start.iter().chain(&goal).any(|v| !v.is_finite()) {
        return Err("Invalid connector guidance frame".into());
    }
    let mut calls = 0;
    let mut sample = |p| -> Result<[f32; 2], String> {
        // The canonical-frame evaluation outside this solver uses one call.
        if calls >= MAX_POSE_CALLS - 1 {
            return Err("Connector pose guidance allowance reached".into());
        }
        calls += 1;
        let result = evaluate(p)?;
        if result.iter().any(|v| !v.is_finite()) {
            return Err("Nonfinite connector guidance pose".into());
        }
        Ok(result)
    };
    let mut position = start;
    let mut current = sample(position)?;
    for _ in 0..MAX_ITERATIONS {
        let error = distance(current, goal);
        if error <= cell * 0.005 {
            return Ok((position, current, calls));
        }
        let residual = [goal[0] - current[0], goal[1] - current[1]];
        let epsilon = (error * 0.125).clamp(cell * 0.015625, cell * 0.125);
        let x = sample([position[0] + epsilon, position[1]])?;
        let z = sample([position[0], position[1] + epsilon])?;
        let [a, c] = [(x[0] - current[0]) / epsilon, (x[1] - current[1]) / epsilon];
        let [b, d] = [(z[0] - current[0]) / epsilon, (z[1] - current[1]) / epsilon];
        let determinant = a * d - b * c;
        if !determinant.is_finite() || determinant.abs() < 1e-4 {
            return Err("Singular connector pose response".into());
        }
        let delta = [
            ((d * residual[0] - b * residual[1]) / determinant).clamp(-cell * 0.5, cell * 0.5),
            ((-c * residual[0] + a * residual[1]) / determinant).clamp(-cell * 0.5, cell * 0.5),
        ];
        let mut accepted = None;
        for scale in [1., 0.5, 0.25] {
            let mut proposed = [
                position[0] + delta[0] * scale,
                position[1] + delta[1] * scale,
            ];
            let length = distance(proposed, start);
            if length > cell {
                for i in 0..2 {
                    proposed[i] = start[i] + (proposed[i] - start[i]) * cell / length;
                }
            }
            let value = sample(proposed)?;
            if distance(value, goal) < error {
                accepted = Some((proposed, value));
                break;
            }
        }
        let Some((next, value)) = accepted else {
            return Err("Connector guidance has no improving bounded step".into());
        };
        position = next;
        current = value;
    }
    if distance(current, goal) > cell * 0.02 {
        return Err("Connector grid proposal did not converge within local bounds".into());
    }
    Ok((position, current, calls))
}

pub(crate) fn propose(
    context: Context<'_>,
    attempt: usize,
    safeguarded: bool,
) -> Result<Proposal, String> {
    if attempt > 1 || !coastal_placement::swims(context.attributes) {
        return Err("Connector guidance requires a local coastal trial".into());
    }
    let connector = context
        .attributes
        .path_connectors
        .iter()
        .find(|c| c.object_id == context.connector)
        .ok_or("Rejected connector is absent from source attributes")?;
    let pose = context.canonical;
    let original = coastal_placement::world(connector.position, [pose.x, pose.y, pose.z], pose.yaw);
    let size = context.layers.size as f32;
    let cell = size / (context.layers.height_resolution - 1) as f32;
    let phase = if attempt == 0 { 0. } else { 0.5 };
    let goal = [original[0], original[2]]
        .map(|v| (((v + size * 0.5) / cell - phase).round() + phase) * cell - size * 0.5);
    if distance(goal, [original[0], original[2]]) < cell * 0.005 {
        return Err("Connector is already at the requested grid phase".into());
    }
    let evaluate = |p: [f32; 2]| {
        let program = if context.recipe.coastal_normals.as_deref()
            == Some(papyrus_terrain::terrain_normal::VERSION)
        {
            coastal_placement::pose_source_normal
        } else {
            coastal_placement::pose
        };
        let (position, yaw) = program(
            context.layers,
            context.attributes,
            [p[0], 0., p[1]],
            context.recipe.placement_yaw(context.raw.yaw),
            context.profile.sea_level,
        )?;
        let point = coastal_placement::world(
            connector.position,
            position,
            context.recipe.placement_yaw(yaw),
        );
        Ok([point[0], point[2]])
    };
    let baseline = evaluate([context.raw.x, context.raw.z])?;
    if distance(baseline, [original[0], original[2]]) > cell * 0.001 {
        return Err("Connector guidance does not reproduce the canonical source frame".into());
    }
    let (point, predicted, calls) = if safeguarded {
        solve_safeguarded([context.raw.x, context.raw.z], goal, cell, evaluate)?
    } else {
        solve([context.raw.x, context.raw.z], goal, cell, evaluate)?
    };
    Ok(Proposal {
        raw: RawProposal {
            x: point[0],
            z: point[1],
            yaw: context.raw.yaw,
        },
        audit: serde_json::json!({
            "engine":if safeguarded {"papyrus-connector-grid-guidance-2"} else {"papyrus-connector-grid-guidance-1"},"connector":context.connector,"phase":phase,
            "canonical_connector":original,"goal":goal,"predicted_connector":predicted,
            "raw_offset":[point[0]-context.raw.x,point[1]-context.raw.z],"pose_calls":calls+1,
            "source_placement_validated":false,"road_validated":false,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn safeguarded_solver_backtracks_worsening_steps_within_original_bounds() {
        let mut queries = Vec::new();
        let (root, connector, calls) = solve_safeguarded([0.; 2], [0.01, 0.], 1., |p| {
            queries.push(p);
            assert!(distance(p, [0.; 2]) <= 1.126);
            Ok([p[0].powi(3), p[1]])
        })
        .unwrap();
        assert!(queries.contains(&[0.5, 0.]));
        assert!(queries.contains(&[0.25, 0.]));
        assert!(distance(connector, [0.01, 0.]) <= 0.005);
        assert!(distance(root, [0.; 2]) <= 1.);
        assert_eq!(queries.len(), calls);
        assert!(calls < MAX_POSE_CALLS);
    }
    #[test]
    fn safeguarded_solver_rejects_invalid_and_unreachable_source_responses() {
        assert!(solve_safeguarded([0.; 2], [0.5; 2], 1., |_| Ok([0.; 2])).is_err());
        assert!(solve_safeguarded([0.; 2], [0.5; 2], 1., |_| Ok([f32::NAN; 2])).is_err());
        let mut calls = 0;
        assert!(
            solve_safeguarded([0.; 2], [10.; 2], 1., |p| {
                calls += 1;
                assert!(distance(p, [0.; 2]) <= 1.126);
                Ok(p)
            })
            .is_err()
        );
        assert!(calls < MAX_POSE_CALLS);
    }
    #[test]
    fn safeguarded_solver_preserves_coupled_linear_solution() {
        let (root, connector, calls) =
            solve_safeguarded([0.; 2], [0.2, 0.3], 1., |p| Ok([p[0] + 2. * p[1], p[1]])).unwrap();
        assert!(distance(root, [-0.4, 0.3]) < 0.001);
        assert!(distance(connector, [0.2, 0.3]) < 0.001);
        assert!(calls < MAX_POSE_CALLS);
    }
    #[test]
    fn coupled_response_aligns_the_connector_rather_than_the_root() {
        let (root, connector, calls) =
            solve([0.; 2], [0.2, 0.3], 1., |p| Ok([p[0] + 2. * p[1], p[1]])).unwrap();
        assert!(distance(root, [-0.4, 0.3]) < 0.001);
        assert!(distance(connector, [0.2, 0.3]) < 0.001);
        assert!(calls <= MAX_POSE_CALLS);
    }
    #[test]
    fn guidance_rejects_nonfinite_singular_and_unreachable_responses_without_expanding_bounds() {
        assert!(solve([0.; 2], [0.5; 2], 1., |_| Ok([0.; 2])).is_err());
        assert!(solve([0.; 2], [0.5; 2], 1., |_| Ok([f32::NAN; 2])).is_err());
        let mut calls = 0;
        assert!(
            solve([0.; 2], [10.; 2], 1., |p| {
                calls += 1;
                assert!(distance(p, [0.; 2]) <= 1.126);
                Ok(p)
            })
            .is_err()
        );
        assert!(calls <= MAX_POSE_CALLS);
    }
}
