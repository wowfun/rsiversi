use super::BoundSessions;
use rsi_extension::{Error, Result};
use rsi_settings_protocol::{
    SettingsAccessContract, SettingsContract, SettingsError, SettingsVersion,
};
use serde::Deserialize;
use serde_json::{Value, json};

const NAMESPACE: &str = "rsi.agent-loop";
const FIELD: &str = "max_parallel_tool_calls";

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
        value: serde_json::Number,
    },
    Reset {
        expected: SettingsVersion,
    },
}

impl BoundSessions {
    pub(super) async fn loop_settings(&self, payload: Value) -> Result<Value> {
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
        let result = async {
            let (version, selected) = match input.operation {
                Operation::Read => {
                    let registry = self.context.lookup_local::<SettingsContract>()
                        .ok_or_else(|| SettingsError::StaleRegistration(NAMESPACE.into()))?;
                    registry.scope(NAMESPACE)?.user_value(Some(FIELD)).await?
                }
                Operation::Set { expected, value } => {
                    let snapshot = access.replace(NAMESPACE, &expected, json!({(FIELD):value})).await?;
                    (snapshot.version(), Some(Value::from(value)))
                }
                Operation::Reset { expected } => (access.clear(NAMESPACE, &expected).await?.version(), None),
            };
            let description = access.describe(NAMESPACE).await?;
            if version.scope_id != description.version.scope_id {
                return Err(SettingsError::StaleRegistration(NAMESPACE.into()));
            }
            Ok(json!({"base": description.defaults[FIELD], "selected": selected, "version": version, "writable": description.writable}))
        }.await;
        match result {
            Ok(value) => Ok(value),
            Err(SettingsError::Conflict { .. }) => Ok(json!({"code":"conflict"})),
            Err(SettingsError::StaleRegistration(_)) => Ok(json!({"code":"stale_registration"})),
            Err(SettingsError::UnknownNamespace(_)) => Ok(Value::Null),
            Err(error) => Err(Error::Remote(error.to_string())),
        }
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
