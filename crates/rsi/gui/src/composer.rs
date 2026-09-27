//! Displayed composer actions, independent of streaming transcript revisions.
use crate::{GuiApplication, SurfaceId};
use rsi_agent_session_protocol::{MessageDelivery, TurnId};
use rsi_client_preferences::{BusySubmit, Composer};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Basis {
    pub generation: String,
    active: Option<TurnId>,
    preferences: Composer,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Action {
    Queue,
    Steer,
}
impl Action {
    fn opposite(self) -> Self {
        match self {
            Self::Queue => Self::Steer,
            Self::Steer => Self::Queue,
        }
    }
    fn delivery(self) -> MessageDelivery {
        match self {
            Self::Queue => MessageDelivery::NextTurn,
            Self::Steer => MessageDelivery::Steer,
        }
    }
}
#[derive(Clone, Debug, Serialize)]
struct Choice {
    id: Action,
    label: &'static str,
}
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Projection {
    #[serde(skip)]
    basis: Basis,
    revision: String,
    primary: Choice,
    alternative: Option<Choice>,
}
#[derive(Clone, Debug, Default)]
pub(crate) struct Actions {
    revision: u64,
    offered: BTreeMap<SurfaceId, Projection>,
    acknowledged: BTreeMap<SurfaceId, Projection>,
}
impl Actions {
    pub fn project(&mut self, app: &GuiApplication) -> rsi_api_protocol::Result<serde_json::Value> {
        let panes = app.composer_bases();
        self.offered.retain(|key, _| panes.contains_key(key));
        for (key, basis) in panes {
            if self.offered.get(&key).is_some_and(|old| old.basis == basis) {
                continue;
            }
            self.revision = self
                .revision
                .checked_add(1)
                .ok_or(rsi_api_protocol::ApiError::Capacity)?;
            let busy = basis.active.is_some();
            let primary = if busy && basis.preferences.busy_submit == BusySubmit::Steer {
                Action::Steer
            } else {
                Action::Queue
            };
            let label = |action| match action {
                Action::Queue if !busy => "Send",
                Action::Queue => "Queue",
                Action::Steer => "Steer",
            };
            self.offered.insert(
                key,
                Projection {
                    basis,
                    revision: self.revision.to_string(),
                    primary: Choice {
                        id: primary,
                        label: label(primary),
                    },
                    alternative: busy.then(|| Choice {
                        id: primary.opposite(),
                        label: label(primary.opposite()),
                    }),
                },
            );
        }
        serde_json::to_value(&self.offered).map_err(|_| rsi_api_protocol::ApiError::Capacity)
    }
    pub fn acknowledge(&mut self) {
        self.acknowledged.clone_from(&self.offered);
    }
    pub fn cancel(&self, pane: SurfaceId, basis: &Basis, turn: &TurnId) -> Result<(), String> {
        if !self.acknowledged.get(&pane).is_some_and(|shown| {
            shown.basis.generation == basis.generation
                && shown.basis.active.as_ref() == Some(turn)
                && basis.active.as_ref() == Some(turn)
        }) {
            return Err(
                "Stop target changed or was not displayed; review the refreshed conversation"
                    .into(),
            );
        }
        Ok(())
    }
    pub fn select(
        &self,
        pane: SurfaceId,
        basis: &Basis,
        revision: &str,
        action: Action,
    ) -> Result<MessageDelivery, String> {
        let shown = self.acknowledged.get(&pane).filter(|shown| &shown.basis == basis && shown.revision == revision)
            .ok_or("Sending options changed; review the refreshed action and send again. Your draft is retained.")?;
        if shown.primary.id != action
            && !shown
                .alternative
                .as_ref()
                .is_some_and(|choice| choice.id == action)
        {
            return Err("This delivery action was not displayed".into());
        }
        Ok(action.delivery())
    }
}
impl GuiApplication {
    fn composer_bases(&self) -> BTreeMap<SurfaceId, Basis> {
        let preferences = self
            .preferences
            .lock()
            .expect("GUI preferences poisoned")
            .0
            .web;
        self.panes
            .lock()
            .expect("GUI panes poisoned")
            .iter()
            .filter_map(|(key, pane)| {
                let attached = pane.current.lock().expect("GUI pane poisoned").clone()?;
                let active = attached
                    .renderer
                    .state
                    .lock()
                    .expect("GUI renderer poisoned")
                    .transcript
                    .active
                    .clone();
                Some((
                    *key,
                    Basis {
                        generation: attached.generation.to_string(),
                        active,
                        preferences,
                    },
                ))
            })
            .collect()
    }
    pub(crate) fn displayed_cancel(
        &self,
        pane: SurfaceId,
        generation: &str,
        turn: &TurnId,
    ) -> Result<(), String> {
        let stream = self.stream.lock().expect("GUI frame stream poisoned");
        let basis = self
            .composer_bases()
            .remove(&pane)
            .filter(|basis| basis.generation == generation)
            .ok_or("Stop attachment changed")?;
        stream.actions.cancel(pane, &basis, turn)
    }
    pub(crate) fn composer_delivery(
        &self,
        pane: SurfaceId,
        generation: &str,
        revision: &str,
        action: Action,
    ) -> Result<MessageDelivery, String> {
        let stream = self.stream.lock().expect("GUI frame stream poisoned");
        let basis = self
            .composer_bases()
            .remove(&pane)
            .filter(|basis| basis.generation == generation)
            .ok_or("Composer attachment changed")?;
        stream.actions.select(pane, &basis, revision, action)
    }
}
