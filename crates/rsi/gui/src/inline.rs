use super::{Arc, Attachment, BTreeMap, GuiApplication, Result, error};
use rsi_ui::{ActionInput, PresentationLease, SnapshotPin};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(super) struct InlineCard {
    revision: Arc<()>,
    pub(super) lease: Arc<PresentationLease>,
    pub(super) stop: CancellationToken,
    busy: AtomicBool,
    error: std::sync::Mutex<Option<String>>,
}
impl InlineCard {
    fn ticket(&self, snapshot: &SnapshotPin) -> String {
        format!(
            "inline:{}:{}",
            self.lease.identity().epoch,
            snapshot.revision()
        )
    }
}
impl Drop for InlineCard {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

pub(super) fn frames(
    attached: &Attachment,
    state: &crate::renderer::RenderState,
    ui: &rsi_ui::Ui,
) -> serde_json::Value {
    let transcript = state.history.as_ref().unwrap_or(&state.transcript);
    let mut cards = attached.inline.lock().expect("inline cards poisoned");
    cards.retain(|key, card| {
        let current = transcript
            .blocks
            .iter()
            .any(|block| block.key == *key && Arc::ptr_eq(&block.revision, &card.revision))
            && ui.is_current(&card.lease.identity().reference);
        if !current {
            card.stop.cancel();
        }
        current
    });
    serde_json::to_value(cards.iter().map(|(key, card)| {
        let snapshot = card.lease.snapshot().ok().flatten();
        (key, serde_json::json!({
            "ticket": snapshot.as_ref().map(|snapshot| card.ticket(snapshot)),
            "binding": card.lease.identity(),
            "model": snapshot.map(|snapshot| snapshot.model().model),
            "busy": card.busy.load(Ordering::Acquire),
            "error": card.error.lock().expect("inline error poisoned").clone().or(card.lease.status().diagnostic),
        }))
    }).collect::<BTreeMap<_, _>>()).expect("bounded inline frames")
}

impl GuiApplication {
    pub(crate) async fn ui_visible(
        self: &Arc<Self>,
        index: crate::SurfaceId,
        generation: &str,
        sequence: &str,
        keys: Vec<String>,
    ) -> Result<()> {
        let sequence = sequence
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0 && value.to_string() == sequence)
            .ok_or("Invalid visibility sequence")?;
        if keys.len() > 4
            || keys.iter().any(|key| key.len() > 1024)
            || keys.iter().collect::<std::collections::BTreeSet<_>>().len() != keys.len()
        {
            return Err("Visible cards exceed their finite bounds".into());
        }
        let pane = self.pane(index)?;
        let attached = pane.attachment(generation)?;
        let mut last = attached.inline_work.lock().await;
        if sequence <= *last {
            return Ok(());
        }
        let blocks = {
            let state = attached.renderer.state.lock().expect("renderer poisoned");
            let transcript = state.history.as_ref().unwrap_or(&state.transcript);
            keys.iter()
                .filter_map(|key| {
                    transcript
                        .blocks
                        .iter()
                        .find(|block| block.key == *key)
                        .cloned()
                })
                .collect::<Vec<_>>()
        };
        let retired = {
            let mut cards = attached.inline.lock().expect("inline cards poisoned");
            let old = std::mem::take(&mut *cards);
            let mut retired = Vec::new();
            for (key, card) in old {
                if blocks
                    .iter()
                    .any(|block| block.key == key && Arc::ptr_eq(&block.revision, &card.revision))
                    && !card.stop.is_cancelled()
                {
                    cards.insert(key, card);
                } else {
                    card.stop.cancel();
                    retired.push(card);
                }
            }
            retired
        };
        for card in retired {
            if let Err(failure) = card.lease.close().await {
                *self.notice.lock().expect("notice poisoned") = error(failure);
                self.changed();
            }
        }
        for block in blocks {
            if attached
                .inline
                .lock()
                .expect("inline cards poisoned")
                .contains_key(&block.key)
            {
                continue;
            }
            let sources = block.sources();
            let Some(lease) = self
                .ui
                .present_inline(
                    &attached.ui_target,
                    &rsi_ui::BlockInput {
                        key: &block.key,
                        text: &block.text,
                        tool: block.tool.as_ref(),
                        sources: &sources,
                    },
                )
                .map_err(error)?
            else {
                continue;
            };
            let card = Arc::new(InlineCard {
                revision: block.revision,
                lease: Arc::new(lease),
                stop: CancellationToken::new(),
                busy: AtomicBool::new(false),
                error: std::sync::Mutex::default(),
            });
            attached
                .inline
                .lock()
                .expect("inline cards poisoned")
                .insert(block.key, card.clone());
            self.watch_inline(&pane, &card);
        }
        *last = sequence;
        pane.changed();
        self.changed();
        Ok(())
    }

