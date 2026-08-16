use std::fs;
use std::path::{Path, PathBuf};

use rsi_meta::{CompositionLock, CompositionManifest};
use rsi_meta_cli::protocol::CommandOutcomeEnvelope;
use rsi_meta_frame_contract::{Frame, LifecyclePhase};
use rsi_meta_loader::{ApiVersion, PluginLoader, PluginPackage, prepare_config};
use serde_json::{Value, json};

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("conformance package is nested three levels below repository")
        .to_path_buf()
}

fn validator(name: &str) -> jsonschema::Validator {
    let path = repository().join("schemas/rsi-meta").join(name);
    let schema: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap_or_else(|error| panic!("{} is not a valid schema: {error}", path.display()))
}

fn assert_valid(validator: &jsonschema::Validator, value: &Value) {
    if !validator.is_valid(value) {
        let errors = validator
            .iter_errors(value)
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        panic!("expected fixture to be valid:\n{value:#}\n{errors}");
    }
}

fn assert_invalid(validator: &jsonschema::Validator, value: &Value) {
    assert!(
        !validator.is_valid(value),
        "expected fixture to be rejected:\n{value:#}"
    );
}

#[test]
fn golden_composition_resolves_and_prepares_every_package() {
    let manifest_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("golden/composition.toml");
    let manifest: CompositionManifest =
        toml::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
    let report = manifest.validate();
    assert!(report.is_valid(), "{:#?}", report.diagnostics);

    let base = manifest_path.parent().unwrap();
    for instance in manifest.instances {
        let package = PluginPackage::open(base.join(&instance.package)).unwrap_or_else(|error| {
            panic!(
                "golden instance {} cannot open {}: {error}",
                instance.id,
                base.join(&instance.package).display()
            )
        });
        prepare_config(&package, instance.config).unwrap_or_else(|error| {
            panic!(
                "golden instance {} has invalid config: {error}",
                instance.id
            )
        });
    }
}

#[test]
fn published_protocol_document_covers_observable_wire_contracts() {
    let document =
        fs::read_to_string(repository().join("crates/rsi-meta/docs/subsystems/protocols.md"))
            .unwrap();
    for required_fragment in [
        "/ws?after=N",
        "code=bearer_token_rotated",
        "code=event_stream_interrupted",
        "last_cursor=N",
        r#"{"provider": "provider-instance-id"}"#,
        "apply_manifest_path",
        "query_graph",
        "query_events",
        "inspect_plugin",
        "rotate_token",
        "shutdown",
        "connection-local correlation",
        "durable `OperationId`",
        "expected_graph_revision",
        "restart_required",
        "optional `operation_id`",
        "1,000",
        "10,000",
    ] {
        assert!(
            document.contains(required_fragment),
            "control protocol omits observable wire contract {required_fragment:?}"
        );
    }
}

#[test]
fn published_schemas_match_v0_decoders_and_runtime_validation() {
    let graph = json!({
        "protocol": "rsi-meta.control",
        "version": 0,
        "kind": "result",
        "command_id": "graph",
        "graph_revision": 0,
        "payload": {
            "type": "graph",
            "graph": {
                "revision": 0,
                "composition_id": "legacy",
                "instances": {},
                "bindings": []
            },
            "cursor": 0
        }
    });
    let decoded: CommandOutcomeEnvelope = serde_json::from_value(graph.clone()).unwrap();
    assert_eq!(
        decoded.payload,
        serde_json::from_value(json!({
            "type": "graph",
            "graph": {
                "revision": 0,
                "composition_id": "legacy",
                "instances": {},
                "bindings": []
            },
            "cursor": 0
        }))
        .unwrap()
    );
    assert_valid(&validator("control-envelope.schema.json"), &graph);

    let legacy_lock = json!({
        "format_version": 0,
        "target": "test-target",
        "manifest_sha256": "0".repeat(64)
    });
    let decoded: CompositionLock = serde_json::from_value(legacy_lock.clone()).unwrap();
    assert!(decoded.packages.is_empty());
    assert_valid(&validator("lock.schema.json"), &legacy_lock);

    let frame_validator = validator("plugin-frame.schema.json");
    for cross_kind_frame in [
        json!({
            "protocol": "rsi-meta.plugin", "version": 0, "kind": "lifecycle",
            "phase": "prepared", "generation": 1,
            "service": "fixture.echo", "event": "end", "payload": null
        }),
        json!({
            "protocol": "rsi-meta.plugin", "version": 0, "kind": "service_request",
            "request_id": "request-1", "service": "fixture.echo",
            "operation": "open", "payload": {}, "generation": 1
        }),
        json!({
            "protocol": "rsi-meta.plugin", "version": 0, "kind": "service_event",
            "service": "fixture.echo", "event": "end", "payload": null,
            "operation": "half_close"
        }),
        json!({
            "protocol": "rsi-meta.plugin", "version": 0, "kind": "durable_command",
            "command_id": "command-1", "command": {"type": "shutdown"},
            "payload": null
        }),
    ] {
        assert_invalid(&frame_validator, &cross_kind_frame);
    }

    let duplicate_injection = json!({
        "format_version": 0,
        "package": {"id": "duplicate.inject", "version": "1.0.0"},
        "host_api": {"major": 0, "minimum_minor": 0},
        "artifacts": [{"target": "test-target", "path": "plugin.so"}],
        "injects": [
            {"contract": "fixture.echo", "required": true},
            {"contract": "fixture.echo", "required": true}
        ]
    });
    assert_invalid(&validator("plugin.schema.json"), &duplicate_injection);

    let temporary = tempfile::tempdir().unwrap();
    let manifest_path = temporary.path().join("plugin.toml");
    let manifest_toml = toml::to_string(&duplicate_injection).unwrap();
    fs::write(&manifest_path, manifest_toml).unwrap();
    let package = PluginPackage::open(&manifest_path).unwrap();
    let loader = PluginLoader::new(
        temporary.path().join("cache"),
        "test-target",
        ApiVersion::CURRENT,
    );
    assert!(loader.validate_manifest(package.manifest()).is_err());
}

