use crate::application::{Result, SettingsEditor};
use rsi_conversation::{FieldWindow, SourceIndex, SourceRef};
use serde::Serialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

pub(crate) const SOURCE_PAGE_BYTES: usize = 64 * 1024;
const SOURCE_PAGE_COUNT: usize = 64;

#[derive(Debug, Serialize)]
pub(crate) struct BlockSources {
    pub pane: u8,
    pub generation: String,
    pub ticket: String,
    pub start: usize,
    pub total: usize,
    pub page: Vec<SourceRef>,
    #[serde(skip)]
    sources: SourceIndex,
}
impl BlockSources {
    pub fn new(pane: u8, generation: String, ticket: String, sources: SourceIndex) -> Self {
        Self {
            pane,
            generation,
            ticket,
            start: 0,
            total: sources.len(),
            page: sources.iter().take(SOURCE_PAGE_COUNT).collect(),
            sources,
        }
    }
    pub fn page(&mut self, ticket: String, forward: bool) {
        self.ticket = ticket;
        self.start = if forward {
            if self.start + self.page.len() >= self.total {
                return;
            }
            self.start + self.page.len()
        } else {
            self.start.saturating_sub(SOURCE_PAGE_COUNT)
        };
        self.page = self
            .sources
            .iter()
            .skip(self.start)
            .take(SOURCE_PAGE_COUNT)
            .collect();
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SourceDetail {
    pub pane: u8,
    pub generation: String,
    pub source: SourceRef,
    pub ticket: String,
    pub window: Option<FieldWindow>,
    pub error: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct Details {
    revision: u64,
    pub stop: CancellationToken,
    pub source: Option<SourceDetail>,
    pub block_sources: Option<BlockSources>,
    pub editor: Option<SettingsEditor>,
    pub interaction: Option<Value>,
}
impl Details {
    pub fn begin(&mut self) -> Result<u64> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or("Detail generation exhausted")?;
        self.stop.cancel();
        self.stop = CancellationToken::new();
        self.source = None;
        self.block_sources = None;
        self.editor = None;
        self.interaction = None;
        Ok(self.revision)
    }
    pub fn source_result(&mut self, revision: u64, result: Result<FieldWindow>) {
        if self.revision != revision || self.stop.is_cancelled() {
            return;
        }
        if let Some(source) = &mut self.source {
            match result {
                Ok(window) => source.window = Some(window),
                Err(error) => source.error = Some(error),
            }
        }
    }
    pub fn detach(&mut self, pane: u8, generation: &str) -> Result<()> {
        if self
            .source
            .as_ref()
            .is_some_and(|source| source.pane == pane && source.generation == generation)
            || self
                .block_sources
                .as_ref()
                .is_some_and(|sources| sources.pane == pane && sources.generation == generation)
            || self
                .interaction
                .as_ref()
                .is_some_and(|detail| detail["pane"] == pane && detail["generation"] == generation)
        {
            self.begin()?;
        }
        Ok(())
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

impl Drop for Details {
    fn drop(&mut self) {
        self.stop.cancel();
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
