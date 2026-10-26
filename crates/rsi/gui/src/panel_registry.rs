//! Resource lifetime is independent of document layout. Tickets never survive close.
use crate::{SurfaceId, application::Result, details::Details};
use rsi_agent_session_protocol::SessionId;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

const MAXIMUM_VIEWS: usize = 64;
const MAXIMUM_SESSION_VIEWS: usize = 16;
const MAXIMUM_SESSION_FLOATS: usize = 4;

#[derive(Debug)]
struct Entry {
    open_revision: u64,
    opening_request: Option<String>,
    owner: Option<(SurfaceId, String, SessionId)>,
    reopen: Value,
    floating: bool,
    detail: Details,
}
#[derive(Debug)]
pub(crate) struct PanelRegistry {
    sequence: Arc<AtomicU64>,
    entries: BTreeMap<u64, Entry>,
    pub settings: Details,
}
impl Default for PanelRegistry {
    fn default() -> Self {
        let sequence = Arc::new(AtomicU64::new(0));
        Self {
            sequence: sequence.clone(),
            entries: BTreeMap::new(),
            settings: Details::with_sequence(sequence),
        }
    }
}
impl PanelRegistry {
    pub fn open(
        &mut self,
        owner: Option<(SurfaceId, String, SessionId)>,
        reopen: Value,
    ) -> Result<&mut Details> {
        let opening_request = crate::resource_open::request();
        if opening_request.is_some()
            && self
                .entries
                .values()
                .any(|entry| entry.opening_request == opening_request)
        {
            return Err("Resource opening request already owns a panel".into());
        }
        let session = owner.as_ref().map(|owner| &owner.2);
        if self.entries.len() >= MAXIMUM_VIEWS
            || self
                .entries
                .values()
                .filter(|entry| entry.owner.as_ref().map(|owner| &owner.2) == session)
                .count()
                >= MAXIMUM_SESSION_VIEWS
        {
            return Err("Close a resource tab before opening another".into());
        }
        if serde_json::to_vec(&reopen)
            .map_err(|error| error.to_string())?
            .len()
            > 64 * 1024
        {
            return Err("Resource coordinates exceed their limit".into());
        }
        let id = self
            .sequence
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| "View identity exhausted")?
            + 1;
        let mut detail = Details::with_sequence(self.sequence.clone());
        detail.begin()?;
        let open_revision = detail.revision;
        crate::resource_open::allocated(id, open_revision)?;
        Ok(&mut self
            .entries
            .entry(id)
            .or_insert(Entry {
                open_revision,
                opening_request,
                owner,
                reopen,
                floating: false,
                detail,
            })
            .detail)
    }
    /// Reinspect an exact interaction without allocating another document tab.
    pub fn reopen(
        &mut self,
        owner: Option<(SurfaceId, String, SessionId)>,
        reopen: Value,
    ) -> Result<&mut Details> {
        if crate::resource_open::request().is_some() {
            return self.open(owner, reopen);
        }
        let existing = self
            .entries
            .iter()
            .find(|(_, entry)| entry.owner == owner && entry.reopen == reopen)
            .map(|(id, _)| *id);
        if let Some(id) = existing {
            let entry = self.entries.get_mut(&id).expect("selected entry");
            entry.detail.begin()?;
            entry.open_revision = entry.detail.revision;
            entry.opening_request = None;
            return Ok(&mut entry.detail);
        }
        self.open(owner, reopen)
    }
    pub fn ticket(&self, ticket: &str) -> Result<&Details> {
        let revision = parse(ticket)?;
        self.entries
            .values()
            .find(|entry| entry.detail.revision == revision && !entry.detail.stop.is_cancelled())
            .map(|entry| &entry.detail)
            .ok_or_else(|| "Resource view has retired".into())
    }
    pub fn ticket_mut(&mut self, ticket: &str) -> Result<&mut Details> {
        self.revision_mut(parse(ticket)?)
            .ok_or_else(|| "Resource view has retired".into())
    }
    pub fn revision_mut(&mut self, revision: u64) -> Option<&mut Details> {
        self.entries
            .values_mut()
            .find(|entry| entry.detail.revision == revision && !entry.detail.stop.is_cancelled())
            .map(|entry| &mut entry.detail)
    }
    pub fn lease_mut(&mut self, lease: &Arc<rsi_ui::PresentationLease>) -> Option<&mut Details> {
        self.entries
            .values_mut()
            .find(|entry| {
                !entry.detail.stop.is_cancelled()
                    && entry
                        .detail
                        .ui
                        .as_ref()
                        .and_then(|ui| ui.lease.as_ref())
                        .is_some_and(|current| Arc::ptr_eq(current, lease))
            })
            .map(|entry| &mut entry.detail)
    }
    pub fn remote_mut(&mut self, application: &str) -> Option<&mut Details> {
        self.entries
            .values_mut()
            .find(|entry| {
                !entry.detail.stop.is_cancelled()
                    && entry
                        .detail
                        .ui
                        .as_ref()
                        .and_then(|ui| ui.remote.as_ref())
                        .is_some_and(|remote| remote.application == application)
            })
            .map(|entry| &mut entry.detail)
    }
    pub fn close(&mut self, view: &str, expected_open_revision: &str) -> Result<()> {
        let view = parse(view)?;
        let expected = parse(expected_open_revision)?;
        if self
            .entries
            .get(&view)
            .is_some_and(|entry| entry.open_revision == expected)
        {
            self.entries.remove(&view);
        }
        Ok(())
    }
    pub fn floating(&mut self, view: &str, floating: bool) -> Result<()> {
        let id = parse(view)?;
        let entry = self.entries.get(&id).ok_or("Resource view has retired")?;
        let session = entry.owner.as_ref().map(|owner| &owner.2);
        if floating
            && !entry.floating
            && self
                .entries
                .values()
                .filter(|entry| {
                    entry.floating && entry.owner.as_ref().map(|owner| &owner.2) == session
                })
                .count()
                >= MAXIMUM_SESSION_FLOATS
        {
            return Err("At most four resource windows can float in a Session".into());
        }
        self.entries.get_mut(&id).expect("checked entry").floating = floating;
        Ok(())
    }
    pub fn detach(&mut self, pane: SurfaceId, generation: &str) {
        self.entries.retain(|_, entry| {
            entry
                .owner
                .as_ref()
                .is_none_or(|owner| owner.0 != pane || owner.1 != generation)
        });
    }
    pub fn settle(&mut self, pane: SurfaceId, generation: &str, owner: &str, id: &str) {
        self.entries.retain(|_, entry| {
            let had = entry.detail.interaction.is_some();
            entry.detail.settle(pane, generation, owner, id);
            !had || entry.detail.interaction.is_some()
        });
    }
    pub fn settle_observed(
        &mut self,
        pane: SurfaceId,
        generation: &str,
        owner: &str,
        id: &str,
        kind: &str,
    ) -> bool {
        let before = self.entries.len();
        self.entries.retain(|_, entry| {
            if entry
                .detail
                .interaction
                .as_ref()
                .is_none_or(|detail| detail["kind"] != kind)
            {
                return true;
            }
            entry.detail.settle(pane, generation, owner, id);
            entry.detail.interaction.is_some()
        });
        self.entries.len() != before
    }
    pub fn prune_ui(&mut self, current: impl Fn(&rsi_ui::UiReference) -> bool) {
        self.entries.retain(|_, entry| {
            entry
                .detail
                .ui
                .as_ref()
                .filter(|ui| ui.remote.is_none())
                .and_then(|ui| ui.binding.as_ref())
                .is_none_or(&current)
        });
    }
    pub fn stop(&mut self) {
        self.entries.clear();
        self.settings.stop.cancel();
    }
    pub fn snapshot(&self) -> Vec<Value> {
        self.entries.iter().map(|(id, entry)| {
            let detail = &entry.detail;
            json!({"view":id.to_string(),"open_revision":entry.open_revision.to_string(),"opening_request":entry.opening_request,"session":entry.owner.as_ref().map(|owner| &owner.2),"pane":entry.owner.as_ref().map(|owner| owner.0),"generation":entry.owner.as_ref().map(|owner| &owner.1),"floating":entry.floating,"reopen":entry.reopen,
                "ui_detail":detail.ui,"remote_ui_catalog":detail.remote_catalog,
                "image_detail":detail.image,"source_media":detail.source.as_ref().and_then(crate::details::SourceDetail::media),
                "detail":detail.interaction,"source_detail":detail.source,"block_sources":detail.block_sources})
        }).collect()
    }
}
fn parse(value: &str) -> Result<u64> {
    value
        .parse()
        .ok()
        .filter(|number: &u64| *number != 0 && number.to_string() == value)
        .ok_or_else(|| "Invalid resource identity".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn owner() -> (SurfaceId, String, SessionId) {
        (
            SurfaceId::MAIN,
            "1".into(),
            SessionId::new("session").unwrap(),
        )
    }
    #[test]
    fn closing_and_reopening_one_view_cannot_retire_siblings_or_revive_old_tickets() {
        let mut registry = PanelRegistry::default();
        let first = registry.open(Some(owner()), json!({})).unwrap();
        let ticket = first.revision.to_string();
        let stopped = first.stop.clone();
        let second = registry.open(Some(owner()), json!({})).unwrap();
        let other = second.revision.to_string();
        let active = second.stop.clone();
        let view = registry.snapshot()[0]["view"].as_str().unwrap().to_owned();
        registry.floating(&view, true).unwrap();
        assert!(registry.ticket(&ticket).is_ok());
        assert!(!stopped.is_cancelled());
        registry.close(&view, &ticket).unwrap();
        assert!(stopped.is_cancelled());
        assert!(!active.is_cancelled());
        assert!(registry.ticket(&ticket).is_err());
        assert!(registry.ticket(&other).is_ok());
        registry.open(Some(owner()), json!({})).unwrap();
        assert!(registry.ticket(&ticket).is_err());
        registry.settings.begin().unwrap();
        assert!(!active.is_cancelled());
        registry.detach(SurfaceId::MAIN, "old");
        assert!(!active.is_cancelled());
        registry.detach(SurfaceId::MAIN, "1");
        assert!(active.is_cancelled());
    }
    #[test]
    fn view_and_float_capacity_rejection_preserves_current_authorities() {
        let mut registry = PanelRegistry::default();
        for _ in 0..16 {
            registry.open(Some(owner()), json!({})).unwrap();
        }
        assert!(registry.open(Some(owner()), json!({})).is_err());
        let views = registry.snapshot();
        for item in &views[..4] {
            registry
                .floating(item["view"].as_str().unwrap(), true)
                .unwrap();
        }
        assert!(
            registry
                .floating(views[4]["view"].as_str().unwrap(), true)
                .is_err()
        );
        assert_eq!(registry.snapshot().len(), 16);
        for entry in registry.entries.values() {
            assert!(!entry.detail.stop.is_cancelled());
        }
    }
    #[test]
    fn delayed_close_cannot_remove_a_reinspected_incarnation() {
        let mut registry = PanelRegistry::default();
        registry.open(Some(owner()), json!({})).unwrap();
        let first = registry.snapshot()[0].clone();
        registry.reopen(Some(owner()), json!({})).unwrap();
        let next = registry.snapshot()[0].clone();
        assert_eq!(first["view"], next["view"]);
        assert_ne!(first["open_revision"], next["open_revision"]);
        registry
            .close(
                first["view"].as_str().unwrap(),
                first["open_revision"].as_str().unwrap(),
            )
            .unwrap();
        assert_eq!(registry.snapshot().len(), 1);
        // Updating content does not change the closing identity.
        registry
            .entries
            .values_mut()
            .next()
            .unwrap()
            .detail
            .begin()
            .unwrap();
        registry
            .close(
                next["view"].as_str().unwrap(),
                next["open_revision"].as_str().unwrap(),
            )
            .unwrap();
        assert!(registry.snapshot().is_empty());
        registry
            .close(
                next["view"].as_str().unwrap(),
                next["open_revision"].as_str().unwrap(),
            )
            .unwrap();
    }
}