#[test]
fn control_envelope_has_kind_specific_required_fields() {
    let validator = validator("control-envelope.schema.json");
    let command = json!({
        "protocol": "rsi-meta.control",
        "version": 0,
        "kind": "command",
        "command_id": "cmd-1",
        "expected_graph_revision": 2,
        "payload": {
            "type": "apply_manifest_path",
            "manifest_path": "/candidate/composition.toml",
            "lock_path": "/candidate/rsi-meta.lock"
        }
    });
    let result = json!({
        "protocol": "rsi-meta.control",
        "version": 0,
        "kind": "result",
        "command_id": "cmd-1",
        "graph_revision": 3,
        "payload": {"type": "graph", "graph": {}, "cursor": 8}
    });
    let event = json!({
        "protocol": "rsi-meta.control",
        "version": 0,
        "kind": "event",
        "cursor": 8,
        "graph_revision": 3,
        "payload": {"type": "composition_committed"}
    });
    for valid in [&command, &result, &event] {
        assert_valid(&validator, valid);
    }

    let mut missing_command_id = command.clone();
    missing_command_id
        .as_object_mut()
        .unwrap()
        .remove("command_id");
    assert_invalid(&validator, &missing_command_id);
    let mut negative_expected_revision = command.clone();
    negative_expected_revision["expected_graph_revision"] = json!(-1);
    assert_invalid(&validator, &negative_expected_revision);
    let mut read_with_expected_revision = command.clone();
    read_with_expected_revision["payload"] = json!({"type": "query_graph"});
    assert_invalid(&validator, &read_with_expected_revision);
    let mut null_payload = command.clone();
    null_payload["payload"] = Value::Null;
    assert_invalid(&validator, &null_payload);
    let mut missing_payload_type = command.clone();
    missing_payload_type
        .as_object_mut()
        .unwrap()
        .remove("expected_graph_revision");
    missing_payload_type["payload"] = json!({"manifest_path": "m", "lock_path": "l"});
    assert_invalid(&validator, &missing_payload_type);
    let mut command_with_operation_id = command.clone();
    command_with_operation_id["operation_id"] = json!("apply-1");
    assert_invalid(&validator, &command_with_operation_id);
    let mut oversized_command_id = command.clone();
    oversized_command_id["command_id"] = json!("x".repeat(256));
    assert_invalid(&validator, &oversized_command_id);
    let beyond_u64: Value = serde_json::from_str("18446744073709551616").unwrap();
    let mut oversized_expected_revision = command.clone();
    oversized_expected_revision["expected_graph_revision"] = beyond_u64.clone();
    assert_invalid(&validator, &oversized_expected_revision);
    let mut missing_result_revision = result.clone();
    missing_result_revision
        .as_object_mut()
        .unwrap()
        .remove("graph_revision");
    assert_invalid(&validator, &missing_result_revision);
    let mut graph_without_cursor = result.clone();
    graph_without_cursor["payload"]
        .as_object_mut()
        .unwrap()
        .remove("cursor");
    assert_invalid(&validator, &graph_without_cursor);
    let mut event_with_operation_id = event.clone();
    event_with_operation_id["operation_id"] = json!("apply-1");
    assert_valid(&validator, &event_with_operation_id);
    let mut event_with_command_id = event.clone();
    event_with_command_id["command_id"] = json!("legacy-event");
    assert_invalid(&validator, &event_with_command_id);
    let mut missing_event_cursor = event;
    missing_event_cursor
        .as_object_mut()
        .unwrap()
        .remove("cursor");
    assert_invalid(&validator, &missing_event_cursor);
    let mut oversized_event_cursor = result;
    oversized_event_cursor["graph_revision"] = beyond_u64;
    assert_invalid(&validator, &oversized_event_cursor);
}

