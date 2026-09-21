// Direct Host tests must observe asynchronous product owner activation.
pub async fn ready(host: &rsi_host::RunningHost) {
    let mut changes = host.subscribe_profile();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let ready = {
                let status = changes.borrow_and_update();
                ["rsi-history", "rsi.history.api", "rsi.managed-providers"]
                    .into_iter()
                    .all(|id| {
                        let instance = status
                            .observed()
                            .iter()
                            .find(|instance| instance.id().as_str() == id);
                        match instance.map(rsi_host::ProfileInstanceStatus::state) {
                            Some(rsi_host::ProfileInstanceState::Active) => true,
                            Some(
                                rsi_host::ProfileInstanceState::Failed
                                | rsi_host::ProfileInstanceState::Disposed
                                | rsi_host::ProfileInstanceState::Unloading,
                            ) => panic!("product owner {id} failed"),
                            _ => false,
                        }
                    })
            };
            if ready {
                break;
            }
            changes
                .changed()
                .await
                .expect("product Profile ended before readiness");
        }
    })
    .await
    .expect("product owners did not become ready");
}
