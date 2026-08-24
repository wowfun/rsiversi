use super::*;

#[tokio::test]
async fn pending_dependency_cycles_are_reported_without_running_factories() {
    #[derive(Debug)]
    struct CycleFactory(PluginDescriptor);

    #[async_trait]
    impl PluginFactory for CycleFactory {
        fn descriptor(&self) -> &PluginDescriptor {
            &self.0
        }

        async fn activate(&self, _: Context, _: Arc<Value>) -> Result<()> {
            panic!("a cyclic factory must remain pending")
        }
    }

    let runtime = Runtime::default();
    let left = runtime
        .root()
        .apply(
            Arc::new(CycleFactory(
                PluginDescriptor::new(FactoryIdentity::builtin("left", "1"))
                    .requiring(Requirement::new("right", "test.right", V1))
                    .providing(Provision::new("left", "test.left", V1)),
            )),
            Value::Null,
        )
        .await
        .unwrap();
    let right = runtime
        .root()
        .apply(
            Arc::new(CycleFactory(
                PluginDescriptor::new(FactoryIdentity::builtin("right", "1"))
                    .requiring(Requirement::new("left", "test.left", V1))
                    .providing(Provision::new("right", "test.right", V1)),
            )),
            Value::Null,
        )
        .await
        .unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let reports_cycle = [left.snapshot(), right.snapshot()].iter().all(|snapshot| {
                matches!(
                    &snapshot.state,
                    FiberState::Pending(report)
                        if report.reasons.iter().any(|reason| matches!(
                            reason,
                            rsi_meta::PendingReason::DependencyCycle { .. }
                        ))
                )
            });
            if reports_cycle {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("both cycle participants should report the cycle");

    assert!(right.dispose().await.is_clean());
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let no_cycle = matches!(
                left.snapshot().state,
                FiberState::Pending(ref report)
                    if !report.reasons.iter().any(|reason| matches!(
                        reason,
                        rsi_meta::PendingReason::DependencyCycle { .. }
                    ))
            );
            if no_cycle {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("disposing a declaration left a stale dependency-cycle diagnostic");

    assert!(left.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test(start_paused = true)]
async fn provider_withdrawal_notifies_only_dependents_in_its_exact_isolation_slot() {
    let runtime = Runtime::default();
    let default_provider = runtime
        .root()
        .apply(
            Arc::new(EndpointFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "default-slot-provider",
                    "1",
                ))
                .providing(Provision::new("isolated", "test.isolation", V1)),
                endpoint: Arc::new(Echo),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    support::wait_active(&default_provider).await;

    let retiring_provider = runtime
        .root()
        .apply(
            Arc::new(EndpointFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "retiring-provider",
                    "1",
                ))
                .requiring(Requirement::new("isolated", "test.isolation", V1))
                .providing(Provision::new("upstream", "test.isolation", V1)),
                endpoint: Arc::new(Echo),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    support::wait_active(&retiring_provider).await;

    let isolated_dependent = runtime
        .root()
        .isolate("isolated", IsolationId(7))
        .unwrap()
        .apply(
            Arc::new(EndpointFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "isolated-dependent",
                    "1",
                ))
                .requiring(Requirement::new("upstream", "test.isolation", V1))
                .providing(Provision::new("isolated", "test.isolation", V1)),
                endpoint: Arc::new(Echo),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    support::wait_active(&isolated_dependent).await;

    let report = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        retiring_provider.dispose(),
    )
    .await
    .expect("an unrelated isolation slot formed a circular dependent wait");
    assert!(report.is_clean());
    assert!(matches!(
        isolated_dependent.snapshot().state,
        FiberState::Pending(_)
    ));

    assert!(isolated_dependent.dispose().await.is_clean());
    assert!(default_provider.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}
