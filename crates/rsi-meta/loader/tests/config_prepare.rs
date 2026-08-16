use std::fs;

use rsi_meta_loader::{PluginPackage, prepare_config, prepare_config_with_schema};
use serde_json::json;
use tempfile::TempDir;

fn package_with_schema(schema: &serde_json::Value) -> (TempDir, PluginPackage) {
    let root = TempDir::new().unwrap();
    let manifest = r#"format_version = 0
config_schema = "config.schema.json"

[package]
id = "fixture.config"
version = "0.0.0"

[host_api]
major = 0
minimum_minor = 0

[[artifacts]]
target = "test-target"
path = "plugin.so"
"#;
    fs::write(root.path().join("plugin.toml"), manifest).unwrap();
    fs::write(
        root.path().join("config.schema.json"),
        serde_json::to_vec(schema).unwrap(),
    )
    .unwrap();
    let package = PluginPackage::open(root.path().join("plugin.toml")).unwrap();
    (root, package)
}

#[test]
fn prepare_can_validate_against_the_exact_schema_bytes_pinned_by_the_lock_check() {
    let original = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "required": ["value"],
        "properties": {"value": {"const": "original"}},
        "additionalProperties": false
    });
    let replacement = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "required": ["value"],
        "properties": {"value": {"const": "replacement"}},
        "additionalProperties": false
    });
    let (root, package) = package_with_schema(&original);
    let pinned = fs::read(root.path().join("config.schema.json")).unwrap();
    fs::write(
        root.path().join("config.schema.json"),
        serde_json::to_vec(&replacement).unwrap(),
    )
    .unwrap();

    assert!(
        prepare_config_with_schema(&package, Some(&pinned), json!({"value": "original"})).is_ok()
    );
}

#[test]
fn prepare_resolves_an_annotated_env_reference_and_returns_safe_audit_views() {
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "required": ["count", "token"],
        "properties": {
            "count": {"type": "integer", "minimum": 1},
            "token": {
                "type": "string",
                "minLength": 1,
                "x-rsi-meta-secret": true
            }
        },
        "additionalProperties": false
    });
    let (_root, package) = package_with_schema(&schema);
    let env_value = std::env::var("PATH").expect("test process must have PATH");
    let input = json!({
        "token": {"$secret": {"env": "PATH"}},
        "count": 2
    });

    let prepared = prepare_config(&package, input).unwrap();

    assert_eq!(
        prepared.resolved(),
        &json!({"count": 2, "token": env_value})
    );
    assert_eq!(
        prepared.redacted(),
        &json!({
            "count": 2,
            "token": {"$secret": {"env": "<redacted>"}}
        })
    );
    assert_eq!(
        prepared.audit_hash().to_hex(),
        "ba7b6814d47353aad54e4efc7ad9613fff38c29281568c3bfea4a5db4b6264d0"
    );
    assert!(!format!("{prepared:?}").contains(&env_value));
}

#[test]
fn secret_annotation_composes_with_sibling_all_of_constraints() {
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "required": ["token"],
        "allOf": [
            {
                "properties": {
                    "token": {
                        "type": "string",
                        "x-rsi-meta-secret": true
                    }
                }
            },
            {
                "properties": {
                    "token": {"type": "string", "minLength": 1}
                }
            }
        ],
        "unevaluatedProperties": false
    });
    let (_root, package) = package_with_schema(&schema);
    let expected = std::env::var("PATH").expect("test process must have PATH");

    let prepared = prepare_config(&package, json!({"token": {"$secret": {"env": "PATH"}}}))
        .expect("an annotation in one allOf branch applies at that instance location");

    assert_eq!(prepared.resolved(), &json!({"token": expected}));
}

