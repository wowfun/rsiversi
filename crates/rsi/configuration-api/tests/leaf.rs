use rsi_api_protocol::*;
use rsi_configuration_api::leaf::{Application, Client, Operation, Outcome, Receipt, SetGrant};
use std::sync::{Arc, Mutex};
#[derive(Debug)]
struct Remote {
    description: ConnectionDescription,
    operations: Vec<OperationSpec>,
    reply: Mutex<Option<Value>>,
}
#[async_trait::async_trait]
impl ApiClient for Remote {
    fn description(&self) -> &ConnectionDescription {
        &self.description
    }
    fn operations(&self) -> &[OperationSpec] {
        &self.operations
    }
    fn input_budget(&self, _: OperationClass) -> ByteBudget {
        ByteBudget::default()
    }
    async fn call(&self, op: &OperationSpec, _: RetainedBytes) -> Result<ApiOutput> {
        let value = self.reply.lock().unwrap().take().unwrap();
        Ok(ApiOutput::Reply(ApiMessage {
            json: ByteBudget::default().encode(&value, op.maximum_response_bytes)?,
            binary: None,
        }))
    }
}
use rsi_api_protocol::HostEpoch;
use rsi_configuration_api::leaf::{
    Catalog, CatalogRequest, Change, ChangeKind, Commit, Grant, Grants, Leaf, Preview, Principal,
    Target, validate_configuration,
};
use serde_json::{Value, json};

fn target() -> Target {
    Target {
        root: "a".repeat(64),
        profile: "editable".into(),
        leaf: "fixture.plugin".into(),
    }
}

#[test]
fn grant_byte_bound_is_independent_of_the_scope_count() {
    let scopes = (0..128)
        .map(|i| Grant {
            principal: Principal::Local,
            target: Target {
                profile: "p".repeat(255),
                leaf: format!("{i:03}{}", "x".repeat(253)),
                ..target()
            },
            operation: ChangeKind::Disable,
        })
        .collect();
    let grants = Grants {
        revision: "1".into(),
        scopes,
    };
    assert_eq!(grants.scopes.len(), 128);
    assert!(serde_json::to_vec(&grants).unwrap().len() > 64 * 1024);
    assert!(grants.validate().is_err());
}

