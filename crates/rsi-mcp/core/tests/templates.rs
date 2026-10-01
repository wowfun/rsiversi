#[allow(dead_code)] // Shared HTTP fixture also serves cancellation and stream tests.
mod support;
use rsi_mcp::{McpError, TemplateCatalog, TemplateParameters};
use serde_json::json;
use std::sync::{Arc, atomic::Ordering};
use support::*;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn templates_are_opt_in_frozen_and_expand_only_before_admitted_reads() {
    let fixture = HttpFixture::start(Mode { templates: Some(json!({"resourceTemplates":[{"uriTemplate":"fixture:{+path}{?q}","name":"Files","_meta":{"title":"complete"}}]})), templates_only: true, ..Mode::default() }).await;
    let service = fixture.service(Arc::new(Credentials::default()));
    service.configure(fixture.config()).await.unwrap();
    let disabled = service
        .refresh("fixture", None, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(disabled.templates, TemplateCatalog::Disabled);
    let parameters =
        TemplateParameters::from_value(json!({"path":"/outside/project","q":"中文"})).unwrap();
    assert_eq!(
        service
            .template_resource(&disabled, 0, &parameters, None, CancellationToken::new())
            .await
            .unwrap_err(),
        McpError::NotFound
    );
    let mut config = fixture.config();
    config.servers[0].resource_templates = true;
    service.configure(config).await.unwrap();
    let frozen = service
        .refresh("fixture", None, CancellationToken::new())
        .await
        .unwrap();
    assert!(frozen.resources.is_empty());
    assert_ne!(frozen.target_sha256, disabled.target_sha256);
    assert_eq!(
        frozen.templates.entries()[0].extensions["_meta"]["title"],
        "complete"
    );
    let (uri, response) = service
        .template_resource(&frozen, 0, &parameters, None, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(uri, "fixture:/outside/project?q=%E4%B8%AD%E6%96%87");
    assert_eq!(response["contents"][0]["uri"], uri);
    let before = fixture.credentials_seen.load(Ordering::Acquire);
    for parameters in [json!({"unknown":"x"}), json!({"q":"é".repeat(1000)})] {
        assert_eq!(
            service
                .template_resource(
                    &frozen,
                    0,
                    &TemplateParameters::from_value(parameters).unwrap(),
                    None,
                    CancellationToken::new()
                )
                .await
                .unwrap_err(),
            McpError::Protocol
        );
    }
    assert_eq!(
        fixture.credentials_seen.load(Ordering::Acquire),
        before,
        "rejected parameters cause zero RPCs"
    );
    fixture.mode.lock().unwrap().templates =
        Some(json!({"resourceTemplates":[{"uriTemplate":"fixture:{bad","name":"broken"}]}));
    assert_eq!(
        service
            .refresh("fixture", None, CancellationToken::new())
            .await
            .unwrap_err(),
        McpError::Protocol
    );
    assert_eq!(
        service.status()[0].last_verified_sha256,
        Some(frozen.sha256().to_owned())
    );
    assert!(!service.status()[0].ready);
    service.shutdown().await.unwrap();
    fixture.shutdown().await;
}

#[tokio::test]
async fn only_method_not_found_means_unsupported_and_does_not_poison_connection() {
    for (templates, error, expected) in [
        (None, None, Some(TemplateCatalog::Unsupported)),
        (
            Some(json!({"resourceTemplates":[]})),
            None,
            Some(TemplateCatalog::Available { templates: vec![] }),
        ),
        (None, Some(-32603), None),
    ] {
        let fixture = HttpFixture::start(Mode {
            templates,
            template_error: error,
            ..Mode::default()
        })
        .await;
        let service = fixture.service(Arc::new(Credentials::default()));
        let mut config = fixture.config();
        config.servers[0].resource_templates = true;
        service.configure(config).await.unwrap();
        let refreshed = service
            .refresh("fixture", None, CancellationToken::new())
            .await;
        if let Some(expected) = expected {
            let frozen = refreshed.unwrap();
            assert_eq!(frozen.templates, expected);
            service
                .resource(&frozen, "fixture://text", None, CancellationToken::new())
                .await
                .unwrap();
        } else {
            assert_eq!(refreshed.unwrap_err(), McpError::RemoteError);
        }
        service.shutdown().await.unwrap();
        fixture.shutdown().await;
    }
}