    fn watch_inline(self: &Arc<Self>, pane: &Arc<super::Pane>, card: &InlineCard) {
        let lease = card.lease.clone();
        let stop = card.stop.clone();
        let app = self.clone();
        let pane = pane.clone();
        let task = self.tasks.token();
        self.execution.spawn(async move {
            let _task = task;
            let mut changes = lease.changes();
            loop {
                pane.changed();
                app.changed();
                if changes.borrow_and_update().stopped {
                    break;
                }
                tokio::select! { biased;
                    () = stop.cancelled() => break,
                    result = changes.changed() => { if result.is_err() { break; } }
                }
            }
            if let Err(failure) = lease.close().await {
                *app.notice.lock().expect("notice poisoned") = error(failure);
                app.changed();
            }
        });
    }

    fn selected_inline(&self, ticket: &str) -> Option<(Arc<InlineCard>, SnapshotPin)> {
        let panes = self.panes.lock().expect("GUI panes poisoned");
        for pane in panes.values() {
            let Some(attached) = pane.current.lock().expect("pane poisoned").clone() else {
                continue;
            };
            let state = attached.renderer.state.lock().expect("renderer poisoned");
            let transcript = state.history.as_ref().unwrap_or(&state.transcript);
            for (key, card) in attached
                .inline
                .lock()
                .expect("inline cards poisoned")
                .iter()
            {
                if card.stop.is_cancelled()
                    || card.busy.load(Ordering::Acquire)
                    || !transcript.blocks.iter().any(|block| {
                        block.key == *key && Arc::ptr_eq(&block.revision, &card.revision)
                    })
                {
                    continue;
                }
                let Some(snapshot) = card.lease.snapshot().ok().flatten() else {
                    continue;
                };
                if card.ticket(&snapshot) == ticket {
                    return Some((card.clone(), snapshot));
                }
            }
        }
        None
    }

    pub(super) async fn inline_invoke(
        &self,
        ticket: &str,
        name: String,
        input: ActionInput,
    ) -> Result<()> {
        let (card, snapshot) = self
            .selected_inline(ticket)
            .ok_or("This inline card has retired")?;
        let action = snapshot.action(&name).ok_or("This action is unavailable")?;
        if card.busy.swap(true, Ordering::AcqRel) {
            return Err("This card is busy".into());
        }
        let invocation = card.lease.invoke(&action, input);
        for pane in self.panes.lock().expect("GUI panes poisoned").values() {
            pane.changed();
        }
        self.changed();
        let result = invocation.await.map_err(error);
        *card.error.lock().expect("inline error poisoned") = result.err();
        card.busy.store(false, Ordering::Release);
        // Pane metadata also changed without a new source snapshot on failed actions.
        for pane in self.panes.lock().expect("GUI panes poisoned").values() {
            pane.changed();
        }
        self.changed();
        Ok(())
    }

    pub(super) fn inline_source(
        &self,
        ticket: &str,
        name: &str,
        offset: u64,
        maximum: usize,
    ) -> futures_util::future::BoxFuture<'static, Result<rsi_api_protocol::RetainedBytes>> {
        let Some((card, snapshot)) = self.selected_inline(ticket) else {
            return Box::pin(async { Err("This inline source has retired".into()) });
        };
        let read = card
            .lease
            .source(snapshot.revision(), name, offset, maximum);
        Box::pin(async move { read.await.map_err(error) })
    }
}
