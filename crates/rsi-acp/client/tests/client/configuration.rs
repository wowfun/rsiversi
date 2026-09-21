use super::*;
use rsi_acp_protocol::configuration::ConfigSelection;
use serde_json::Value;

fn selections() -> Vec<ConfigSelection> {
    vec![
        ConfigSelection {
            id: "model".into(),
            value: "fixture/selected".into(),
        },
        ConfigSelection {
            id: "effort".into(),
            value: "xhigh".into(),
        },
    ]
}

fn options(model: &str, effort: &str, grouped: bool) -> Value {
    let choices = json!([
        {"value":"fixture/default","name":"Default"},
        {"value":"fixture/selected","name":"Selected"}
    ]);
    json!([
        {"id":"model","name":"Model","type":"select","currentValue":model,
         "options":if grouped {json!([{"group":"provider","name":"Provider","options":choices}])}else{choices}},
        {"id":"effort","name":"Effort","type":"select","currentValue":effort,
         "options":[{"value":"low","name":"Low"},{"value":"xhigh","name":"Xhigh"}]}
    ])
}

async fn respond_setup(peer: &mut Peer, options: Value, load: bool) {
    let port = peer.handle();
    let Message::Request { id, method, .. } = peer.next().await.unwrap().message else {
        panic!("initialize")
    };
    assert_eq!(method, "initialize");
    port.respond(
        &id,
        Ok(&json!({"protocolVersion":1,"agentCapabilities":{"loadSession":true}})),
    )
    .await
    .unwrap();
    let Message::Request { id, method, .. } = peer.next().await.unwrap().message else {
        panic!("setup")
    };
    assert_eq!(method, if load { "session/load" } else { "session/new" });
    if load {
        port.notify("session/update", &json!({"sessionId":"remote","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"unpublished replay"}}})).await.unwrap();
    }
    let mut value = json!({"configOptions":options});
    if !load {
        value["sessionId"] = json!("remote");
    }
    port.respond(&id, Ok(&value)).await.unwrap();
}

#[tokio::test]
async fn setup_applies_advertised_options_in_order_and_waits_for_confirmation() {
    for grouped in [false, true] {
        let (_root, _journal, client, mut peer) = fixture(false).await;
        let handle = client.handle();
        let requested = selections();
        let remote = async {
            respond_setup(&mut peer, options("fixture/default", "low", grouped), false).await;
            let port = peer.handle();
            for (index, selection) in requested.iter().enumerate() {
                let Message::Request { id, method, params } = peer.next().await.unwrap().message
                else {
                    panic!("configuration")
                };
                assert_eq!(method, "session/set_config_option");
                assert_eq!(
                    params,
                    json!({"sessionId":"remote","configId":selection.id,"value":selection.value})
                );
                assert_eq!(handle.snapshot().status, Status::Starting);
                let current = options(
                    "fixture/selected",
                    if index == 0 { "low" } else { "xhigh" },
                    grouped,
                );
                port.notify("session/update", &json!({"sessionId":"remote","update":{"sessionUpdate":"config_option_update","configOptions":current}})).await.unwrap();
                port.respond(&id, Ok(&json!({"configOptions":current})))
                    .await
                    .unwrap();
            }
        };
        let (result, ()) = tokio::join!(client.initialize(Setup::New, vec![], &requested), remote);
        assert_eq!(result.unwrap().status, Status::Ready);
        client.close().await.unwrap();
        peer.close().await.unwrap();
    }
}

#[tokio::test]
async fn missing_or_unconfirmed_configuration_never_admits_a_prompt() {
    for scenario in [
        "absent",
        "unknown-choice",
        "duplicate",
        "too-many",
        "malformed",
        "unconfirmed",
        "reverted",
        "rejected",
    ] {
        let (_root, _journal, client, mut peer) = fixture(false).await;
        let requested = selections();
        let remote = async {
            let mut initial = options("fixture/default", "low", false);
            match scenario {
                "absent" => initial = json!([]),
                "unknown-choice" => initial[0]["options"] = json!([]),
                "duplicate" => {
                    let duplicate = initial[0].clone();
                    initial.as_array_mut().unwrap().push(duplicate);
                }
                "too-many" => {
                    initial[0]["options"] = json!(
                        (0..4097)
                            .map(|i| json!({"name":"choice","value":i.to_string()}))
                            .collect::<Vec<_>>()
                    );
                }
                _ => {}
            }
            respond_setup(&mut peer, initial, false).await;
            if matches!(
                scenario,
                "absent" | "unknown-choice" | "duplicate" | "too-many"
            ) {
                return;
            }
            let port = peer.handle();
            let Message::Request { id, method, .. } = peer.next().await.unwrap().message else {
                panic!("configuration")
            };
            assert_eq!(method, "session/set_config_option");
            if scenario == "rejected" {
                port.respond(
                    &id,
                    Err(&json!({"code":-32602,"message":"private peer error"})),
                )
                .await
                .unwrap();
                return;
            }
            let mut current = options(
                if scenario == "unconfirmed" {
                    "fixture/default"
                } else {
                    "fixture/selected"
                },
                "low",
                false,
            );
            if scenario == "malformed" {
                current[0]["options"] = json!([{"value":false,"name":"bad"}]);
            }
            port.respond(&id, Ok(&json!({"configOptions":current})))
                .await
                .unwrap();
            if scenario == "reverted" {
                let Message::Request { id, method, .. } = peer.next().await.unwrap().message else {
                    panic!("effort")
                };
                assert_eq!(method, "session/set_config_option");
                port.respond(
                    &id,
                    Ok(&json!({"configOptions":options("fixture/default","xhigh",false)})),
                )
                .await
                .unwrap();
            }
        };
        let (result, ()) = tokio::join!(client.initialize(Setup::New, vec![], &requested), remote);
        assert!(result.is_err(), "{scenario}");
        assert!(!client.handle().connected());
        assert!(
            client
                .handle()
                .submit(vec![rsi_acp_protocol::schema::ContentBlock::Text(
                    rsi_acp_protocol::schema::TextContent::new("must not send")
                )])
                .await
                .is_err()
        );
        client.close().await.unwrap();
        peer.close().await.unwrap();
    }
}

#[tokio::test]
async fn failed_load_configuration_retains_the_old_visible_epoch() {
    let (_root, journal, client, mut peer) = fixture(true).await;
    let original = client.handle().snapshot();
    let requested = selections();
    let remote = respond_setup(&mut peer, json!([]), true);
    let (result, ()) = tokio::join!(client.initialize(Setup::Load, vec![], &requested), remote);
    assert_eq!(result.unwrap_err(), rsi_acp_client::Error::Unsupported);
    let current = journal.get(&original.id).await.unwrap();
    assert_eq!(current.epoch, original.epoch);
    assert!(
        journal
            .page(&current.id, current.epoch, 0)
            .await
            .unwrap()
            .records
            .is_empty()
    );
    client.close().await.unwrap();
    peer.close().await.unwrap();
}

#[tokio::test]
async fn configuration_timeout_fails_setup_without_ready_or_prompt() {
    let (_root, _journal, client, mut peer) = fixture(false).await;
    let requested = selections();
    let remote = async {
        respond_setup(&mut peer, options("fixture/default", "low", false), false).await;
        let Message::Request { method, .. } = peer.next().await.unwrap().message else {
            panic!("configuration")
        };
        assert_eq!(method, "session/set_config_option");
        tokio::time::pause();
    };
    let (result, ()) = tokio::join!(client.initialize(Setup::New, vec![], &requested), remote);
    assert_eq!(result.unwrap_err(), rsi_acp_client::Error::Unknown);
    assert!(!client.handle().connected());
    client.close().await.unwrap();
    peer.close().await.unwrap();
}
