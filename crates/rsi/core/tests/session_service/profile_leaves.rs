use super::{HostProfileId, ProfileCatalog, RunningRsi, composition, fixture, host_profile};
use rsi_api_protocol::{ApiError, ApiOutput, ByteBudget, CallOrigin, OperationSpec};
use rsi_configuration_api::leaf::*;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::sync::{Arc, Condvar, Mutex};
#[path = "profile_leaf_tool.rs"]
mod tool;

async fn raw(
    running: &RunningRsi,
    origin: CallOrigin,
    spec: OperationSpec,
    input: &impl Serialize,
) -> rsi_api_protocol::Result<Value> {
    let input = ByteBudget::default().encode(input, spec.maximum_request_bytes)?;
    let ApiOutput::Reply(reply) = running
        .api_dispatch()
        .unwrap()
        .admit(&spec.id, origin)?
        .invoke(input)
        .await?
    else {
        panic!("finite source operation")
    };
    Ok(serde_json::from_slice(reply.json.as_bytes()).unwrap())
}
async fn call<T: DeserializeOwned>(
    running: &RunningRsi,
    origin: CallOrigin,
    op: Operation,
    input: &impl Serialize,
) -> Reply<T> {
    match raw(running, origin, op.spec(), input).await {
        Ok(value) => Ok(Ok(serde_json::from_value(value).unwrap())),
        Err(ApiError::Domain(bytes)) => Ok(Err(serde_json::from_slice(bytes.as_bytes()).unwrap())),
        Err(error) => Err(error),
    }
}
async fn catalog(running: &RunningRsi, origin: CallOrigin, profile: &str) -> Catalog {
    call(
        running,
        origin,
        Operation::Catalog,
        &CatalogRequest {
            profile: Some(profile.into()),
            after: None,
        },
    )
    .await
    .unwrap()
    .unwrap()
}
async fn grant(running: &RunningRsi, scope: Grant, granted: bool) {
    let grants: Grants = call(running, CallOrigin::Local, Operation::Grants, &json!({}))
        .await
        .unwrap()
        .unwrap();
    let changed: Grants = call(
        running,
        CallOrigin::Local,
        Operation::SetGrant,
        &SetGrant {
            expected: grants.revision.clone(),
            scope: scope.clone(),
            granted,
        },
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        changed.revision.parse::<u64>().unwrap(),
        grants.revision.parse::<u64>().unwrap() + 1
    );
    assert_eq!(changed.scopes.contains(&scope), granted);
}
fn commit(preview: &Preview) -> Commit {
    Commit {
        host_epoch: preview.host_epoch.clone(),
        ticket: preview.ticket.clone(),
        digest: preview.digest.clone(),
    }
}
fn ticket(preview: &Preview) -> Ticket {
    Ticket {
        host_epoch: preview.host_epoch.clone(),
        ticket: preview.ticket.clone(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "One public owner lifecycle preserves the causal grant, source, and receipt sequence"
)]
async fn profile_leaf_owner_requires_distinct_grants_and_retains_exact_conflict_and_save_receipts()
{
    let fixture = fixture("http://127.0.0.1:1");
    let source = std::fs::read(&fixture.profile).unwrap();
    let sources = ProfileCatalog::new(fixture.paths.clone());
    let editable = sources
        .copy_host(
            &HostProfileId::new("fixture").unwrap(),
            &HostProfileId::new("editable").unwrap(),
        )
        .unwrap();
    let running =
        RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &host_profile(&fixture))
            .await
            .unwrap();
    let page = catalog(&running, CallOrigin::Local, "editable").await;
    let leaf = page
        .leaves
        .iter()
        .find(|leaf| leaf.target.leaf == "fixture-provider")
        .unwrap();
    assert!(leaf.allowed.is_empty());
    let public = serde_json::to_string(&page).unwrap();
    for secret in [
        "fixture-secret",
        "http://127.0.0.1:1",
        "context_window_tokens",
    ] {
        assert!(!public.contains(secret));
    }
    let request = PreviewRequest {
        target: leaf.target.clone(),
        change: Change::Enabled { enabled: false },
    };
    let denied: Reply<Preview> =
        call(&running, CallOrigin::Local, Operation::Preview, &request).await;
    assert_eq!(denied.unwrap().unwrap_err(), Failure::Unauthorized);
    let scope = Grant {
        principal: Principal::Local,
        target: leaf.target.clone(),
        operation: ChangeKind::Disable,
    };
    grant(&running, scope.clone(), true).await;
    let preview: Preview = call(&running, CallOrigin::Local, Operation::Preview, &request)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(std::fs::read(&editable).unwrap(), source);
    assert!(!preview.enabled);
    std::fs::write(
        &editable,
        [source.as_slice(), b"\n# another writer\n"].concat(),
    )
    .unwrap();
    let receipt: Receipt = call(
        &running,
        CallOrigin::Local,
        Operation::Commit,
        &commit(&preview),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        receipt.outcome,
        Outcome::Failed {
            failure: Failure::Conflict
        }
    ));
    let again: Receipt = call(
        &running,
        CallOrigin::Local,
        Operation::Receipt,
        &ticket(&preview),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(again.preview.digest, preview.digest);
    assert!(matches!(
        again.outcome,
        Outcome::Failed {
            failure: Failure::Conflict
        }
    ));
    let preview: Preview = call(&running, CallOrigin::Local, Operation::Preview, &request)
        .await
        .unwrap()
        .unwrap();
    grant(&running, scope.clone(), false).await;
    let denied: Reply<Receipt> = call(
        &running,
        CallOrigin::Local,
        Operation::Commit,
        &commit(&preview),
    )
    .await;
    assert_eq!(denied.unwrap().unwrap_err(), Failure::Unauthorized);
    grant(&running, scope, true).await;
    let receipt: Receipt = call(
        &running,
        CallOrigin::Local,
        Operation::Commit,
        &commit(&preview),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        receipt.outcome,
        Outcome::Saved {
            application: Application::NotSelected,
            ..
        }
    ));
    let bytes = std::fs::read(&editable).unwrap();
    let repeated: Receipt = call(
        &running,
        CallOrigin::Local,
        Operation::Commit,
        &commit(&preview),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(repeated.preview.digest, preview.digest);
    assert_eq!(
        std::fs::read(&editable).unwrap(),
        bytes,
        "same ticket must never append a second override"
    );
    assert_eq!(std::fs::read(&fixture.profile).unwrap(), source);
    let receipts: Vec<String> = call(&running, CallOrigin::Local, Operation::Receipts, &json!({}))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipts.len(), 2);
    assert!(receipts.contains(&preview.ticket));
    assert!(running.shutdown().await.is_clean());
    let running =
        RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &host_profile(&fixture))
            .await
            .unwrap();
    let current = catalog(&running, CallOrigin::Local, "editable").await;
    let leaf = current
        .leaves
        .iter()
        .find(|leaf| leaf.target.leaf == "fixture-provider")
        .unwrap();
    assert!(!leaf.enabled);
    assert_eq!(leaf.allowed, vec![ChangeKind::Disable]);
    assert!(matches!(
        call::<Receipt>(
            &running,
            CallOrigin::Local,
            Operation::Receipt,
            &ticket(&preview)
        )
        .await,
        Err(ApiError::Unavailable)
    ));
    assert!(running.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "One public owner lifecycle preserves the causal grant, source, and receipt sequence"
)]
async fn profile_leaf_device_needs_both_grants_and_never_inherits_local_proposals() {
    let fixture = fixture("http://127.0.0.1:1");
    let running =
        RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &host_profile(&fixture))
            .await
            .unwrap();
    let device = running
        .device_administration()
        .unwrap()
        .register("leaf-device")
        .await
        .unwrap();
    let origin = CallOrigin::Device(
        running
            .device_authentication()
            .unwrap()
            .authenticate(&device.token)
            .unwrap(),
    );
    assert!(matches!(
        call::<Catalog>(
            &running,
            origin.clone(),
            Operation::Catalog,
            &CatalogRequest::default()
        )
        .await,
        Err(ApiError::Unauthorized)
    ));
    raw(
        &running,
        CallOrigin::Local,
        rsi_configuration_api::ConfigurationOperation::SetGrant.spec(),
        &json!({"device":device.record.id,"expected_revision":"0","granted":true}),
    )
    .await
    .unwrap();
    let page = catalog(&running, origin.clone(), "fixture").await;
    let target = page
        .leaves
        .iter()
        .find(|leaf| leaf.target.leaf == "fixture-provider")
        .unwrap()
        .target
        .clone();
    let request = PreviewRequest {
        target: target.clone(),
        change: Change::Enabled { enabled: false },
    };
    assert_eq!(
        call::<Preview>(&running, origin.clone(), Operation::Preview, &request)
            .await
            .unwrap()
            .unwrap_err(),
        Failure::Unauthorized
    );
    grant(
        &running,
        Grant {
            principal: Principal::Local,
            target: target.clone(),
            operation: ChangeKind::Disable,
        },
        true,
    )
    .await;
    let local: Preview = call(&running, CallOrigin::Local, Operation::Preview, &request)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        call::<Receipt>(&running, origin.clone(), Operation::Commit, &commit(&local)).await,
        Err(ApiError::OutcomeUnknown | ApiError::Unauthorized)
    ));
    let scope = Grant {
        principal: Principal::Device(device.record.id.clone()),
        target,
        operation: ChangeKind::Disable,
    };
    grant(&running, scope.clone(), true).await;
    let preview: Preview = call(&running, origin.clone(), Operation::Preview, &request)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        call::<Grants>(&running, origin.clone(), Operation::Grants, &json!({})).await,
        Err(ApiError::Unauthorized)
    ));
    grant(&running, scope, false).await;
    assert_eq!(
        call::<Receipt>(
            &running,
            origin.clone(),
            Operation::Commit,
            &commit(&preview)
        )
        .await
        .unwrap()
        .unwrap_err(),
        Failure::Unauthorized
    );
    call::<()>(&running, origin, Operation::Discard, &ticket(&preview))
        .await
        .unwrap()
        .unwrap();
    call::<()>(
        &running,
        CallOrigin::Local,
        Operation::Discard,
        &ticket(&local),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(running.shutdown().await.is_clean());
}

