use crate::wire::number;
use rsi_acp_protocol::{
    observation::{ConversationId, Page, Snapshot},
    service::{Error, PermissionSummary, Resident, Result, View},
};
use std::collections::BTreeSet;
pub(super) fn snapshot(value: &Snapshot, id: Option<&ConversationId>) -> Result<()> {
    value.validate().map_err(|_| Error::Input)?;
    if id.is_some_and(|id| *id != value.id) {
        return Err(Error::Input);
    }
    Ok(())
}
fn targets(permissions: &[PermissionSummary], generation: u64) -> Result<()> {
    let mut ids = BTreeSet::new();
    if permissions.len() > 32 {
        return Err(Error::Input);
    }
    for permission in permissions {
        if permission.id.is_empty()
            || permission.id.len() > 128
            || permission.title.len() > 512
            || number(&permission.generation)? != generation
            || !ids.insert(&permission.id)
        {
            return Err(Error::Input);
        }
    }
    Ok(())
}
pub(super) fn resident(value: &Resident) -> Result<()> {
    snapshot(&value.snapshot, None)?;
    number(&value.sequence)?;
    targets(&value.permissions, value.snapshot.generation)
}
pub(super) fn view(value: &View, id: &ConversationId) -> Result<()> {
    snapshot(&value.snapshot, Some(id))?;
    targets(
        &value
            .permissions
            .iter()
            .map(|permission| PermissionSummary {
                id: permission.id.clone(),
                generation: permission.generation.clone(),
                title: permission.title.clone(),
            })
            .collect::<Vec<_>>(),
        value.snapshot.generation,
    )?;
    for permission in &value.permissions {
        if permission.options.is_empty()
            || permission.options.len() > 32
            || number(&permission.source_sequence)? == 0
        {
            return Err(Error::Input);
        }
        let mut ids = BTreeSet::new();
        for option in &permission.options {
            if option.id.is_empty()
                || option.id.len() > 256
                || option.name.len() > 128
                || !matches!(
                    option.kind.as_str(),
                    "allow_once" | "allow_always" | "reject_once" | "reject_always"
                )
                || !ids.insert(&option.id)
            {
                return Err(Error::Input);
            }
        }
    }
    Ok(())
}
pub(super) fn page(value: &Page, epoch: u64, after: u64) -> Result<()> {
    if value.records.len() > 64 || value.has_more && value.records.is_empty() {
        return Err(Error::Input);
    }
    let mut previous = after;
    let mut bytes = 0_usize;
    for record in &value.records {
        if record.sequence <= previous
            || record.sequence > i64::MAX as u64
            || record.epoch != epoch
            || record.bytes > 1024 * 1024
            || record.bytes == 0
        {
            return Err(Error::Input);
        }
        previous = record.sequence;
        if let Some(value) = &record.value {
            let size = serde_json::to_vec(value).map_err(|_| Error::Input)?.len();
            if size != record.bytes {
                return Err(Error::Input);
            }
            bytes = bytes.saturating_add(size);
        }
    }
    if bytes > 256 * 1024 {
        return Err(Error::Input);
    }
    Ok(())
}
