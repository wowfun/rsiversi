use crate::application::{Result, SettingsEditor};
use serde_json::Value;

#[derive(Debug, Default)]
pub(crate) struct Details {
    revision: u64,
    pub editor: Option<SettingsEditor>,
    pub interaction: Option<Value>,
}
impl Details {
    pub fn begin(&mut self) -> Result<u64> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or("Detail generation exhausted")?;
        self.editor = None;
        self.interaction = None;
        Ok(self.revision)
    }
    pub fn settings(&mut self, revision: u64, editor: SettingsEditor) {
        if self.revision == revision {
            self.editor = Some(editor);
        }
    }
    pub fn settle(&mut self, pane: u8, generation: &str, owner: &str, id: &str) {
        if self.interaction.as_ref().is_some_and(|detail| {
            detail["pane"] == pane
                && detail["generation"] == generation
                && detail["request"]["id"] == id
                && (detail["request"]["session_id"] == owner
                    || detail["request"]["subject"]["session_id"] == owner)
        }) {
            self.interaction = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn an_answer_from_a_prior_view_cannot_close_the_other_panes_current_request() {
        let mut detail = Details::default();
        detail.begin().unwrap();
        detail.interaction = Some(
            serde_json::json!({"pane":1,"generation":"2","request":{"id":"question","session_id":"other-session"}}),
        );
        detail.settle(0, "1", "first-session", "question");
        assert!(detail.interaction.is_some());
        detail.settle(1, "1", "other-session", "question");
        assert!(detail.interaction.is_some());
        detail.settle(1, "2", "other-session", "question");
        assert!(detail.interaction.is_none());
    }
    #[test]
    fn closing_a_view_invalidates_an_inflight_settings_read() {
        let mut detail = Details::default();
        let read = detail.begin().unwrap();
        detail.begin().unwrap();
        let version = rsi_settings_protocol::SettingsVersion {
            scope_id: rsi_settings_protocol::SettingsScopeId::parse("0".repeat(32)).unwrap(),
            revision: 0,
        };
        detail.settings(
            read,
            SettingsEditor {
                namespace: "rsi.agent".into(),
                text: "{}".into(),
                ticket: "old".into(),
                version,
            },
        );
        assert!(detail.editor.is_none());
    }
}
