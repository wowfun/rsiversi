use super::*;
use rsi_ui::{ActionInput, PresentationAction, UiElement};
use rsi_ui_api::{ExportScope, Invoke, Observe, Selection, UiClient, UiItem, UiObservation};
use std::{
    io::BufRead as _,
    process::{Command, Stdio},
};

async fn next(observation: &mut UiObservation) -> UiItem {
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        let mut latest = None;
        loop {
            let incoming = if latest.is_some() {
                match tokio::time::timeout(
                    std::time::Duration::from_millis(200),
                    observation.next(),
                )
                .await
                {
                    Ok(result) => result,
                    Err(_) => return latest.unwrap(),
                }
            } else {
                observation.next().await
            };
            let item = incoming.unwrap().expect("browser model");
            latest = if item.item.ticket.is_some() {
                Some(item)
            } else {
                None
            };
        }
    })
    .await
    .unwrap()
}
async fn invoke(
    client: &UiClient,
    item: &UiItem,
    label: &str,
    url: &str,
    node: &str,
    text: &str,
    value: Option<serde_json::Value>,
) {
    invoke_for(
        client,
        item,
        "browser-proof",
        label,
        [url, node, text],
        value,
    )
    .await;
}
async fn invoke_for(
    client: &UiClient,
    item: &UiItem,
    application: &str,
    label: &str,
    fields: [&str; 3],
    value: Option<serde_json::Value>,
) {
    let [url, node, text] = fields;
    let action = item
        .item
        .snapshot
        .model
        .standard_view
        .as_ref()
        .unwrap()
        .elements
        .iter()
        .find_map(|element| match element {
            UiElement::Button {
                label: caption,
                value,
                ..
            } if caption == label => Some(value.clone()),
            _ => None,
        })
        .expect("browser action");
    client
        .invoke(&Invoke {
            application: application.into(),
            action: PresentationAction {
                presentation: item.item.snapshot.presentation.clone(),
                revision: item.item.snapshot.revision,
                action: "operate".into(),
            },
            ticket: item.item.ticket.clone().unwrap(),
            input: ActionInput {
                value: value.unwrap_or(action),
                fields: BTreeMap::from([
                    ("url".into(), url.into()),
                    ("node".into(), node.into()),
                    ("text".into(), text.into()),
                ]),
            },
        })
        .await
        .unwrap_or_else(|error| panic!("{label}: {error}"));
}
fn node(item: &UiItem, name: &str) -> String {
    let code = item
        .item
        .snapshot
        .model
        .standard_view
        .as_ref()
        .unwrap()
        .elements
        .iter()
        .find_map(|e| match e {
            UiElement::Code { text } => Some(text),
            _ => None,
        })
        .unwrap();
    let snapshot: serde_json::Value = serde_json::from_str(code).unwrap();
    snapshot["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["name"] == name)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .into()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit Linux confined Session browser through production UI mutation API"]
async fn session_browser_real_http_ws_nodes_images_and_presentation_detach() {
    struct Server(std::process::Child);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let node_path = std::env::var("RSI_TEST_BROWSER_NODE").unwrap();
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../browser/tests/fixtures/session.mjs");
    let mut server = Server(
        Command::new(&node_path)
            .arg(script)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut port = String::new();
    std::io::BufReader::new(server.0.stdout.take().unwrap())
        .read_line(&mut port)
        .unwrap();
    let origin = format!("http://127.0.0.1:{}", port.trim());
    let (endpoint, provider) = provider().await;
    let fixture = fixture(&endpoint);
    let mut config = rsi_browser::RuntimeConfig {
        node: node_path.into(),
        chromium_directory: std::env::var("RSI_TEST_BROWSER_CHROMIUM").unwrap().into(),
        package_directory: std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../browser/runtime")
            .canonicalize()
            .unwrap(),
        systemd_run: "/usr/bin/systemd-run".into(),
        user_runtime_directory: std::env::var("RSI_TEST_BROWSER_USER_RUNTIME")
            .unwrap()
            .into(),
        artifact_digest: "0".repeat(64),
    };
    config.artifact_digest = config.digest().unwrap();
    if let Ok(directory) = std::env::var("RSI_EVIDENCE_DIR") {
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            std::path::Path::new(&directory).join("browser-runtime.json"),
            serde_json::to_vec_pretty(&config).unwrap(),
        )
        .unwrap();
    }
    let mut document: toml::Value =
        toml::from_str(&std::fs::read_to_string(&fixture.profile).unwrap()).unwrap();
    for step in [
        serde_json::json!({"kind":"patch","target":"session-browser","config":{"runtime":config}}),
        serde_json::json!({"kind":"patch","target":"session-browser","enabled":true}),
        serde_json::json!({"kind":"patch","target":"session-browser-ui","enabled":true}),
    ] {
        document["steps"]
            .as_array_mut()
            .unwrap()
            .push(toml::Value::try_from(step).unwrap());
    }
    std::fs::write(&fixture.profile, toml::to_string(&document).unwrap()).unwrap();
    let daemon = DaemonFixture::new(&fixture).await;
    let service = daemon.running.session_service().unwrap();
    let workspace = daemon
        .running
        .workspace_registry()
        .unwrap()
        .get_or_create(&fixture.workspace)
        .await
        .unwrap();
    let session = service
        .create(CreateSession {
            workspace_id: workspace.id.clone(),
            session_id: SessionId::new("browser-proof-session").unwrap(),
            agent_preset_id: None,
        })
        .await
        .unwrap();
    let client = UiClient::new(daemon.connection.api_client()).unwrap();
    let observe = Observe {
        application: "browser-proof".into(),
        selections: vec![Selection {
            scope: ExportScope {
                kind: "session".into(),
                key: session.header().await.unwrap().session_id().to_string(),
            },
            bundle: "rsi.browser".into(),
            surface: "session".into(),
        }],
    };
    let mut stream = client.observe(&observe).await.unwrap();
    let item = next(&mut stream).await;
    invoke(
        &client,
        &item,
        "Approve and open this exact local origin",
        &origin,
        "",
        "",
        None,
    )
    .await;
    let item = next(&mut stream).await;
    assert!(
        format!("{:?}", item.item.snapshot.model.standard_view).contains("Shared Session browser")
    );
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    invoke(&client, &item, "Observe page", "", "", "", None).await;
    let item = next(&mut stream).await;
    assert!(
        format!("{:?}", item.item.snapshot.model.standard_view).contains("WS verified"),
        "real same-origin WebSocket traversed the confined proxy: {:?}",
        item.item.snapshot.model.standard_view
    );
    let input = node(&item, "Name");
    let stale = item
        .item
        .snapshot
        .model
        .standard_view
        .as_ref()
        .unwrap()
        .elements
        .iter()
        .find_map(|e| match e {
            UiElement::Button { label, value, .. } if label == "Click node" => Some(value.clone()),
            _ => None,
        })
        .unwrap();
    invoke(&client, &item, "Fill node", "", &input, "Ada", None).await;
    let item = next(&mut stream).await;
    invoke(
        &client,
        &item,
        "Click node",
        "",
        &node(&item, "Apply"),
        "",
        Some(stale),
    )
    .await;
    let item = next(&mut stream).await;
    assert!(format!("{:?}", item.item.snapshot.model.standard_view).contains("stale_observation"));
    invoke(
        &client,
        &item,
        "Click node",
        "",
        &node(&item, "Apply"),
        "",
        None,
    )
    .await;
    let item = next(&mut stream).await;
    assert!(format!("{:?}", item.item.snapshot.model.standard_view).contains("Hello Ada"));
    invoke(
        &client,
        &item,
        "Click node",
        "",
        &node(&item, "Replace node"),
        "",
        None,
    )
    .await;
    let item = next(&mut stream).await;
    invoke(
        &client,
        &item,
        "Click node",
        "",
        &node(&item, "Apply"),
        "",
        None,
    )
    .await;
    let item = next(&mut stream).await;
    assert!(
        format!("{:?}", item.item.snapshot.model.standard_view)
            .contains("node_changed_or_not_actionable"),
        "a detached ElementHandle never retargets its replacement"
    );
    invoke(&client, &item, "Capture screenshot", "", "", "", None).await;
    let item = next(&mut stream).await;
    let image = &item.item.snapshot.model.data["image"];
    assert_eq!(image["width"], 1280);
    assert_eq!(image["height"], 720);
    let mut png = Vec::new();
    let length = image["bytes"].as_u64().unwrap() as usize;
    while png.len() < length {
        let bytes = client
            .source(&rsi_ui_api::Source {
                application: "browser-proof".into(),
                presentation: item.item.snapshot.presentation.clone(),
                revision: item.item.snapshot.revision,
                name: image["source"].as_str().unwrap().into(),
                offset: png.len() as u64,
                maximum: (length - png.len()).min(65536),
            })
            .await
            .unwrap();
        assert!(!bytes.is_empty());
        png.extend_from_slice(bytes.as_ref());
    }
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    if let Ok(directory) = std::env::var("RSI_EVIDENCE_DIR") {
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            std::path::Path::new(&directory).join("native-session-browser.png"),
            &png,
        )
        .unwrap();
    }
    drop(stream);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let mut stream = client.observe(&observe).await.unwrap();
    let item = next(&mut stream).await;
    assert!(
        format!("{:?}", item.item.snapshot.model.standard_view).contains("Hello Ada"),
        "presentation detach retains browser"
    );
    for (number, capacity) in [(2, false), (3, true)] {
        let another = service
            .create(CreateSession {
                workspace_id: workspace.id.clone(),
                session_id: SessionId::new(format!("browser-proof-{number}")).unwrap(),
                agent_preset_id: None,
            })
            .await
            .unwrap();
        let application = format!("browser-proof-{number}");
        let observe = Observe {
            application: application.clone(),
            selections: vec![Selection {
                scope: ExportScope {
                    kind: "session".into(),
                    key: another.header().await.unwrap().session_id().to_string(),
                },
                bundle: "rsi.browser".into(),
                surface: "session".into(),
            }],
        };
        let mut other = client.observe(&observe).await.unwrap();
        let opened = next(&mut other).await;
        invoke_for(
            &client,
            &opened,
            &application,
            "Approve and open this exact local origin",
            [&origin, "", ""],
            None,
        )
        .await;
        let opened = next(&mut other).await;
        if capacity {
            assert!(format!("{:?}", opened.item.snapshot.model.standard_view).contains("capacity"));
        } else {
            assert!(
                format!("{:?}", opened.item.snapshot.model.standard_view)
                    .contains("Shared Session browser")
            );
        }
        // Keep the second browser owned after its panel detaches; it fills the
        // shared runtime's second slot when the third Session attempts to open.
        drop(other);
    }
    invoke(
        &client,
        &item,
        "Navigate",
        &format!("{origin}/unicode"),
        "",
        "",
        None,
    )
    .await;
    let item = next(&mut stream).await;
    let text = format!("{:?}", item.item.snapshot.model.standard_view);
    assert!(
        text.contains("Unicode �") && text.contains("Scalar �"),
        "hostile UTF-16 must survive the real CDP and Rust JSON boundaries: {text}"
    );
    assert!(
        !text.contains("signature"),
        "private node semantics must stay private"
    );
    invoke(&client, &item, "Capture screenshot", "", "", "", None).await;
    let item = next(&mut stream).await;
    assert_eq!(item.item.snapshot.model.data["image"]["width"], 1280);
    invoke(&client, &item, "Close browser", "", "", "", None).await;
    let item = next(&mut stream).await;
    assert!(format!("{:?}", item.item.snapshot.model.standard_view).contains("closed"));
    drop((stream, client, session, service));
    daemon.shutdown().await;
    provider.abort();
}