#[test]
fn stream_envelope_requires_sequence_and_credit_by_kind() {
    let validator = validator("stream-envelope.schema.json");
    let open = json!({
        "protocol": "rsi-meta.stream",
        "version": 0,
        "kind": "open",
        "stream_id": "stream-1",
        "payload": {"consumer": "consumer", "service": "fixture.echo"}
    });
    let data = json!({
        "protocol": "rsi-meta.stream",
        "version": 0,
        "kind": "data",
        "stream_id": "stream-1",
        "sequence": 1,
        "payload": [104, 101, 108, 108, 111]
    });
    let credit = json!({
        "protocol": "rsi-meta.stream",
        "version": 0,
        "kind": "credit",
        "stream_id": "stream-1",
        "credit_bytes": 5
    });
    for valid in [&open, &data, &credit] {
        assert_valid(&validator, valid);
    }

    let mut missing_sequence = data.clone();
    missing_sequence.as_object_mut().unwrap().remove("sequence");
    assert_invalid(&validator, &missing_sequence);
    let mut zero_sequence = data.clone();
    zero_sequence["sequence"] = json!(0);
    assert_invalid(&validator, &zero_sequence);
    let mut base64_payload = data.clone();
    base64_payload["payload"] = json!("aGVsbG8=");
    assert_invalid(&validator, &base64_payload);
    let mut non_byte_payload = data;
    non_byte_payload["payload"] = json!([104, 256]);
    assert_invalid(&validator, &non_byte_payload);
    let mut missing_credit = credit;
    missing_credit
        .as_object_mut()
        .unwrap()
        .remove("credit_bytes");
    assert_invalid(&validator, &missing_credit);
    let beyond_u64: Value = serde_json::from_str("18446744073709551616").unwrap();
    let mut oversized_sequence = open.clone();
    oversized_sequence["kind"] = json!("data");
    oversized_sequence["sequence"] = beyond_u64.clone();
    oversized_sequence["payload"] = json!([1]);
    assert_invalid(&validator, &oversized_sequence);
    let mut oversized_credit = open.clone();
    oversized_credit["kind"] = json!("credit");
    oversized_credit["credit_bytes"] = beyond_u64;
    oversized_credit.as_object_mut().unwrap().remove("payload");
    assert_invalid(&validator, &oversized_credit);
    let mut oversized_stream_id = open;
    oversized_stream_id["stream_id"] = json!("x".repeat(256));
    assert_invalid(&validator, &oversized_stream_id);
}

#[test]
fn plugin_frame_schema_matches_rust_numeric_and_durable_command_shapes() {
    let validator = validator("plugin-frame.schema.json");
    let valid = json!({
        "protocol": "rsi-meta.plugin",
        "version": 0,
        "kind": "durable_command",
        "command_id": "apply-1",
        "command": {
            "type": "apply_manifest_path",
            "manifest_path": "/candidate/rsi-meta.toml",
            "lock_path": "/candidate/rsi-meta.lock"
        }
    });
    assert_valid(&validator, &valid);

    let mut unsupported = valid;
    unsupported["command"] = json!({"type": "shutdown"});
    assert_invalid(&validator, &unsupported);

    let beyond_u64: Value = serde_json::from_str("18446744073709551616").unwrap();
    let lifecycle = json!({
        "protocol": "rsi-meta.plugin",
        "version": 0,
        "kind": "lifecycle",
        "phase": "prepared",
        "generation": beyond_u64
    });
    assert_invalid(&validator, &lifecycle);
}

