use rusqlite::{Connection, OptionalExtension, params};

use super::{
    MAX_STATE_BYTES_PER_COMPOSITION, MAX_STATE_BYTES_PER_INSTANCE, MAX_STATE_KEYS_PER_COMPOSITION,
    MAX_STATE_KEYS_PER_INSTANCE, MAX_STATE_TOMBSTONES_PER_INSTANCE, to_u64,
};
use crate::model::InstanceId;
use crate::{HostError, Result};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PluginStateValue {
    pub version: u64,
    pub value: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CasResult {
    Applied(PluginStateValue),
    Conflict(Option<PluginStateValue>),
}

pub(super) fn enforce_state_quotas(
    connection: &Connection,
    composition_id: &str,
    instance_id: &InstanceId,
    replaced_key: &str,
    value_bytes: usize,
    tombstone: bool,
) -> Result<()> {
    let (instance_keys, instance_tombstones, instance_bytes) = state_usage(
        connection,
        "SELECT COUNT(*), COALESCE(SUM(tombstone), 0),
                COALESCE(SUM(length(CAST(COALESCE(value_json, '') AS BLOB))), 0)
         FROM plugin_state
         WHERE composition_id = ?1 AND instance_id = ?2 AND state_key != ?3",
        params![composition_id, instance_id.0, replaced_key],
    )?;
    require_quota(
        "instance_key_count",
        instance_keys.saturating_add(1),
        MAX_STATE_KEYS_PER_INSTANCE,
    )?;
    require_quota(
        "instance_tombstone_count",
        instance_tombstones.saturating_add(usize::from(tombstone)),
        MAX_STATE_TOMBSTONES_PER_INSTANCE,
    )?;
    require_quota(
        "instance_live_bytes",
        instance_bytes.saturating_add(value_bytes),
        MAX_STATE_BYTES_PER_INSTANCE,
    )?;

    let (composition_keys, _, composition_bytes) = state_usage(
        connection,
        "SELECT COUNT(*), COALESCE(SUM(tombstone), 0),
                COALESCE(SUM(length(CAST(COALESCE(value_json, '') AS BLOB))), 0)
         FROM plugin_state
         WHERE composition_id = ?1
           AND NOT (instance_id = ?2 AND state_key = ?3)",
        params![composition_id, instance_id.0, replaced_key],
    )?;
    require_quota(
        "composition_key_count",
        composition_keys.saturating_add(1),
        MAX_STATE_KEYS_PER_COMPOSITION,
    )?;
    require_quota(
        "composition_live_bytes",
        composition_bytes.saturating_add(value_bytes),
        MAX_STATE_BYTES_PER_COMPOSITION,
    )
}

fn state_usage(
    connection: &Connection,
    query: &str,
    parameters: impl rusqlite::Params,
) -> Result<(usize, usize, usize)> {
    let (keys, tombstones, bytes): (i64, i64, i64) =
        connection.query_row(query, parameters, |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    Ok((
        usize::try_from(keys)
            .map_err(|_| HostError::InvalidEnvelope("negative state key count".to_owned()))?,
        usize::try_from(tombstones)
            .map_err(|_| HostError::InvalidEnvelope("negative tombstone count".to_owned()))?,
        usize::try_from(bytes)
            .map_err(|_| HostError::InvalidEnvelope("negative state byte count".to_owned()))?,
    ))
}

fn require_quota(quota: &'static str, requested: usize, maximum: usize) -> Result<()> {
    if requested > maximum {
        return Err(HostError::StateQuotaExceeded {
            quota,
            requested,
            maximum,
        });
    }
    Ok(())
}

pub(super) fn read_plugin_state(
    connection: &Connection,
    composition_id: &str,
    instance_id: &InstanceId,
    key: &str,
) -> Result<Option<PluginStateValue>> {
    let row = connection
        .query_row(
            "SELECT version, value_json, tombstone FROM plugin_state
             WHERE composition_id = ?1 AND instance_id = ?2 AND state_key = ?3",
            params![composition_id, instance_id.0, key],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    row.map(|(version, value_json, tombstone)| {
        let value = if tombstone == 0 {
            value_json
                .map(|json| serde_json::from_str(&json))
                .transpose()?
        } else {
            None
        };
        Ok(PluginStateValue {
            version: to_u64(version, "plugin state version")?,
            value,
        })
    })
    .transpose()
}
