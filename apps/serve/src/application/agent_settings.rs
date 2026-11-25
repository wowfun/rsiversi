use super::BoundSessions;
use rsi_extension::{Error, Result};
use rsi_settings_protocol::{
    SettingsAccessContract, SettingsContract, SettingsError, SettingsVersion,
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Clone, Copy)]
pub(super) enum Field {
    ToolConcurrency,
    DefaultPermission,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    request: String,
    operation: Operation,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Read,
    Set {
        expected: SettingsVersion,
        value: SettingValue,
    },
    Reset {
        expected: SettingsVersion,
    },
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SettingValue {
    Number(serde_json::Number),
    Permission(rsi_agent_session_protocol::SessionPermission),
}

impl BoundSessions {
    pub(super) async fn agent_settings(&self, field: Field, payload: Value) -> Result<Value> {
        let (namespace, key) = match field {
            Field::ToolConcurrency => ("rsi.agent-loop", "max_parallel_tool_calls"),
            Field::DefaultPermission => ("rsi.agent", "permission_default"),
        };
        let input: Request = serde_json::from_value(payload).map_err(|_| Error::Protocol)?;
        let origin = self
            .routes
            .caller(&input.request)
            .map_err(|_| Error::Cancelled)?;
        let grant = self
            .context
            .lookup_local::<rsi_configuration_access::ConfigurationAccessContract>()
            .ok_or_else(|| Error::Remote("Host configuration access unavailable".into()))?;
        let _lease = grant
            .admit(&origin)
            .map_err(|error| Error::Remote(error.to_string()))?;
        let Some(access) = self.context.lookup_local::<SettingsAccessContract>() else {
            return Ok(Value::Null);
        };
        match self
            .apply_setting(field, namespace, key, input.operation, access.as_ref())
            .await
        {
            Ok(value) => Ok(value),
            Err(SettingsError::Conflict { .. }) => Ok(json!({"code":"conflict"})),
            Err(SettingsError::StaleRegistration(_)) => Ok(json!({"code":"stale_registration"})),
            Err(SettingsError::UnknownNamespace(_)) => Ok(Value::Null),
            Err(error) => Err(Error::Remote(error.to_string())),
        }
    }

    async fn apply_setting(
        &self,
        field: Field,
        namespace: &str,
        key: &str,
        operation: Operation,
        access: &dyn rsi_settings_protocol::SettingsAccess,
    ) -> rsi_settings_protocol::Result<Value> {
        let registry = self
            .context
            .lookup_local::<SettingsContract>()
            .ok_or_else(|| SettingsError::StaleRegistration(namespace.into()))?;
        let scope = registry.scope(namespace)?;
        let (version, selected) = match operation {
            Operation::Read => scope.user_value(Some(key)).await?,
            Operation::Set { expected, value } => {
                let value = match (field, value) {
                    (Field::ToolConcurrency, SettingValue::Number(n)) => Value::Number(n),
                    (Field::DefaultPermission, SettingValue::Permission(permission)) => {
                        json!(permission)
                    }
                    _ => {
                        return Err(SettingsError::InvalidInput(format!(
                            "Invalid value for {key}"
                        )));
                    }
                };
                let mut snapshot = access
                    .set_fields(
                        namespace,
                        &expected,
                        [(key.into(), value)].into_iter().collect(),
                    )
                    .await?;
                (snapshot.version(), Some(snapshot.value[key].take()))
            }
            Operation::Reset { expected } => {
                let snapshot = match field {
                    Field::ToolConcurrency => access.clear(namespace, &expected).await?,
                    Field::DefaultPermission => {
                        let (version, section) = scope.user_value(None).await?;
                        if version.scope_id != expected.scope_id {
                            return Err(SettingsError::StaleRegistration(namespace.into()));
                        }
                        if version.revision != expected.revision {
                            return Err(SettingsError::Conflict {
                                expected: expected.revision,
                                actual: version.revision,
                            });
                        }
                        let mut raw = section.unwrap_or_else(|| json!({}));
                        let fields = raw
                            .as_object_mut()
                            .ok_or_else(|| SettingsError::Corrupt(namespace.into()))?;
                        fields.remove(key);
                        if fields.is_empty() {
                            access.clear(namespace, &expected).await?
                        } else {
                            access.replace(namespace, &expected, raw).await?
                        }
                    }
                };
                (snapshot.version(), None)
            }
        };
        let description = access.describe(namespace).await?;
        if version.scope_id != description.version.scope_id {
            return Err(SettingsError::StaleRegistration(namespace.into()));
        }
        let base = match field {
            Field::ToolConcurrency => description.defaults[key].clone(),
            Field::DefaultPermission => scope.get()?.value["sandbox"].take(),
        };
        Ok(
            json!({"base": base, "selected": selected, "version": version, "writable": description.writable}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clearing_an_override_requires_an_explicit_versioned_reset() {
        let version = json!({"scope_id":"0123456789abcdef0123456789abcdef","revision":7});
        assert!(
            serde_json::from_value::<Operation>(json!({"type":"set","expected":version})).is_err()
        );
        assert!(
            serde_json::from_value::<Operation>(
                json!({"type":"set","expected":version,"value":null})
            )
            .is_err()
        );
        assert!(serde_json::from_value::<Operation>(json!({"type":"reset"})).is_err());
        assert!(matches!(
            serde_json::from_value::<Operation>(json!({"type":"reset","expected":version})),
            Ok(Operation::Reset { .. })
        ));
    }
}
