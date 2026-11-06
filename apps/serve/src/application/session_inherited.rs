use super::{
    Sessions,
    session_history::{read_facts, resource_key},
};
use rsi_agent_session_protocol::{ForkTurnSelection, SessionHeader, SessionId};
use rsi_agent_store_protocol::{InheritedInterval, capture_inherited_intervals};
use rsi_extension::{Error, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Debug)]
struct Selection {
    target: SessionId,
    sources: Vec<InheritedInterval>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReadInterval {
    key: String,
    interval: usize,
    after_seq: u64,
    through_seq: u64,
    offset: usize,
}
impl Sessions {
    pub(super) async fn open_inherited(
        &self,
        request: &str,
        key: &str,
        header: &SessionHeader,
    ) -> Result<Value> {
        let slot = resource_key("seed", key)?;
        let sources = capture_inherited_intervals(self.store.as_ref(), header)
            .await
            .map_err(|error| Error::Remote(error.to_string()))?;
        let meta=sources.iter().map(|row|{
            let origin=row.source.history_origin().expect("captured inherited interval");
            json!({"parent":row.parent.session_id(),"parent_time":row.parent.created_at_ms(),"child":row.source.session_id(),
                "child_time":row.source.created_at_ms(),"after":origin.resolved_after_seq,"through":origin.resolved_terminal_seq,
                "full":matches!(origin.requested_turns,ForkTurnSelection::All)})
        }).collect();
        let selection = Arc::new(Selection {
            target: header.session_id().clone(),
            sources,
        });
        self.routes
            .retain_resource(request, slot, selection)
            .map_err(|e| Error::Remote(e.to_string()))?;
        Ok(Value::Array(meta))
    }
    pub(super) async fn read_inherited(
        &self,
        request: &str,
        target: &SessionId,
        read: ReadInterval,
    ) -> Result<Value> {
        let (selection, retired) = self
            .routes
            .shared_resource::<Selection>(request, &resource_key("seed", &read.key)?)
            .map_err(|_| Error::Cancelled)?;
        if &selection.target != target {
            return Err(Error::Protocol);
        }
        let source = selection
            .sources
            .get(read.interval)
            .ok_or(Error::Protocol)?;
        let origin = source
            .source
            .history_origin()
            .expect("captured inherited interval");
        if read.after_seq < origin.resolved_after_seq
            || read.after_seq >= read.through_seq
            || read.through_seq > origin.resolved_terminal_seq
        {
            return Err(Error::Protocol);
        }
        tokio::select! {biased;()=retired.cancelled()=>Err(Error::Cancelled),
        result=read_facts(self.store.as_ref(),&origin.parent_session_id,read.after_seq,read.through_seq,read.offset)=>result}
    }
}
