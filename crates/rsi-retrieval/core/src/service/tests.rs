use super::*;
#[derive(Debug)]
struct SettingsFixture(Option<Value>);
#[async_trait::async_trait]
impl SettingsScope for SettingsFixture {
    fn get(&self) -> rsi_settings_protocol::Result<rsi_settings_protocol::SettingsSnapshot> {
        Ok(rsi_settings_protocol::SettingsSnapshot {
            scope_id: rsi_settings_protocol::SettingsScopeId::parse("a".repeat(32)).unwrap(),
            revision: 0,
            value: self
                .0
                .clone()
                .ok_or_else(|| rsi_settings_protocol::SettingsError::Corrupt("fixture".into()))?,
        })
    }
    async fn replace(
        &self,
        _: u64,
        _: Value,
    ) -> rsi_settings_protocol::Result<rsi_settings_protocol::SettingsSnapshot> {
        unreachable!()
    }
    async fn clear(
        &self,
        _: u64,
    ) -> rsi_settings_protocol::Result<rsi_settings_protocol::SettingsSnapshot> {
        unreachable!()
    }
}
#[async_trait::async_trait]
impl CredentialsResolve for SettingsFixture {
    async fn resolve(
        &self,
        _: &rsi_credentials_protocol::CredentialRef,
    ) -> rsi_credentials_protocol::Result<rsi_credentials_protocol::ResolvedCredential> {
        panic!("invalid or disabled settings must not resolve credentials")
    }
}
#[tokio::test]
async fn corrupt_or_unreadable_configuration_is_distinct_from_disabled_access() {
    for (value, expected) in [
        (None, RetrievalError::Configuration),
        (
            Some(json!({"web_fetch":"invalid"})),
            RetrievalError::Configuration,
        ),
        (
            Some(json!({"web_fetch":false,"web_search":false})),
            RetrievalError::Disabled,
        ),
    ] {
        let fixture = Arc::new(SettingsFixture(value));
        let service = Arc::new(RetrievalService {
            dns: Err(RetrievalError::Resolution),
            settings: fixture.clone(),
            credentials: fixture,
            permits: Arc::new(Semaphore::new(1)),
            stop: CancellationToken::new(),
            tasks: TaskTracker::new(),
        });
        assert_eq!(
            service
                .fetch("https://example.com/".into(), CancellationToken::new())
                .await,
            Err(expected)
        );
        service.shutdown().await;
    }
}
#[tokio::test]
async fn worker_panics_are_not_content_decode_or_cancellation_errors() {
    let work = Work {
        dns: Err(RetrievalError::Resolution),
        stop: CancellationToken::new(),
        tasks: TaskTracker::new(),
        _permit: Arc::new(Arc::new(Semaphore::new(1)).try_acquire_owned().unwrap()),
    };
    assert_eq!(
        work.blocking::<()>(|_| panic!("injected worker failure"))
            .await,
        Err(RetrievalError::WorkerFailed)
    );
    work.stop.cancel();
    assert_eq!(
        work.blocking(|_| Ok(())).await,
        Err(RetrievalError::Cancelled)
    );
}
#[test]
fn exa_request_and_sources_preserve_real_highlights_without_generated_answers() {
    let request: Value = serde_json::from_slice(&search_request("rust ownership", 5)).unwrap();
    assert_eq!(
        request,
        json!({"query":"rust ownership","type":"auto","numResults":5,"contents":{"highlights":{"highlightsPerUrl":1}}})
    );
    let response = json!({"results":[{"url":"https://example.com/a","title":"Source A","highlights":[" ","Exact provider highlight","ignored"],"publishedDate":"2026-09-16"},{"url":"https://example.com/b","highlights":[]},{"url":"https://example.com/c"}]});
    let result = normalize_search("rust ownership".into(), 5, &response.to_string()).unwrap();
    assert_eq!(result.sources.len(), 1);
    assert_eq!(result.omitted, 2);
    assert!(!result.truncated);
    assert_eq!(result.sources[0].text, "Exact provider highlight");
    assert_eq!(
        result.sources[0].published_at.as_deref(),
        Some("2026-09-16")
    );
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("generated")
    );
    let mut entries = vec![
        json!({"url":"https://example.com/a","highlights":["界".repeat(9000)],"title":"x".repeat(2000)}),
    ];
    entries.extend((0..10).map(|_| json!({"url":"https://example.com/b","highlights":["useful"]})));
    let result =
        normalize_search("query".into(), 5, &json!({"results":entries}).to_string()).unwrap();
    assert_eq!(result.sources.len(), 5);
    assert!(result.truncated);
    assert!(result.sources[0].truncated);
    assert_eq!(result.sources[0].title.len(), 1024);
    assert!(result.sources[0].text.len() <= 8192);
    for response in [
        json!({}),
        json!({"results":[{"url":"https://example.com","highlights":[42]}]}),
    ] {
        assert_eq!(
            normalize_search("x".into(), 5, &response.to_string()),
            Err(RetrievalError::Protocol)
        );
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abandoned_blocking_work_retains_admission_and_owner_shutdown_until_actual_completion() {
    let permits = Arc::new(Semaphore::new(1));
    let tasks = TaskTracker::new();
    let work = Work {
        dns: Err(RetrievalError::Resolution),
        stop: CancellationToken::new(),
        tasks: tasks.clone(),
        _permit: Arc::new(permits.clone().try_acquire_owned().unwrap()),
    };
    let (started, started_wait) = oneshot::channel();
    let (release, release_wait) = std::sync::mpsc::channel();
    let running = work.clone();
    let waiter = tokio::spawn(async move {
        running
            .blocking(move |_| {
                let _ = started.send(());
                release_wait.recv().unwrap();
                Ok(())
            })
            .await
    });
    started_wait.await.unwrap();
    work.stop.cancel();
    waiter.abort();
    let _ = waiter.await;
    drop(work);
    assert!(permits.clone().try_acquire_owned().is_err());
    tasks.close();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), tasks.wait())
            .await
            .is_err()
    );
    release.send(()).unwrap();
    tasks.wait().await;
    assert!(permits.try_acquire_owned().is_ok());
}

