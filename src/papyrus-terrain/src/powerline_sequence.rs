//! Ordered powerline/platform replacement with live accepted-placement history.
use serde::{Deserialize, Serialize};

pub const VERSION: &str = "papyrus-powerline-sequence-1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    Platform,
    Regular,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub engine: String,
    pub regular_spacing: usize,
}
impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        if self.engine != VERSION || !(1..=1024).contains(&self.regular_spacing) {
            return Err("Invalid powerline sequence policy".into());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct History {
    accepted: usize,
    regular_since_platform: usize,
}
impl History {
    /// Call only after successful fitting and terrain admission.
    /// Source history counts plain prefabs but omits them from the rule counter.
    pub fn accept(&mut self, rule: Option<Rule>) -> Result<(), String> {
        let accepted = self
            .accepted
            .checked_add(1)
            .filter(|&n| n <= 1_000_000)
            .ok_or("Powerline sequence history exhausted")?;
        let regular_since_platform = match rule {
            Some(Rule::Platform) => 0,
            Some(Rule::Regular) => self.regular_since_platform + 1,
            None => self.regular_since_platform,
        };
        self.accepted = accepted;
        self.regular_since_platform = regular_since_platform;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Pool {
    regular_spacing: usize,
    first_platform: Option<usize>,
    first_regular: Option<usize>,
}
impl Pool {
    /// Preserve source resource order. A prefab without a Powerline rule is an
    /// eligible fallback, matching the source's failed derived-component cast.
    pub fn new(rules: &[Option<Rule>], policy: &Policy) -> Result<Self, String> {
        policy.validate()?;
        if !(1..=4096).contains(&rules.len()) {
            return Err("Invalid powerline sequence resource roster".into());
        }
        Ok(Self {
            regular_spacing: policy.regular_spacing,
            first_platform: rules
                .iter()
                .position(|r| r.is_none() || *r == Some(Rule::Platform)),
            first_regular: rules
                .iter()
                .position(|r| r.is_none() || *r == Some(Rule::Regular)),
        })
    }

    /// Returns a replacement index only when the selected rule requests one.
    /// Missing alternatives preserve the original selection. This consumes no RNG.
    pub fn replacement(
        self,
        selected: Option<Rule>,
        history: History,
        path_length: usize,
        path_index: usize,
    ) -> Result<Option<usize>, String> {
        if !(1..=1_000_000).contains(&path_length) || path_index >= path_length {
            return Err("Invalid powerline sequence path slot".into());
        }
        let last = path_index == path_length - 1;
        let since = history.regular_since_platform;
        Ok(match selected {
            Some(Rule::Regular)
                if path_length >= 3
                    && (history.accepted == 0 || last || since >= self.regular_spacing) =>
            {
                self.first_platform
            }
            Some(Rule::Platform)
                if path_length < 3
                    || (since < self.regular_spacing && since != history.accepted && !last) =>
            {
                self.first_regular
            }
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests;