#[cfg(unix)]
#[test]
fn prepare_reads_a_current_user_private_regular_secret_file() {
    use std::os::unix::fs::PermissionsExt;

    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "required": ["token"],
        "properties": {
            "token": {"type": "string", "x-rsi-meta-secret": true}
        },
        "additionalProperties": false
    });
    let (root, package) = package_with_schema(&schema);
    let secret_path = root.path().join("private-token");
    let secret_value = "fixture-file-secret\n";
    fs::write(&secret_path, secret_value).unwrap();
    fs::set_permissions(&secret_path, fs::Permissions::from_mode(0o600)).unwrap();

    let prepared = prepare_config(
        &package,
        json!({"token": {"$secret": {"file": secret_path}}}),
    )
    .unwrap();

    assert_eq!(prepared.resolved(), &json!({"token": secret_value}));
    assert_eq!(
        prepared.redacted(),
        &json!({"token": {"$secret": {"file": "<redacted>"}}})
    );
    let diagnostic = format!("{prepared:?}");
    assert!(!diagnostic.contains(secret_value));
    assert!(!diagnostic.contains(root.path().to_string_lossy().as_ref()));
}

#[derive(Debug)]
struct StaticKeyringBuilder;

impl keyring::credential::CredentialBuilderApi for StaticKeyringBuilder {
    fn build(
        &self,
        _target: Option<&str>,
        service: &str,
        user: &str,
    ) -> keyring::Result<Box<keyring::credential::Credential>> {
        if service != "rsi-meta-tests" || user != "instance-a" {
            return Err(keyring::Error::NoEntry);
        }
        Ok(Box::new(StaticCredential))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[derive(Debug)]
struct StaticCredential;

impl keyring::credential::CredentialApi for StaticCredential {
    fn set_secret(&self, _secret: &[u8]) -> keyring::Result<()> {
        Err(keyring::Error::NoEntry)
    }

    fn get_secret(&self) -> keyring::Result<Vec<u8>> {
        Ok(b"fixture-keyring-secret".to_vec())
    }

    fn delete_credential(&self) -> keyring::Result<()> {
        Err(keyring::Error::NoEntry)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[test]
fn prepare_resolves_the_exact_keyring_service_and_user_shape() {
    keyring::set_default_credential_builder(Box::new(StaticKeyringBuilder));
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "required": ["token"],
        "properties": {
            "token": {"type": "string", "x-rsi-meta-secret": true}
        },
        "additionalProperties": false
    });
    let (_root, package) = package_with_schema(&schema);

    let prepared = prepare_config(
        &package,
        json!({
            "token": {
                "$secret": {
                    "keyring": {"service": "rsi-meta-tests", "user": "instance-a"}
                }
            }
        }),
    )
    .unwrap();

    assert_eq!(
        prepared.resolved(),
        &json!({"token": "fixture-keyring-secret"})
    );
    assert_eq!(
        prepared.redacted(),
        &json!({
            "token": {
                "$secret": {
                    "keyring": {"service": "<redacted>", "user": "<redacted>"}
                }
            }
        })
    );

    let missing_service = "must-not-appear-service";
    let missing_user = "must-not-appear-user";
    let error = prepare_config(
        &package,
        json!({
            "token": {
                "$secret": {
                    "keyring": {"service": missing_service, "user": missing_user}
                }
            }
        }),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        rsi_meta_loader::ConfigPrepareError::SecretUnavailable { .. }
    ));
    let diagnostic = format!("{error:?} {error}");
    assert!(!diagnostic.contains(missing_service));
    assert!(!diagnostic.contains(missing_user));
}

#[test]
fn keyring_identifiers_are_bounded_before_backend_lookup() {
    keyring::set_default_credential_builder(Box::new(StaticKeyringBuilder));
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "required": ["token"],
        "properties": {
            "token": {"type": "string", "x-rsi-meta-secret": true}
        },
        "additionalProperties": false
    });
    let (_root, package) = package_with_schema(&schema);
    let oversized = "x".repeat(256);

    let error = prepare_config(
        &package,
        json!({
            "token": {
                "$secret": {
                    "keyring": {"service": oversized, "user": "instance-a"}
                }
            }
        }),
    )
    .expect_err("oversized keyring identity must be rejected before lookup");

    assert!(matches!(
        error,
        rsi_meta_loader::ConfigPrepareError::InvalidConfig { .. }
            | rsi_meta_loader::ConfigPrepareError::InvalidSecretReference { .. }
    ));
}

