use super::*;

#[tokio::test]
async fn mutual_missing_requirements_report_honest_missing_services_without_activation() {
    #[derive(Debug)]
    struct CycleFactory(FactorySpec);

    #[async_trait]
    impl PluginFactory for CycleFactory {
        fn identity(&self) -> FactoryIdentity {
            self.0.identity()
        }

        fn prepare(&self, desired: &Value) -> Result<PreparedActivation> {
            self.0.prepare(desired)
        }

        async fn activate(&self, _: ActivationPlan) -> Result<()> {
            panic!("a cyclic factory must remain pending")
        }
    }

    let runtime = Runtime::default();
    let left = runtime
        .root()
        .apply(
            Arc::new(CycleFactory(
                FactorySpec::new(FactoryIdentity::builtin("left", "1"))
                    .requiring(Requirement::new("right", "test.right", V1)),
            )),
            Value::Null,
        )
        .await
        .unwrap();
    let right = runtime
        .root()
        .apply(
            Arc::new(CycleFactory(
                FactorySpec::new(FactoryIdentity::builtin("right", "1"))
                    .requiring(Requirement::new("left", "test.left", V1)),
            )),
            Value::Null,
        )
        .await
        .unwrap();

    for (snapshot, expected) in [(left.snapshot(), "right"), (right.snapshot(), "left")] {
        let FiberState::Pending(report) = snapshot.state else {
            panic!("mutual missing requirement did not remain Pending");
        };
        assert_eq!(report.total_reasons, 1);
        assert!(matches!(
            report.reasons.as_slice(),
            [rsi_meta::PendingReason::MissingService { service, .. }] if service.as_ref() == expected
        ));
    }

    assert!(right.dispose().await.is_clean());
    assert!(matches!(
        left.snapshot().state,
        FiberState::Pending(ref report)
            if matches!(report.reasons.as_slice(), [rsi_meta::PendingReason::MissingService { service, .. }] if service.as_ref() == "right")
    ));

    assert!(left.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test(start_paused = true)]
async fn provider_withdrawal_notifies_only_dependents_in_its_exact_isolation_slot() {
    let runtime = Runtime::default();
    let default_provider = runtime
        .root()
        .apply(
            Arc::new(EndpointFactory::new(
                FactoryIdentity::builtin("default-slot-provider", "1"),
                "isolated",
                "test.isolation",
                V1,
                Arc::new(Echo),
            )),
            Value::Null,
        )
        .await
        .unwrap();
    support::wait_active(&default_provider).await;

    let retiring_provider = runtime
        .root()
        .apply(
            Arc::new(
                EndpointFactory::new(
                    FactoryIdentity::builtin("retiring-provider", "1"),
                    "upstream",
                    "test.isolation",
                    V1,
                    Arc::new(Echo),
                )
                .requiring(Requirement::new("isolated", "test.isolation", V1)),
            ),
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
            Arc::new(
                EndpointFactory::new(
                    FactoryIdentity::builtin("isolated-dependent", "1"),
                    "isolated",
                    "test.isolation",
                    V1,
                    Arc::new(Echo),
                )
                .requiring(Requirement::new("upstream", "test.isolation", V1)),
            ),
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
