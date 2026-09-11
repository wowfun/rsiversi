use crate::{
    application::{GuiApplication, Result, SettingsEditor, error},
    details::SettingsCatalog,
};
use rsi_settings_protocol::{MAXIMUM_SETTINGS_PAGE, validate_namespace};

impl GuiApplication {
    pub(crate) async fn list_settings(&self, ticket: Option<&str>) -> Result<()> {
        let (revision, stop, after) = {
            let mut details = self.details.lock().expect("Web details poisoned");
            let after = if let Some(ticket) = ticket {
                let Some(catalog) = details
                    .settings_catalog
                    .as_ref()
                    .filter(|catalog| catalog.ticket == ticket)
                else {
                    return Ok(());
                };
                let Some(after) = catalog.page.as_ref().and_then(|page| page.next.clone()) else {
                    return Ok(());
                };
                Some(after)
            } else {
                None
            };
            let revision = details.begin()?;
            details.settings_catalog = Some(SettingsCatalog {
                ticket: revision.to_string(),
                namespace: None,
                page: None,
                error: None,
            });
            (revision, details.stop.clone(), after)
        };
        self.changed();
        let result = tokio::select! { biased;
            () = stop.cancelled() => return Ok(()),
            result = self.settings.list(after.as_deref(), MAXIMUM_SETTINGS_PAGE) => result.map_err(error),
        };
        self.details
            .lock()
            .expect("Web details poisoned")
            .settings_page(revision, result);
        Ok(())
    }
    pub(crate) async fn read_settings(&self, namespace: &str) -> Result<()> {
        validate_namespace(namespace).map_err(error)?;
        let (revision, stop) = {
            let mut details = self.details.lock().expect("Web details poisoned");
            let revision = details.begin()?;
            details.settings_catalog = Some(SettingsCatalog {
                ticket: revision.to_string(),
                namespace: Some(namespace.into()),
                page: None,
                error: None,
            });
            (revision, details.stop.clone())
        };
        self.changed();
        let read = async {
            let description = self.settings.describe(namespace).await.map_err(error)?;
            let snapshot = self.settings.read(namespace).await.map_err(error)?;
            if description.version.scope_id != snapshot.scope_id {
                return Err("Settings registration changed; read it again".into());
            }
            let text = settings_text(&snapshot.value)?;
            Ok(SettingsEditor {
                namespace: namespace.into(),
                text,
                ticket: rsi_ui::fresh_identity("settings")?,
                version: snapshot.version(),
                description,
            })
        };
        let result = tokio::select! { biased;
            () = stop.cancelled() => return Ok(()),
            result = read => result,
        };
        let mut details = self.details.lock().expect("Web details poisoned");
        match result {
            Ok(editor) => details.settings(revision, editor),
            Err(error) => details.settings_error(revision, error),
        }
        Ok(())
    }
    pub(crate) async fn save_settings(&self, ticket: &str, text: &str) -> Result<()> {
        let (namespace, version, mut description) = {
            let details = self.details.lock().expect("Web details poisoned");
            let editor = details
                .editor
                .as_ref()
                .filter(|editor| editor.ticket == ticket)
                .ok_or("Settings view changed; read it again before saving")?;
            if !editor.description.writable {
                return Err("Settings provider is read-only".into());
            }
            (
                editor.namespace.clone(),
                editor.version.clone(),
                editor.description.clone(),
            )
        };
        let value = serde_json::from_str(text).map_err(|_| "Settings must contain valid JSON")?;
        let snapshot = self
            .settings
            .replace(&namespace, &version, value)
            .await
            .map_err(error)?;
        let mut details = self.details.lock().expect("Web details poisoned");
        if details
            .editor
            .as_ref()
            .is_some_and(|editor| editor.ticket == ticket)
        {
            description.version = snapshot.version();
            details.editor = Some(SettingsEditor {
                namespace,
                text: settings_text(&snapshot.value)?,
                ticket: rsi_ui::fresh_identity("settings")?,
                version: snapshot.version(),
                description,
            });
        }
        Ok(())
    }
}

const MAXIMUM_SETTINGS_TEXT_BYTES: usize = 8 * 1024 * 1024;
fn settings_text(value: &serde_json::Value) -> Result<String> {
    // SettingsAccess guarantees the compact section bound before exposing a snapshot.
    pretty_settings_text(value).or_else(|_| serde_json::to_string(value).map_err(error))
}
fn pretty_settings_text(value: &impl serde::Serialize) -> Result<String> {
    struct Output(Vec<u8>);
    impl std::io::Write for Output {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let length = self
                .0
                .len()
                .checked_add(bytes.len())
                .filter(|length| *length <= MAXIMUM_SETTINGS_TEXT_BYTES)
                .ok_or_else(|| {
                    std::io::Error::other("Settings indentation exceeds the editor bound")
                })?;
            if length > self.0.capacity() {
                self.0.reserve_exact(
                    length.next_power_of_two().min(MAXIMUM_SETTINGS_TEXT_BYTES) - self.0.len(),
                );
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Output(Vec::new());
    serde_json::to_writer_pretty(&mut output, value).map_err(error)?;
    String::from_utf8(output.0).map_err(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::ser::SerializeSeq;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Nested<'a>(&'a AtomicUsize, usize);
    impl serde::Serialize for Nested<'_> {
        fn serialize<S: serde::Serializer>(
            &self,
            serializer: S,
        ) -> std::result::Result<S::Ok, S::Error> {
            let mut sequence =
                serializer.serialize_seq(Some(if self.1 == 0 { 500_000 } else { 1 }))?;
            if self.1 == 0 {
                for _ in 0..500_000 {
                    self.0.fetch_add(1, Ordering::SeqCst);
                    sequence.serialize_element(&())?;
                }
            } else {
                sequence.serialize_element(&Nested(self.0, self.1 - 1))?;
            }
            sequence.end()
        }
    }
    #[test]
    fn pretty_expansion_stops_before_serializing_the_complete_oversized_document() {
        let visited = AtomicUsize::new(0);
        let source = Nested(&visited, 16);
        assert!(
            serde_json::to_vec(&source).unwrap().len()
                < rsi_settings_protocol::MAXIMUM_SETTINGS_SECTION_BYTES
        );
        visited.store(0, Ordering::SeqCst);
        assert!(pretty_settings_text(&source).is_err());
        assert!(
            visited.load(Ordering::SeqCst) < 500_000,
            "serialized the entire pretty-expanded value before rejecting it"
        );
        let value: serde_json::Value =
            serde_json::from_slice(&serde_json::to_vec(&source).unwrap()).unwrap();
        let compact = settings_text(&value).unwrap();
        assert!(compact.len() < rsi_settings_protocol::MAXIMUM_SETTINGS_SECTION_BYTES);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&compact).unwrap(),
            value
        );
        assert_eq!(
            settings_text(&serde_json::json!({"enabled":true})).unwrap(),
            "{\n  \"enabled\": true\n}"
        );
    }
}