#[cfg(unix)]
#[test]
fn prepare_rejects_a_config_schema_symlink_that_escapes_the_package() {
    use std::os::unix::fs::symlink;

    let root = TempDir::new().unwrap();
    let package_directory = root.path().join("package");
    fs::create_dir(&package_directory).unwrap();
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object"
    });
    fs::write(
        root.path().join("outside.schema.json"),
        serde_json::to_vec(&schema).unwrap(),
    )
    .unwrap();
    symlink(
        root.path().join("outside.schema.json"),
        package_directory.join("config.schema.json"),
    )
    .unwrap();
    fs::write(
        package_directory.join("plugin.toml"),
        r#"format_version = 0
config_schema = "config.schema.json"

[package]
id = "fixture.config"
version = "0.0.0"

[host_api]
major = 0
minimum_minor = 0

[[artifacts]]
target = "test-target"
path = "plugin.so"
"#,
    )
    .unwrap();
    let package = PluginPackage::open(package_directory.join("plugin.toml")).unwrap();

    let error = prepare_config(&package, json!({})).unwrap_err();

    assert!(matches!(
        error,
        rsi_meta_loader::ConfigPrepareError::UnsafeSchemaPath
    ));
}

#[cfg(unix)]
#[test]
fn prepare_rejects_overpermissive_secret_file_mode_without_leaking_source_or_value() {
    use std::os::unix::fs::PermissionsExt;

    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {
            "token": {"type": "string", "x-rsi-meta-secret": true}
        }
    });
    let (root, package) = package_with_schema(&schema);
    let secret_path = root.path().join("overpermissive-token");
    let secret_value = "must-not-appear-in-error";
    fs::write(&secret_path, secret_value).unwrap();
    fs::set_permissions(&secret_path, fs::Permissions::from_mode(0o640)).unwrap();

    let error = prepare_config(
        &package,
        json!({"token": {"$secret": {"file": secret_path}}}),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        rsi_meta_loader::ConfigPrepareError::UnsafeSecretFile { .. }
    ));
    let diagnostic = format!("{error:?} {error}");
    assert!(!diagnostic.contains(secret_value));
    assert!(!diagnostic.contains(root.path().to_string_lossy().as_ref()));
}

#[cfg(unix)]
#[test]
fn prepare_rejects_a_secret_file_symlink_even_when_its_target_is_private() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {
            "token": {"type": "string", "x-rsi-meta-secret": true}
        }
    });
    let (root, package) = package_with_schema(&schema);
    let target = root.path().join("real-token");
    let link = root.path().join("linked-token");
    fs::write(&target, "private-target-secret").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&target, &link).unwrap();

    let error =
        prepare_config(&package, json!({"token": {"$secret": {"file": link}}})).unwrap_err();

    assert!(matches!(
        error,
        rsi_meta_loader::ConfigPrepareError::UnsafeSecretFile { .. }
    ));
}

#[cfg(unix)]
#[test]
fn prepare_rejects_an_oversized_secret_file() {
    use std::os::unix::fs::PermissionsExt;

    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {
            "token": {"type": "string", "x-rsi-meta-secret": true}
        }
    });
    let (root, package) = package_with_schema(&schema);
    let secret_path = root.path().join("oversized-token");
    fs::write(&secret_path, vec![b'x'; 2 * 1024 * 1024]).unwrap();
    fs::set_permissions(&secret_path, fs::Permissions::from_mode(0o600)).unwrap();

    assert!(
        prepare_config(
            &package,
            json!({"token": {"$secret": {"file": secret_path}}}),
        )
        .is_err()
    );
}

