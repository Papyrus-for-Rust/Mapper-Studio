use super::{rotation, vector};
use serde::Deserialize;

pub(super) fn validate_query(needle: &str) -> Result<(), String> {
    if needle.is_empty()
        || needle.len() > 128
        || needle.contains('\0')
        || needle != needle.to_lowercase()
    {
        return Err("Invalid monument token query".into());
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Monument {
    pub object_id: i64,
    root: bool,
    hierarchy_active: bool,
    #[serde(rename = "component_enabled")]
    _component_enabled: bool,
    local_to_root: [[f32; 4]; 4],
    rotation_chain: Vec<[f32; 4]>,
    display_phrase: Phrase,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum Phrase {
    Current(Current),
    Legacy(Legacy),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Current {
    #[serde(deserialize_with = "Option::deserialize")]
    token: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    english: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Legacy {
    #[serde(deserialize_with = "Option::deserialize")]
    token: Option<String>,
    #[serde(rename = "legacyEnglish", deserialize_with = "Option::deserialize")]
    english: Option<String>,
}
impl Phrase {
    fn fields(&self) -> (&Option<String>, &Option<String>) {
        match self {
            Self::Current(value) => (&value.token, &value.english),
            Self::Legacy(value) => (&value.token, &value.english),
        }
    }
    fn prepare(&mut self) -> Result<(), String> {
        let (token, english) = self.fields();
        if [token, english]
            .into_iter()
            .flatten()
            .any(|text| text.len() > 4096 || text.contains('\0'))
        {
            return Err("Invalid monument phrase text".into());
        }
        let token = match self {
            Self::Current(value) => &mut value.token,
            Self::Legacy(value) => &mut value.token,
        };
        if let Some(text) = token {
            *text = text.to_lowercase();
        }
        Ok(())
    }
}
impl Monument {
    pub fn prepare(mut self) -> Result<Self, String> {
        if self.object_id == 0
            || !vector(self.local_to_root.as_flattened())
            || self.local_to_root[3] != [0., 0., 0., 1.]
            || self.rotation_chain.len() > 128
            || self.root && !self.rotation_chain.is_empty()
        {
            return Err("Invalid monument phrase identity or transform".into());
        }
        for &value in &self.rotation_chain {
            rotation(value)?;
        }
        self.display_phrase.prepare()?;
        Ok(self)
    }
    pub fn matches(&self, needle: &str) -> Result<bool, String> {
        if !self.hierarchy_active {
            return Ok(false);
        }
        let (token, english) = self.display_phrase.fields();
        if token.as_ref().is_none_or(String::is_empty)
            && english.as_ref().is_none_or(String::is_empty)
        {
            return Ok(false);
        }
        let token = token
            .as_ref()
            .ok_or("Active monument has a valid phrase with a null token")?;
        Ok(token.contains(needle))
    }
}
