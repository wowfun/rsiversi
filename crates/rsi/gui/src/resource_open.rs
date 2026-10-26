//! Operation-scoped correlation for resource allocation, never action authority.
use crate::application::{Command, GuiApplication, Result};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Opened {
    pub request: String,
    pub view: String,
    pub open_revision: String,
}

struct Capture {
    request: String,
    opened: Mutex<Option<Opened>>,
}

tokio::task_local! {
    // The scope follows the admitted future, including across awaits. Concurrent
    // commands cannot overwrite one another's receipt, and spawned observers do
    // not inherit this allocation context.
    static CAPTURE: Arc<Capture>;
}

pub(crate) fn request() -> Option<String> {
    CAPTURE.try_with(|capture| capture.request.clone()).ok()
}

pub(crate) fn allocated(view: u64, revision: u64) -> Result<()> {
    CAPTURE
        .try_with(|capture| {
            let mut opened = capture.opened.lock().expect("resource receipt poisoned");
            if opened.is_some() {
                return Err("One resource command may allocate only one panel".into());
            }
            *opened = Some(Opened {
                request: capture.request.clone(),
                view: view.to_string(),
                open_revision: revision.to_string(),
            });
            Ok(())
        })
        .unwrap_or(Ok(()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    opening_request: String,
    command: Command,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Reply {
    Opened(Opened),
    Unavailable { request: String, reason: String },
}

fn validate(source: &str) -> Result<Request> {
    if source.len() > 64 * 1024 {
        return Err("Resource open exceeds 64 KiB".into());
    }
    let request: Request = serde_json::from_str(source).map_err(|_| "Invalid resource open")?;
    let id = request.opening_request.as_bytes();
    if id.len() != 36
        || !id.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)
            }
        })
    {
        return Err("Invalid resource opening request".into());
    }
    // Each admitted command allocates at most one panel in this task-local scope.
    // Keep multi-panel commands outside this whitelist; allocated() enforces it.
    if !matches!(
        request.command,
        Command::InspectImage { .. }
            | Command::InspectSource { .. }
            | Command::InspectBlock { .. }
            | Command::InspectInteraction { .. }
            | Command::UiBlock { .. }
            | Command::UiSurface { .. }
            | Command::ApplicationUiSurface { .. }
            | Command::RemoteUiList { .. }
            | Command::ReopenRemoteSurface { .. }
    ) {
        return Err("This command cannot open a resource".into());
    }
    Ok(request)
}

impl GuiApplication {
    /// Opens one independently owned document resource through ordinary admission.
    /// The receipt is captured at allocation, independently of frame delivery.
    ///
    /// # Panics
    /// Panics if an earlier application panic poisoned its resource state.
    pub fn try_resource_open(
        self: &Arc<Self>,
        source: &str,
    ) -> Result<futures_util::future::BoxFuture<'static, Result<String>>> {
        let request = validate(source)?;
        self.try_admit(false, None, move |app| async move {
            let capture = Arc::new(Capture {
                request: request.opening_request,
                opened: Mutex::new(None),
            });
            let result = CAPTURE
                .scope(capture.clone(), app.execute(request.command))
                .await;
            let opened = capture
                .opened
                .lock()
                .expect("resource receipt poisoned")
                .take();
            let reply = match (result, opened) {
                (Ok(()), Some(opened)) => Reply::Opened(opened),
                (result, opened) => {
                    if let Some(opened) = opened {
                        app.details
                            .lock()
                            .expect("Web details poisoned")
                            .close(&opened.view, &opened.open_revision)?;
                    }
                    Reply::Unavailable {
                        request: capture.request.clone(),
                        reason: result.err().map_or_else(
                            || "This resource is no longer available".into(),
                            crate::display_error,
                        ),
                    }
                }
            };
            serde_json::to_string(&reply).map_err(crate::display_error)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resource_open_rejects_mutations_and_unbounded_correlation() {
        let id = "00000000-0000-4000-8000-000000000001";
        assert!(
            validate(&format!(
                r#"{{"opening_request":"{id}","command":{{"action":"refresh"}}}}"#
            ))
            .is_err()
        );
        assert!(validate(r#"{"opening_request":"bad","command":{"action":"inspect_interaction","pane":"main","generation":"1","owner":"owner","id":"id"}}"#).is_err());
        assert!(validate(&format!(r#"{{"opening_request":"{id}","command":{{"action":"inspect_interaction","pane":"main","generation":"1","owner":"owner","id":"id"}}}}"#)).is_ok());
    }
    #[tokio::test]
    async fn concurrent_open_receipts_are_isolated_across_yields() {
        async fn open(id: &str, view: u64) -> Opened {
            let capture = Arc::new(Capture {
                request: id.into(),
                opened: Mutex::new(None),
            });
            CAPTURE
                .scope(capture.clone(), async {
                    tokio::task::yield_now().await;
                    allocated(view, view + 1).unwrap();
                })
                .await;
            capture.opened.lock().unwrap().take().unwrap()
        }
        let (a, b) = tokio::join!(open("a", 1), open("b", 3));
        assert_eq!((a.request.as_str(), a.view.as_str()), ("a", "1"));
        assert_eq!((b.request.as_str(), b.view.as_str()), ("b", "3"));
        assert!(request().is_none());
    }
    #[tokio::test]
    async fn duplicate_allocation_is_refused_without_poisoning_the_registry() {
        let registry = Mutex::new(crate::panel_registry::PanelRegistry::default());
        let capture = Arc::new(Capture {
            request: "same-request".into(),
            opened: Mutex::new(None),
        });
        CAPTURE
            .scope(capture.clone(), async {
                registry
                    .lock()
                    .unwrap()
                    .open(None, serde_json::json!({}))
                    .unwrap();
                assert!(
                    registry
                        .lock()
                        .unwrap()
                        .open(None, serde_json::json!({}))
                        .is_err()
                );
                assert!(allocated(99, 100).is_err());
            })
            .await;
        let first = capture.opened.lock().unwrap().take().unwrap();
        let another = Arc::new(Capture {
            request: "same-request".into(),
            opened: Mutex::new(None),
        });
        CAPTURE
            .scope(another.clone(), async {
                assert!(
                    registry
                        .lock()
                        .unwrap()
                        .open(None, serde_json::json!({}))
                        .is_err()
                );
            })
            .await;
        assert!(another.opened.lock().unwrap().is_none());
        let mut registry = registry.lock().unwrap();
        registry.close(&first.view, &first.open_revision).unwrap();
        assert!(registry.snapshot().is_empty());
        registry.open(None, serde_json::json!({})).unwrap();
        assert_eq!(registry.snapshot().len(), 1);
    }
}