#[test]
fn annotated_secrets_require_exact_references_and_errors_hide_input_values() {
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "required": ["token"],
        "properties": {
            "token": {"type": "string", "x-rsi-meta-secret": true}
        },
        "additionalProperties": false
    });
    let (_root, package) = package_with_schema(&schema);
    let plaintext = "plaintext-must-never-be-logged";

    let error = prepare_config(&package, json!({"token": plaintext})).unwrap_err();
    assert!(matches!(
        error,
        rsi_meta_loader::ConfigPrepareError::InvalidConfig { .. }
    ));
    assert!(!format!("{error:?} {error}").contains(plaintext));

    let source_name = "RSI_META_TEST_ENV_THAT_DOES_NOT_EXIST_9B32243D";
    let error = prepare_config(
        &package,
        json!({"token": {"$secret": {"env": source_name}}}),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        rsi_meta_loader::ConfigPrepareError::SecretUnavailable { .. }
    ));
    assert!(!format!("{error:?} {error}").contains(source_name));

    let error = prepare_config(
        &package,
        json!({
            "token": {"$secret": {"env": "PATH", "file": "/tmp/not-used"}}
        }),
    )
    .unwrap_err();
    assert!(
        matches!(
            error,
            rsi_meta_loader::ConfigPrepareError::InvalidConfig { .. }
        ),
        "unexpected error: {error:?}"
    );
}

#[test]
fn secret_references_are_reserved_for_annotated_schema_locations() {
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {"metadata": {}},
        "additionalProperties": false
    });
    let (_root, package) = package_with_schema(&schema);

    let error =
        prepare_config(&package, json!({"metadata": {"$secret": {"env": "PATH"}}})).unwrap_err();

    assert!(matches!(
        error,
        rsi_meta_loader::ConfigPrepareError::InvalidConfig { .. }
    ));
}

#[test]
fn secret_references_are_rejected_at_any_depth_below_an_unannotated_open_schema() {
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {"metadata": {}},
        "additionalProperties": false
    });
    let (_root, package) = package_with_schema(&schema);

    let error = prepare_config(
        &package,
        json!({
            "metadata": {
                "nested": {
                    "$secret": {"env": "PATH"}
                }
            }
        }),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        rsi_meta_loader::ConfigPrepareError::InvalidConfig { .. }
    ));

    let prepared = prepare_config(
        &package,
        json!({"metadata": {"nested": {"ordinary": [1, 2, 3]}}}),
    )
    .unwrap();
    assert_eq!(
        prepared.resolved(),
        &json!({"metadata": {"nested": {"ordinary": [1, 2, 3]}}})
    );
}

#[test]
fn secret_references_are_rejected_through_custom_keyword_ref_targets() {
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {
            "metadata": {"$ref": "#/x-custom-schema"}
        },
        "additionalProperties": false,
        "x-custom-schema": {}
    });
    let (_root, package) = package_with_schema(&schema);

    let error = prepare_config(&package, json!({"metadata": {"$secret": {"env": "PATH"}}}))
        .expect_err("an unannotated custom-keyword ref target must stay secret-free");

    assert!(
        matches!(
            error,
            rsi_meta_loader::ConfigPrepareError::InvalidConfig { .. }
        ),
        "unexpected error: {error:?}"
    );
}

#[test]
fn prepare_rejects_an_oversized_package_config_schema() {
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object"
    });
    let (root, package) = package_with_schema(&schema);
    let oversized_schema = format!(
        "{{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"description\":\"{}\",\"type\":\"object\"}}",
        "x".repeat(5 * 1024 * 1024)
    );
    fs::write(root.path().join("config.schema.json"), oversized_schema).unwrap();

    assert!(matches!(
        prepare_config(&package, json!({})),
        Err(rsi_meta_loader::ConfigPrepareError::SchemaRead)
    ));
}

#[test]
fn nested_schema_resource_ids_preserve_recursive_secret_enforcement() {
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {
            "metadata": {
                "$id": "urn:rsi-meta-test:metadata",
                "type": "object"
            }
        },
        "additionalProperties": false
    });
    let (_root, package) = package_with_schema(&schema);

    assert!(
        prepare_config(&package, json!({"metadata": {"ordinary": true}})).is_ok(),
        "adding recursive secret checks must not invalidate nested schema resources"
    );
    assert!(
        prepare_config(
            &package,
            json!({"metadata": {"nested": {"$secret": {"env": "PATH"}}}}),
        )
        .is_err()
    );
}