#[test]
fn every_published_plugin_manifest_matches_the_frozen_schema() {
    let validator = validator("plugin.schema.json");
    for relative in [
        "plugins/rsi-meta/fs-watch-native/plugin.toml",
        "plugins/rsi-meta/fs-watch-polling/plugin.toml",
        "plugins/rsi-meta/hmr-consumer/plugin.toml",
        "fixtures/rsi-meta/echo-bidi/plugin.toml",
        "fixtures/rsi-meta/nested-scope-consumer/plugin.toml",
        "fixtures/rsi-meta/cas-counter/plugin.toml",
        "fixtures/rsi-meta/lifecycle-probe/plugin.toml",
    ] {
        let source = fs::read_to_string(repository().join(relative)).unwrap();
        let value: toml::Value = toml::from_str(&source).unwrap();
        let value = serde_json::to_value(value).unwrap();
        assert_valid(&validator, &value);
    }

    let oversized_id = json!({
        "format_version": 0,
        "package": {"id": "a".repeat(256), "version": "1.0.0"},
        "host_api": {"major": 0, "minimum_minor": 0},
        "artifacts": [{"target": "test-target", "path": "plugin.so"}]
    });
    assert_invalid(&validator, &oversized_id);

    let oversized_contract = json!({
        "format_version": 0,
        "package": {"id": "valid.package", "version": "1.0.0"},
        "host_api": {"major": 0, "minimum_minor": 0},
        "artifacts": [{"target": "test-target", "path": "plugin.so"}],
        "provides": ["s".repeat(256)]
    });
    assert_invalid(&validator, &oversized_contract);
}

#[test]
fn composition_schema_bounds_identity_and_service_fields() {
    let schema = validator("composition.schema.json");
    let invalid = json!({
        "format_version": 0,
        "composition": {"id": "contains space", "mode": "development"},
        "scopes": [{"id": "root"}],
        "instances": [{
            "id": "i".repeat(256),
            "package": "provider/plugin.toml",
            "scope": "root",
            "bindings": {"service with space": "provider"}
        }]
    });
    assert_invalid(&schema, &invalid);
}

#[test]
fn official_stream_fixtures_require_runtime_tick_for_backpressure_progress() {
    for relative in [
        "fixtures/rsi-meta/echo-bidi/plugin.toml",
        "fixtures/rsi-meta/nested-scope-consumer/plugin.toml",
        "fixtures/rsi-meta/cas-counter/plugin.toml",
        "fixtures/rsi-meta/lifecycle-probe/plugin.toml",
    ] {
        let source = fs::read_to_string(repository().join(relative)).unwrap();
        let manifest: toml::Value = toml::from_str(&source).unwrap();
        let has_required_tick = manifest
            .get("injects")
            .and_then(toml::Value::as_array)
            .is_some_and(|injects| {
                injects.iter().any(|inject| {
                    inject.get("contract").and_then(toml::Value::as_str) == Some("runtime.tick")
                        && inject.get("required").and_then(toml::Value::as_bool) == Some(true)
                })
            });
        assert!(
            has_required_tick,
            "{relative} must declare required runtime.tick progress"
        );
    }
}

#[test]
fn composition_lock_and_plugin_frame_goldens_match_their_schemas() {
    let cases = [
        (
            "composition.schema.json",
            include_str!("../golden/composition.toml"),
            true,
        ),
        (
            "composition.schema.json",
            include_str!("../../../../examples/rsi-meta/echo/rsi-meta.toml"),
            true,
        ),
        (
            "plugin-frame.schema.json",
            include_str!("../golden/plugin-prepare.json"),
            false,
        ),
        (
            "plugin-frame.schema.json",
            include_str!("../golden/plugin-prepared.json"),
            false,
        ),
        (
            "plugin-frame.schema.json",
            include_str!("../golden/plugin-prepare-failed.json"),
            false,
        ),
    ];
    for (schema, source, is_toml) in cases {
        let value = if is_toml {
            let value: toml::Value = toml::from_str(source).unwrap();
            serde_json::to_value(value).unwrap()
        } else {
            serde_json::from_str(source).unwrap()
        };
        assert_valid(&validator(schema), &value);
    }

    let generated_lock_shape = serde_json::to_value(CompositionLock {
        format_version: 0,
        target: rsi_meta_loader::BUILD_TARGET.to_owned(),
        manifest_sha256: rsi_meta_loader::ContentHash::digest(b"fixture composition"),
        packages: Vec::new(),
    })
    .unwrap();
    assert_valid(&validator("lock.schema.json"), &generated_lock_shape);
}

