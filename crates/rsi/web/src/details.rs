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
impl SourceDetail {
    pub fn media(&self) -> Option<rsi_media_protocol::MediaRef> {
        use rsi_conversation::FactField;
        if !matches!(
            self.source.field,
            FactField::InputImage { .. } | FactField::ToolImage { .. } | FactField::ImageOutput
        ) {
            return None;
        }
        let window = self
            .window
            .as_ref()
            .filter(|window| window.start == 0 && !window.more)?;
        serde_json::from_str(&window.text).ok()
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct SettingsCatalog {
    pub ticket: String,
    pub namespace: Option<String>,
    pub page: Option<rsi_settings_protocol::SettingsPage>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct UiDetail {
    pub pane: u8,
    pub generation: String,
    pub ticket: String,
    #[serde(skip)]
    pub view: Option<rsi_ui::BoundView>,
    #[serde(skip)]
    pub lease: Option<std::sync::Arc<rsi_ui::PresentationLease>>,
    #[serde(skip)]
    pub snapshot: Option<rsi_ui::SnapshotPin>,
    #[serde(skip)]
    pub remote: Option<RemotePresentation>,
    pub binding: Option<rsi_ui::UiReference>,
    pub model: Option<rsi_ui::UiModel>,
    pub error: Option<String>,
    pub busy: bool,
}

#[derive(Debug)]
pub(crate) struct RemotePresentation {
    pub application: String,
    pub item: Option<rsi_ui_api::UiItem>,
    pub closed: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct RemoteCatalog {
    pub pane: u8,
    pub generation: String,
    pub ticket: String,
    pub page: Option<rsi_ui_api::CatalogPage>,
    pub error: Option<String>,
    #[serde(skip)]
    pub scope: rsi_ui_api::ExportScope,
}

#[derive(Debug, Default)]
pub(crate) struct Details {
    pub(crate) revision: u64,
    pub stop: CancellationToken,
    pub ui: Option<UiDetail>,
    pub remote_catalog: Option<RemoteCatalog>,
    pub image: Option<ImageDetail>,
    pub source: Option<SourceDetail>,
    pub block_sources: Option<BlockSources>,
    pub editor: Option<SettingsEditor>,
    pub settings_catalog: Option<SettingsCatalog>,
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
        self.ui = None;
        self.remote_catalog = None;
        self.image = None;
        self.source = None;
        self.block_sources = None;
        self.editor = None;
        self.settings_catalog = None;
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
            .remote_catalog
            .as_ref()
            .is_some_and(|detail| detail.pane == pane && detail.generation == generation)
            || self
                .image
                .as_ref()
                .is_some_and(|detail| detail.pane == pane && detail.generation == generation)
            || self
                .ui
                .as_ref()
                .is_some_and(|detail| detail.pane == pane && detail.generation == generation)
            || self
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
            self.settings_catalog = None;
            self.editor = Some(editor);
        }
    }
    pub fn settings_page(
        &mut self,
        revision: u64,
        result: Result<rsi_settings_protocol::SettingsPage>,
    ) {
        if self.revision == revision
            && let Some(catalog) = &mut self.settings_catalog
        {
            match result {
                Ok(page) => catalog.page = Some(page),
                Err(error) => catalog.error = Some(error),
            }
        }
    }
    pub fn settings_error(&mut self, revision: u64, error: String) {
        if self.revision == revision
            && let Some(catalog) = &mut self.settings_catalog
        {
            catalog.error = Some(error);
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

#[derive(Debug, Serialize)]
pub(crate) struct ImageDetail {
    pub pane: u8,
    pub generation: String,
    pub ticket: String,
    pub media: rsi_media_protocol::MediaRef,
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
                description: rsi_settings_protocol::SettingsDescription {
                    namespace: "rsi.agent".into(),
                    version: version.clone(),
                    defaults: serde_json::json!({}),
                    writable: true,
                    metadata: rsi_settings_protocol::SettingsMetadata {
                        schema: serde_json::json!({}),
                        applies: rsi_settings_protocol::SettingsApply::NewSession,
                        description: "Fixture".into(),
                        sensitive_fields: vec![],
                    },
                },
                version,
            },
        );
        assert!(detail.editor.is_none());
    }
}