#[derive(Debug, Default)]
struct Preparation {
    entered: tokio::sync::Notify,
    released: Mutex<bool>,
    ready: Condvar,
}
impl Preparation {
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.ready.notify_all();
    }
}
struct Release(Arc<Preparation>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.release();
    }
}
#[derive(Debug)]
struct SlowFactory(Arc<Preparation>);
#[async_trait::async_trait]
impl rsi_meta::PluginFactory for SlowFactory {
    fn prepare(
        &self,
        config: &rsi_meta::ConfigValue,
    ) -> rsi_meta::Result<rsi_meta::PreparedActivation> {
        if config["invalid"] == true {
            return Err(rsi_meta::MetaError::InvalidInput(
                "secret preparation detail".into(),
            ));
        }
        if config["pause"] == true {
            self.0.entered.notify_one();
            let mut released = self.0.released.lock().unwrap();
            while !*released {
                released = self.0.ready.wait(released).unwrap();
            }
        }
        Ok(rsi_meta::PreparedActivation::new(config.clone()))
    }
    async fn activate(&self, _: rsi_meta::ActivationPlan) -> rsi_meta::Result<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "One public owner lifecycle preserves the causal grant, source, and receipt sequence"
)]
async fn profile_revocation_publishes_before_drain_without_blocking_unrelated_grants() {
    let fixture = fixture("http://127.0.0.1:1");
    let mut source = std::fs::read_to_string(&fixture.profile).unwrap();
    source.push_str(
        "\n[[steps]]\nkind='plugin'\nid='slow'\nplugin='fixture.slow'\nconfig={pause=false}\n",
    );
    std::fs::write(&fixture.profile, &source).unwrap();
    let evidence = Arc::new(Preparation::default());
    let _release = Release(evidence.clone());
    let mut builder = rsi::StandardAddonBuilder::new("fixture.profile-leaf");
    builder
        .register_linked(
            "fixture.slow",
            "1",
            rsi_meta::UpdateMode::Replayable,
            Arc::new(SlowFactory(evidence.clone())),
        )
        .unwrap();
    let composition = composition(fixture.paths.clone())
        .with_addons(rsi::StandardAddonSet::new([builder.build().unwrap()]).unwrap());
    let running = Arc::new(
        RunningRsi::boot_host_profile(composition, &host_profile(&fixture))
            .await
            .unwrap(),
    );
    let mut query = CatalogRequest {
        profile: Some("fixture".into()),
        after: None,
    };
    let target = loop {
        let page: Catalog = call(&running, CallOrigin::Local, Operation::Catalog, &query)
            .await
            .unwrap()
            .unwrap();
        page.validate(
            &query,
            &running.connection_description().unwrap().host_epoch,
        )
        .unwrap();
        if let Some(leaf) = page.leaves.iter().find(|leaf| leaf.target.leaf == "slow") {
            break leaf.target.clone();
        }
        query.after = Some(
            page.next
                .expect("slow leaf must occur on a bounded later page"),
        );
    };
    let scope = Grant {
        principal: Principal::Local,
        target: target.clone(),
        operation: ChangeKind::Configuration,
    };
    grant(&running, scope.clone(), true).await;
    let bad = PreviewRequest {
        target: target.clone(),
        change: Change::Configuration {
            value: json!({"invalid":true}),
        },
    };
    assert_eq!(
        call::<Preview>(&running, CallOrigin::Local, Operation::Preview, &bad)
            .await
            .unwrap()
            .unwrap_err(),
        Failure::Preparation
    );
    let read: Grants = call(&running, CallOrigin::Local, Operation::Grants, &json!({}))
        .await
        .unwrap()
        .unwrap();
    let preview = {
        let running = running.clone();
        tokio::spawn(async move {
            call::<Preview>(
                &running,
                CallOrigin::Local,
                Operation::Preview,
                &PreviewRequest {
                    target,
                    change: Change::Configuration {
                        value: json!({"pause":true}),
                    },
                },
            )
            .await
        })
    };
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        evidence.entered.notified(),
    )
    .await
    .unwrap();
    preview.abort();
    assert!(preview.await.unwrap_err().is_cancelled());
    let revoke = SetGrant {
        expected: read.revision,
        scope: scope.clone(),
        granted: false,
    };
    let mut revoke = Box::pin(call::<Grants>(
        &running,
        CallOrigin::Local,
        Operation::SetGrant,
        &revoke,
    ));
    assert!(futures_util::poll!(revoke.as_mut()).is_pending());
    let during = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let grants: Grants = call(&running, CallOrigin::Local, Operation::Grants, &json!({}))
                .await
                .unwrap()
                .unwrap();
            if !grants.scopes.contains(&scope) {
                break grants;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("revocation must publish without waiting for the paused preparation");
    let unrelated = Grant {
        operation: ChangeKind::Disable,
        ..scope.clone()
    };
    let changed: Grants = call(
        &running,
        CallOrigin::Local,
        Operation::SetGrant,
        &SetGrant {
            expected: during.revision,
            scope: unrelated.clone(),
            granted: true,
        },
    )
    .await
    .unwrap()
    .unwrap();
    assert!(changed.scopes.contains(&unrelated));
    assert!(
        futures_util::poll!(revoke.as_mut()).is_pending(),
        "revocation acknowledgement still waits for admitted work"
    );
    assert_eq!(std::fs::read_to_string(&fixture.profile).unwrap(), source);
    evidence.release();
    let revoked = tokio::time::timeout(std::time::Duration::from_secs(10), revoke)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!revoked.scopes.contains(&scope));
    let retained: Vec<Preview> = call(&running, CallOrigin::Local, Operation::Previews, &json!({}))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        retained.len(),
        1,
        "lost preview reply remains recoverable without preparing twice"
    );
    assert_eq!(
        call::<Receipt>(
            &running,
            CallOrigin::Local,
            Operation::Commit,
            &commit(&retained[0])
        )
        .await
        .unwrap()
        .unwrap_err(),
        Failure::Unauthorized
    );
    call::<()>(
        &running,
        CallOrigin::Local,
        Operation::Discard,
        &ticket(&retained[0]),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(std::fs::read_to_string(&fixture.profile).unwrap(), source);
    assert!(running.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "One public owner lifecycle preserves the causal grant, source, and receipt sequence"
)]
async fn profile_commit_reply_loss_reconciles_the_original_ticket_and_directory_replacement_conflicts()
 {
    let fixture = fixture("http://127.0.0.1:1");
    let sources = ProfileCatalog::new(fixture.paths.clone());
    let editable = sources
        .copy_host(
            &HostProfileId::new("fixture").unwrap(),
            &HostProfileId::new("editable").unwrap(),
        )
        .unwrap();
    let running =
        RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &host_profile(&fixture))
            .await
            .unwrap();
    let page = catalog(&running, CallOrigin::Local, "editable").await;
    let target = page
        .leaves
        .iter()
        .find(|leaf| leaf.target.leaf == "fixture-provider")
        .unwrap()
        .target
        .clone();
    grant(
        &running,
        Grant {
            principal: Principal::Local,
            target: target.clone(),
            operation: ChangeKind::Disable,
        },
        true,
    )
    .await;
    let input = PreviewRequest {
        target,
        change: Change::Enabled { enabled: false },
    };
    let preview: Preview = call(&running, CallOrigin::Local, Operation::Preview, &input)
        .await
        .unwrap()
        .unwrap();
    let parent = editable.parent().unwrap();
    let old = parent.with_file_name("old-editable");
    std::fs::rename(parent, &old).unwrap();
    std::fs::create_dir(parent).unwrap();
    let original = std::fs::read(old.join("host.profile.toml")).unwrap();
    std::fs::write(&editable, &original).unwrap();
    let receipt: Receipt = call(
        &running,
        CallOrigin::Local,
        Operation::Commit,
        &commit(&preview),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        receipt.outcome,
        Outcome::Failed {
            failure: Failure::Conflict
        }
    ));
    assert_eq!(std::fs::read(&editable).unwrap(), original);
    let preview: Preview = call(&running, CallOrigin::Local, Operation::Preview, &input)
        .await
        .unwrap()
        .unwrap();
    let commit = commit(&preview);
    let mut write = Box::pin(call::<Receipt>(
        &running,
        CallOrigin::Local,
        Operation::Commit,
        &commit,
    ));
    assert!(futures_util::poll!(write.as_mut()).is_pending());
    drop(write);
    let receipt = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            match call::<Receipt>(
                &running,
                CallOrigin::Local,
                Operation::Receipt,
                &ticket(&preview),
            )
            .await
            {
                Ok(Ok(receipt)) if !matches!(receipt.outcome, Outcome::Pending) => break receipt,
                Ok(Ok(_)) | Err(ApiError::Unavailable | ApiError::Capacity) => {
                    tokio::task::yield_now().await;
                }
                other => panic!("unexpected receipt: {other:?}"),
            }
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        receipt.outcome,
        Outcome::Saved {
            application: Application::NotSelected,
            ..
        }
    ));
    assert_ne!(std::fs::read(&editable).unwrap(), original);
    assert!(running.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "The same resident must survive the complete preview-capacity and restart-required lifecycle"
)]
async fn profile_leaf_preview_capacity_and_restart_observation_preserve_resident_session() {
    let (endpoint, provider) = super::provider().await;
    let fixture = fixture(&endpoint);
    let running =
        RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &host_profile(&fixture))
            .await
            .unwrap();
    let workspace = running
        .workspace_registry()
        .unwrap()
        .get_or_create(&fixture.workspace)
        .await
        .unwrap();
    let session = running
        .session_service()
        .unwrap()
        .create(rsi_session_protocol::CreateSession {
            workspace_id: workspace.id,
            session_id: rsi_agent_session_protocol::SessionId::new("profile-resident").unwrap(),
            agent_preset_id: None,
        })
        .await
        .unwrap();
    super::run_message_to_terminal(&session, "profile-before").await;
    let before = session.inspect().await.unwrap();
    let mut query = CatalogRequest {
        profile: Some("fixture".into()),
        after: None,
    };
    let target = loop {
        let page: Catalog = call(&running, CallOrigin::Local, Operation::Catalog, &query)
            .await
            .unwrap()
            .unwrap();
        if let Some(leaf) = page
            .leaves
            .iter()
            .find(|leaf| leaf.target.leaf == "rsi-inspector-api")
        {
            break leaf.target.clone();
        }
        query.after = Some(page.next.unwrap());
    };
    grant(
        &running,
        Grant {
            principal: Principal::Local,
            target: target.clone(),
            operation: ChangeKind::Disable,
        },
        true,
    )
    .await;
    let request = PreviewRequest {
        target,
        change: Change::Enabled { enabled: false },
    };
    let mut previews = vec![];
    for _ in 0..4 {
        previews.push(
            call::<Preview>(&running, CallOrigin::Local, Operation::Preview, &request)
                .await
                .unwrap()
                .unwrap(),
        );
    }
    assert!(matches!(
        call::<Preview>(&running, CallOrigin::Local, Operation::Preview, &request).await,
        Err(ApiError::Capacity)
    ));
    call::<()>(
        &running,
        CallOrigin::Local,
        Operation::Discard,
        &ticket(&previews.remove(0)),
    )
    .await
    .unwrap()
    .unwrap();
    let preview: Preview = call(&running, CallOrigin::Local, Operation::Preview, &request)
        .await
        .unwrap()
        .unwrap();
    let receipt: Receipt = call(
        &running,
        CallOrigin::Local,
        Operation::Commit,
        &commit(&preview),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(receipt.outcome, Outcome::Saved { .. }));
    assert!(matches!(
        running.reload().await.unwrap(),
        rsi_host::ReloadOutcome::RestartRequired(_)
    ));
    let receipt: Receipt = call(
        &running,
        CallOrigin::Local,
        Operation::Receipt,
        &ticket(&preview),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        receipt.outcome,
        Outcome::Saved {
            application: Application::RestartRequired,
            ..
        }
    ));
    assert_eq!(session.inspect().await.unwrap(), before);
    assert!(running.shutdown().await.is_clean());
    provider.abort();
}