#[test]
fn annotation_in_one_all_of_branch_is_not_rejected_by_a_sibling_branch() {
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "allOf": [
            {
                "required": ["token"],
                "properties": {
                    "token": {"type": "string", "x-rsi-meta-secret": true}
                }
            },
            {
                "required": ["count"],
                "properties": {"count": {"type": "integer"}}
            }
        ]
    });
    let (_root, package) = package_with_schema(&schema);

    assert!(
        prepare_config(
            &package,
            json!({"token": {"$secret": {"env": "PATH"}}, "count": 1}),
        )
        .is_ok()
    );
}

#[test]
fn draft_2020_12_refs_and_full_instance_constraints_are_enforced() {
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$defs": {
            "credential": {
                "type": "string",
                "minLength": 2,
                "x-rsi-meta-secret": true
            }
        },
        "type": "object",
        "required": ["endpoint", "token"],
        "properties": {
            "endpoint": {"type": "string", "format": "uri"},
            "token": {"$ref": "#/$defs/credential"}
        },
        "additionalProperties": false
    });
    let (_root, package) = package_with_schema(&schema);

    let prepared = prepare_config(
        &package,
        json!({
            "endpoint": "https://example.invalid/v1",
            "token": {"$secret": {"env": "PATH"}}
        }),
    )
    .unwrap();
    assert_eq!(
        prepared.resolved()["endpoint"],
        "https://example.invalid/v1"
    );

    assert!(matches!(
        prepare_config(
            &package,
            json!({
                "endpoint": "not a URI",
                "token": {"$secret": {"env": "PATH"}},
                "extra": true
            })
        ),
        Err(rsi_meta_loader::ConfigPrepareError::InvalidConfig { .. })
    ));
}

#[test]
fn package_without_a_schema_accepts_only_an_empty_instance_config() {
    let root = TempDir::new().unwrap();
    fs::write(
        root.path().join("plugin.toml"),
        r#"format_version = 0

[package]
id = "fixture.no-config"
version = "0.0.0"

[host_api]
major = 0
minimum_minor = 0

[[artifacts]]
target = "test-target"
path = "plugin.so"
"#,
    )
    .unwrap();
    let package = PluginPackage::open(root.path().join("plugin.toml")).unwrap();

    let prepared = prepare_config(&package, json!({})).unwrap();
    assert_eq!(prepared.resolved(), &json!({}));
    assert_eq!(prepared.redacted(), &json!({}));
    assert_eq!(
        prepared.audit_hash().to_hex(),
        "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
    );
    assert!(matches!(
        prepare_config(&package, json!({"unexpected": true})),
        Err(rsi_meta_loader::ConfigPrepareError::MissingSchema)
    ));
}

#[test]
fn external_schema_references_fail_closed_without_retrieval() {
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$ref": "https://127.0.0.1:9/schema-that-must-not-be-fetched.json"
    });
    let (_root, package) = package_with_schema(&schema);

    assert!(matches!(
        prepare_config(&package, json!({})),
        Err(rsi_meta_loader::ConfigPrepareError::InvalidSchema)
    ));
}

#[cfg(unix)]
#[test]
fn resolved_secret_value_is_validated_against_the_original_schema() {
    use std::os::unix::fs::PermissionsExt;

    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {
            "token": {
                "type": "string",
                "pattern": "^allowed-",
                "x-rsi-meta-secret": true
            }
        }
    });
    let (root, package) = package_with_schema(&schema);
    let secret_path = root.path().join("wrong-pattern");
    let secret_value = "resolved-secret-does-not-match";
    fs::write(&secret_path, secret_value).unwrap();
    fs::set_permissions(&secret_path, fs::Permissions::from_mode(0o600)).unwrap();

    let error = prepare_config(
        &package,
        json!({"token": {"$secret": {"file": secret_path}}}),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        rsi_meta_loader::ConfigPrepareError::InvalidConfig { .. }
    ));
    assert!(!format!("{error:?} {error}").contains(secret_value));
}
