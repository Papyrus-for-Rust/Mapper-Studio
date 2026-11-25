//! Machine-local topology groups. Names are resolved against the active source;
//! numeric bit positions are never carried blindly across Rust installations.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub name: String,
    pub layers: Vec<String>,
}

pub fn validate(presets: &[Preset]) -> Result<(), String> {
    if presets.len() > 32 {
        return Err("Keep at most 32 topology groups".into());
    }
    let mut names = std::collections::BTreeSet::new();
    for preset in presets {
        if preset.name.trim().is_empty()
            || preset.name.len() > 64
            || preset.name.chars().any(char::is_control)
            || !names.insert(preset.name.to_lowercase())
            || preset.layers.is_empty()
            || preset.layers.len() > 32
        {
            return Err("Topology groups need unique names and 1–32 layers".into());
        }
        let mut layers = std::collections::BTreeSet::new();
        for layer in &preset.layers {
            if layer.is_empty()
                || layer.len() > 128
                || layer.chars().any(char::is_control)
                || !layers.insert(layer)
            {
                return Err("Topology group layers must be unique installation layer names".into());
            }
        }
    }
    Ok(())
}

/// Resolve all names or fail; loading a partially compatible group must not
/// silently paint only part of a user's intended gameplay policy.
pub fn resolve(layers: &[String], flags: &[(String, u32)]) -> Result<u32, String> {
    if layers.is_empty() {
        return Err("Choose at least one topology layer".into());
    }
    let mut mask = 0;
    for name in layers {
        let mut matches = flags.iter().filter(|(n, _)| n == name);
        let Some((_, bit)) = matches.next() else {
            return Err(format!("Layer {name} is unavailable in this installation"));
        };
        if matches.next().is_some() || *bit == 0 {
            return Err(format!("Layer {name} has ambiguous installation metadata"));
        }
        mask |= bit;
    }
    Ok(mask)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn groups_follow_names_across_bit_changes_and_reject_partial_matches() {
        let layers = vec!["Spawn".into(), "Field".into()];
        assert_eq!(
            resolve(&layers, &[("Spawn".into(), 1), ("Field".into(), 4)]),
            Ok(5)
        );
        assert_eq!(
            resolve(&layers, &[("Spawn".into(), 8), ("Field".into(), 2)]),
            Ok(10)
        );
        assert!(resolve(&layers, &[("Spawn".into(), 1)]).is_err());
        assert!(resolve(&[], &[]).is_err());
        assert!(
            resolve(
                &["Spawn".into()],
                &[("Spawn".into(), 1), ("Spawn".into(), 2)]
            )
            .is_err()
        );
    }
    #[test]
    fn group_budget_and_duplicate_names_are_checked() {
        let preset = Preset {
            name: "My spawn rules".into(),
            layers: vec!["Field".into(), "Spawn".into()],
        };
        validate(std::slice::from_ref(&preset)).unwrap();
        let mut duplicate = preset.clone();
        duplicate.name = preset.name.to_uppercase();
        assert!(validate(&[preset.clone(), duplicate]).is_err());
        let mut invalid = preset;
        invalid.layers.push("Spawn".into());
        assert!(validate(&[invalid]).is_err());
    }
}