#[test]
fn hmr_lifecycle_goldens_are_exact_frame_contract_examples() {
    let validator = validator("plugin-frame.schema.json");
    let success = parse_json_lines(include_str!("../golden/hmr-success.jsonl"));
    let abort = parse_json_lines(include_str!("../golden/hmr-abort.jsonl"));
    for frame in success.iter().chain(&abort) {
        assert_valid(&validator, frame);
    }

    assert_eq!(
        phases(&success),
        ["prepare", "prepared", "committed", "retire", "retired"]
    );
    assert_eq!(generations(&success), [2, 2, 2, 1, 1]);
    assert_eq!(phases(&abort), ["prepare", "prepared", "abort"]);
    assert_eq!(generations(&abort), [2, 2, 2]);

    let expected_success = [
        Frame::lifecycle(LifecyclePhase::Prepare, 2, Some(json!({}))),
        Frame::lifecycle(LifecyclePhase::Prepared, 2, None),
        Frame::lifecycle(LifecyclePhase::Committed, 2, None),
        Frame::lifecycle(LifecyclePhase::Retire, 1, None),
        Frame::lifecycle(LifecyclePhase::Retired, 1, None),
    ]
    .map(|frame| serde_json::to_value(frame).unwrap());
    let expected_abort = [
        Frame::lifecycle(LifecyclePhase::Prepare, 2, Some(json!({}))),
        Frame::lifecycle(LifecyclePhase::Prepared, 2, None),
        Frame::lifecycle(LifecyclePhase::Abort, 2, None),
    ]
    .map(|frame| serde_json::to_value(frame).unwrap());
    assert_eq!(success, expected_success);
    assert_eq!(abort, expected_abort);
}

#[test]
fn prepare_frame_goldens_are_exact_frame_contract_examples() {
    let cases = [
        (
            include_str!("../golden/plugin-prepare.json"),
            Frame::lifecycle(
                LifecyclePhase::Prepare,
                7,
                Some(json!({"path": "/workspace"})),
            ),
        ),
        (
            include_str!("../golden/plugin-prepared.json"),
            Frame::lifecycle(LifecyclePhase::Prepared, 7, None),
        ),
        (
            include_str!("../golden/plugin-prepare-failed.json"),
            Frame::lifecycle(
                LifecyclePhase::PrepareFailed,
                7,
                Some(json!({
                    "code": "state_read_failed",
                    "message": "state.cas returned conflict",
                })),
            ),
        ),
    ];
    for (source, expected) in cases {
        let actual: Value = serde_json::from_str(source).unwrap();
        assert_eq!(actual, serde_json::to_value(expected).unwrap());
    }
}

#[test]
fn prepare_acknowledgements_have_exact_safe_payloads() {
    let validator = validator("plugin-frame.schema.json");
    let prepared = json!({
        "protocol": "rsi-meta.plugin",
        "version": 0,
        "kind": "lifecycle",
        "phase": "prepared",
        "generation": 8,
    });
    let failed_without_message = json!({
        "protocol": "rsi-meta.plugin",
        "version": 0,
        "kind": "lifecycle",
        "phase": "prepare_failed",
        "generation": 8,
        "config": {"code": "state_read_failed"},
    });
    assert_valid(&validator, &prepared);
    assert_valid(&validator, &failed_without_message);

    let mut prepared_with_payload = prepared;
    prepared_with_payload["config"] = json!({"secret": "must-not-cross"});
    assert_invalid(&validator, &prepared_with_payload);

    for config in [
        json!(null),
        json!({}),
        json!({"code": "bad code"}),
        json!({"code": "safe", "message": "line\nbreak"}),
        json!({"code": "safe", "message": "x".repeat(257)}),
        json!({"code": "safe", "extra": true}),
    ] {
        let failed = json!({
            "protocol": "rsi-meta.plugin",
            "version": 0,
            "kind": "lifecycle",
            "phase": "prepare_failed",
            "generation": 8,
            "config": config,
        });
        assert_invalid(&validator, &failed);
    }

    let missing_config = json!({
        "protocol": "rsi-meta.plugin",
        "version": 0,
        "kind": "lifecycle",
        "phase": "prepare_failed",
        "generation": 8,
    });
    assert_invalid(&validator, &missing_config);
}

fn parse_json_lines(source: &str) -> Vec<Value> {
    source
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn phases(frames: &[Value]) -> Vec<&str> {
    frames
        .iter()
        .map(|frame| frame["phase"].as_str().unwrap())
        .collect()
}

fn generations(frames: &[Value]) -> Vec<u64> {
    frames
        .iter()
        .map(|frame| frame["generation"].as_u64().unwrap())
        .collect()
}