#[test]
fn unusable_search_urls_do_not_hide_other_provider_sources() {
    let response = json!({"results":[
        {"url":"https://example.com/a", "highlights":["valid first"]},
        {"url":"javascript:alert(1)", "highlights":["invalid"]},
        {"highlights":["no URL"]},
        {"url":"https://example.com/b", "highlights":["valid last"]}
    ]});
    let result = normalize_search("fixture".into(), 10, &response.to_string()).unwrap();
    assert_eq!(
        result
            .sources
            .iter()
            .map(|source| source.text.as_str())
            .collect::<Vec<_>>(),
        ["valid first", "valid last"]
    );
    assert_eq!(result.omitted, 2);
    result.validate().unwrap();
}

#[tokio::test]
async fn dns_deadlines_release_all_operation_slots_and_do_not_hold_shutdown() {
    use hickory_resolver::{
        TokioResolver,
        config::{LookupIpStrategy, NameServerConfigGroup, ResolverConfig},
        name_server::TokioConnectionProvider,
    };
    let sink = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let config = ResolverConfig::from_parts(
        None,
        vec![],
        NameServerConfigGroup::from_ips_clear(
            &["127.0.0.1".parse().unwrap()],
            sink.local_addr().unwrap().port(),
            false,
        ),
    );
    let mut builder =
        TokioResolver::builder_with_config(config, TokioConnectionProvider::default());
    builder.options_mut().ip_strategy = LookupIpStrategy::Ipv4AndIpv6;
    builder.options_mut().timeout = std::time::Duration::from_mins(1);
    builder.options_mut().attempts = 1;
    let fixture = Arc::new(SettingsFixture(Some(
        json!({"web_fetch":true,"web_search":false}),
    )));
    let service = Arc::new(RetrievalService {
        dns: Ok(Arc::new(builder.build())),
        settings: fixture.clone(),
        credentials: fixture,
        permits: Arc::new(Semaphore::new(8)),
        stop: CancellationToken::new(),
        tasks: TaskTracker::new(),
    });
    let mut requests = vec![];
    for index in 0..8 {
        let service = service.clone();
        requests.push(tokio::spawn(async move {
            service
                .fetch(
                    format!("https://pending-{index}.example/"),
                    CancellationToken::new(),
                )
                .await
        }));
    }
    let mut packet = [0; 4096];
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        sink.recv_from(&mut packet),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while service.permits.available_permits() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::pause();
    assert_eq!(service.permits.available_permits(), 0);
    assert_eq!(
        service
            .fetch("https://busy.example/".into(), CancellationToken::new())
            .await,
        Err(RetrievalError::Busy)
    );
    tokio::time::advance(std::time::Duration::from_secs(31)).await;
    for request in requests {
        assert_eq!(request.await.unwrap(), Err(RetrievalError::Timeout));
    }
    assert_eq!(
        service.permits.available_permits(),
        8,
        "DNS futures must not retain Work after the deadline"
    );
    tokio::time::timeout(std::time::Duration::from_secs(1), service.shutdown())
        .await
        .unwrap();
    assert!(service.tasks.is_empty());
}