#[tokio::test]
async fn decoded_mutation_metadata_cannot_claim_another_review_or_known_failure() {
    let epoch = HostEpoch::generate().unwrap();
    let remote = Arc::new(Remote {
        description: ConnectionDescription {
            wire_version: 1,
            endpoint_id: EndpointId::generate().unwrap(),
            host_epoch: epoch.clone(),
        },
        operations: [
            Operation::Catalog,
            Operation::Preview,
            Operation::Previews,
            Operation::Commit,
            Operation::Receipt,
            Operation::Receipts,
            Operation::Discard,
            Operation::Grants,
            Operation::SetGrant,
        ]
        .into_iter()
        .map(Operation::spec)
        .collect(),
        reply: Mutex::new(None),
    });
    let client = Client::new(remote.clone()).unwrap();
    let preview = Preview {
        host_epoch: epoch,
        ticket: "b".repeat(32),
        target: target(),
        operation: ChangeKind::Disable,
        digest: "c".repeat(64),
        source_digest: "d".repeat(64),
        plugin: "fixture.plugin".into(),
        previous_enabled: true,
        enabled: false,
        effective_enabled: false,
    };
    for invalid in [false, true] {
        let mut other = preview.clone();
        if invalid {
            other.effective_enabled = true;
        } else {
            other.previous_enabled = false;
        }
        *remote.reply.lock().unwrap() = Some(
            serde_json::to_value(Receipt {
                preview: other,
                outcome: Outcome::Saved {
                    directory_synced: true,
                    application: Application::Applied,
                },
            })
            .unwrap(),
        );
        assert!(matches!(
            client.commit(&preview).await,
            Err(ApiError::OutcomeUnknown)
        ));
    }
    *remote.reply.lock().unwrap() = Some(json!({"revision":"01","scopes":[]}));
    assert!(matches!(
        client
            .set_grant(SetGrant {
                expected: "0".into(),
                scope: Grant {
                    principal: Principal::Local,
                    target: target(),
                    operation: ChangeKind::Disable
                },
                granted: true
            })
            .await,
        Err(ApiError::OutcomeUnknown)
    ));
}
#[test]
fn exact_configuration_and_closed_source_identity_are_bounded_before_use() {
    let exact: Value =
        serde_json::from_str(r#"{"number":1e+400,"integer":18446744073709551615,"null":null}"#)
            .unwrap();
    validate_configuration(&exact).unwrap();
    let change = Change::Configuration {
        value: exact.clone(),
    };
    let decoded: Change = serde_json::from_value(serde_json::to_value(&change).unwrap()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap()["value"], exact);
    assert!(
        !format!(
            "{:?}",
            Change::Configuration {
                value: json!({"secret":"do-not-print"})
            }
        )
        .contains("do-not-print")
    );
    for value in [
        json!("a".repeat(64 * 1024)),
        json!("\0".repeat(12 * 1024)),
        json!(vec![0; 4096]),
    ] {
        assert!(validate_configuration(&value).is_err());
    }
    let mut nested = Value::Null;
    for _ in 0..33 {
        nested = json!([nested]);
    }
    assert!(validate_configuration(&nested).is_err());
    for profile in ["standard", "../editable", "Editable", "a b"] {
        assert!(
            Target {
                profile: profile.into(),
                ..target()
            }
            .validate()
            .is_err()
        );
    }
    for root in ["a".repeat(63), "A".repeat(64)] {
        assert!(Target { root, ..target() }.validate().is_err());
    }
    assert!(serde_json::from_value::<Target>(json!({"root":"a".repeat(64),"profile":"editable","leaf":"fixture.plugin","path":"secret"})).is_err());
}
#[test]
fn prepared_preview_and_catalog_reject_identity_confusion_without_rejecting_byte_limited_pages() {
    let epoch = HostEpoch::generate().unwrap();
    let mut preview = Preview {
        host_epoch: epoch.clone(),
        ticket: "b".repeat(32),
        target: target(),
        operation: ChangeKind::Disable,
        digest: "c".repeat(64),
        source_digest: "d".repeat(64),
        plugin: "fixture.plugin".into(),
        previous_enabled: true,
        enabled: false,
        effective_enabled: false,
    };
    preview.validate(&epoch).unwrap();
    preview.enabled = true;
    assert!(preview.validate(&epoch).is_err());
    preview.enabled = false;
    preview.effective_enabled = true;
    assert!(preview.validate(&epoch).is_err());
    assert!(
        Commit {
            host_epoch: HostEpoch::generate().unwrap(),
            ticket: "b".repeat(32),
            digest: "c".repeat(64)
        }
        .validate(&epoch)
        .is_err()
    );
    let leaf = Leaf {
        target: target(),
        plugin: "fixture.plugin".into(),
        enabled: true,
        effective_enabled: true,
        allowed: vec![ChangeKind::Disable],
    };
    let mut page = Catalog {
        host_epoch: epoch.clone(),
        principal: Principal::Local,
        root: Some(target().root),
        profiles: vec![],
        leaves: vec![leaf.clone()],
        next: Some(leaf.target.leaf.clone()),
    };
    let query = CatalogRequest {
        profile: Some("editable".into()),
        after: None,
    };
    page.validate(&query, &epoch).unwrap();
    page.leaves.push(leaf);
    assert!(page.validate(&query, &epoch).is_err());
    page.leaves.truncate(1);
    page.next = Some("different".into());
    assert!(page.validate(&query, &epoch).is_err());
    let grant = Grant {
        principal: Principal::Local,
        target: target(),
        operation: ChangeKind::Disable,
    };
    assert!(
        Grants {
            revision: "01".into(),
            scopes: vec![]
        }
        .validate()
        .is_err()
    );
    assert!(
        Grants {
            revision: "1".into(),
            scopes: vec![grant.clone(), grant]
        }
        .validate()
        .is_err()
    );
}

#[test]
fn largest_escaped_configuration_fits_the_actual_leaf_request_envelope() {
    let value = serde_json::json!("\\".repeat(32767));
    validate_configuration(&value).unwrap();
    assert_eq!(serde_json::to_vec(&value).unwrap().len(), 65536);
    let request = rsi_configuration_api::leaf::PreviewRequest {
        target: target(),
        change: Change::Configuration { value },
    };
    ByteBudget::default()
        .encode(&request, Operation::Preview.spec().maximum_request_bytes)
        .expect("the leaf API embeds a value, not its JSON string");
}
