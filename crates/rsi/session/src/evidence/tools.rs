use super::{BTreeMap, Result, SessionError};
use rsi_ai_protocol::{HostedTool, ToolChoice};
use rsi_session_protocol::ToolComparison;
use serde::Deserialize;
use serde_json::value::RawValue;

pub(super) struct ToolCatalog<'a> {
    pub names: Vec<String>,
    pub hosted: Vec<HostedTool>,
    pub choice: ToolChoice,
    definitions: BTreeMap<&'a str, &'a str>,
}

impl<'a> ToolCatalog<'a> {
    pub fn read(text: &'a str) -> Result<Self> {
        #[derive(Deserialize)]
        struct Tools<'a> {
            #[serde(borrow)]
            definitions: Vec<&'a RawValue>,
            hosted: Vec<HostedTool>,
            choice: ToolChoice,
        }
        #[derive(Deserialize)]
        struct Name<'a> {
            name: &'a str,
        }
        let corrupt = |_| SessionError::Backend("invalid recorded tool catalog".into());
        let tools: Tools<'_> = serde_json::from_str(text).map_err(corrupt)?;
        if tools.definitions.len() > rsi_ai_protocol::MAX_TOOLS {
            return Err(SessionError::Backend(
                "too many recorded tool definitions".into(),
            ));
        }
        let mut definitions = BTreeMap::new();
        let mut names = Vec::with_capacity(tools.definitions.len());
        for raw in tools.definitions {
            let Name { name } = serde_json::from_str(raw.get()).map_err(corrupt)?;
            if definitions.insert(name, raw.get()).is_some() {
                return Err(SessionError::Backend(
                    "duplicate recorded tool definition".into(),
                ));
            }
            names.push(name.to_owned());
        }
        Ok(Self {
            names,
            definitions,
            hosted: tools.hosted,
            choice: tools.choice,
        })
    }

    /// Equal section digests establish equality without decoding the earlier schemas.
    pub fn compare(&self, intent_seq: u64, previous: Option<ToolCatalog<'_>>) -> ToolComparison {
        let mut added = Vec::new();
        let mut removed = Vec::new();
        let mut changed = Vec::new();
        let mut hosted_changed = false;
        let mut choice_changed = false;
        if let Some(previous) = previous {
            for (&name, &schema) in &self.definitions {
                match previous.definitions.get(name) {
                    None => added.push(name.to_owned()),
                    Some(&old) if old != schema => changed.push(name.to_owned()),
                    _ => {}
                }
            }
            removed.extend(
                previous
                    .definitions
                    .keys()
                    .filter(|name| !self.definitions.contains_key(**name))
                    .map(|name| (*name).to_owned()),
            );
            hosted_changed = self.hosted != previous.hosted;
            choice_changed = self.choice != previous.choice;
        }
        ToolComparison::Compared {
            intent_seq,
            added,
            removed,
            changed,
            hosted_changed,
            choice_changed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_uncalled_tools_full_definitions_hosted_configuration_and_choice() {
        let catalog = |defs: serde_json::Value, hosted: Vec<HostedTool>, choice: ToolChoice| {
            serde_json::json!({"definitions":defs,"hosted":hosted,"choice":choice}).to_string()
        };
        let before = catalog(
            serde_json::json!([{"name":"a","input_schema":{}},{"name":"old"}]),
            vec![],
            ToolChoice::Auto,
        );
        let after = catalog(
            serde_json::json!([{"name":"a","input_schema":{"type":"string"}},{"name":"new"}]),
            vec![HostedTool::WebSearch { max_uses: Some(2) }],
            ToolChoice::Required,
        );
        let current = ToolCatalog::read(&after).unwrap();
        assert_eq!(current.names, ["a", "new"]);
        let ToolComparison::Compared {
            intent_seq,
            added,
            removed,
            changed,
            hosted_changed,
            choice_changed,
        } = current.compare(7, Some(ToolCatalog::read(&before).unwrap()))
        else {
            panic!("comparison")
        };
        assert_eq!(intent_seq, 7);
        assert_eq!(added, ["new"]);
        assert_eq!(removed, ["old"]);
        assert_eq!(changed, ["a"]);
        assert!(hosted_changed && choice_changed);
    }
}
