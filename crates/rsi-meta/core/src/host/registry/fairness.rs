use super::{RegistryActor, execute_host_service};
use crate::host::{RegistryQuery, SubscriptionStart};

impl RegistryActor {
    pub(super) fn handle_query(&self, query: RegistryQuery) {
        match query {
            RegistryQuery::Subscribe { reply } => {
                let live = self.events.subscribe();
                let result =
                    self.persistence
                        .latest_cursor()
                        .map(|through_cursor| SubscriptionStart {
                            live,
                            through_cursor,
                        });
                let _ = reply.send(result);
            }
            RegistryQuery::ReplayEvents {
                after_cursor,
                through_cursor,
                limit,
                reply,
            } => {
                let result =
                    self.persistence
                        .query_events_through(after_cursor, through_cursor, limit);
                let _ = reply.send(result);
            }
            RegistryQuery::InspectPlugin { instance_id, reply } => {
                let _ = reply.send(self.plugin_inspections.get(&instance_id).cloned());
            }
        }
    }

    pub(super) async fn await_while_serving<F>(&mut self, future: F) -> F::Output
    where
        F: std::future::Future,
    {
        tokio::pin!(future);
        let result = loop {
            tokio::select! {
                result = &mut future => break result,
                Some(query) = self.query_receiver.recv() => self.handle_query(query),
                Some(call) = self.host_service_receiver.recv() => {
                    let result = execute_host_service(&mut self.persistence, &call);
                    let _ = call.reply.send(result);
                }
                Some(fault) = self.runtime_fault_receiver.recv() => {
                    self.handle_runtime_fault(fault);
                }
            }
        };
        tokio::task::yield_now().await;
        for _ in 0..self.runtime_fault_receiver.len() {
            let fault = self
                .runtime_fault_receiver
                .try_recv()
                .expect("the registry is the sole runtime-fault receiver");
            self.handle_runtime_fault(fault);
        }
        for _ in 0..self.host_service_receiver.len() {
            let call = self
                .host_service_receiver
                .try_recv()
                .expect("the registry is the sole host-service receiver");
            let response = execute_host_service(&mut self.persistence, &call);
            let _ = call.reply.send(response);
        }
        for _ in 0..self.query_receiver.len() {
            let query = self
                .query_receiver
                .try_recv()
                .expect("the registry is the sole query receiver");
            self.handle_query(query);
        }
        result
    }
}
