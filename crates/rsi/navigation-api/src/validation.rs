use super::{
    ActivityCursor, ApiError, HostEpoch, NavigationCursor, NavigationEntry, NavigationFilter,
    NavigationPage, PinnedEntry, PinnedPage, Result, WorkspaceFilter, WorkspaceId, cursor_advances,
    matches_query, revision,
};
use rsi_agent_session_protocol::ExecutionCoordinates;
use std::collections::BTreeSet;

pub(super) fn entry(value: &NavigationEntry, filter: &NavigationFilter) -> Result<ActivityCursor> {
    value.metadata.validate()?;
    let created = revision(&value.created_at_ms)?;
    let activity = revision(&value.last_activity_ms)?;
    let coordinates = ExecutionCoordinates::new(value.location.clone(), &value.path)
        .map_err(|_| ApiError::Invalid("invalid navigation coordinates".into()))?;
    if created == 0
        || activity < created
        || value.metadata.archived != filter.archived
        || !filter.workspace.matches(value.workspace.as_ref())
        || value
            .workspace
            .as_ref()
            .is_some_and(|id| id != &WorkspaceId::from_coordinates(&coordinates))
        || !matches_query(
            &filter.query.to_lowercase(),
            &value.session,
            &value.metadata,
            Some(&value.path),
        )
    {
        return Err(ApiError::Invalid("invalid navigation match".into()));
    }
    Ok(ActivityCursor {
        last_activity_ms: activity,
        session_id: value.session.clone(),
    })
}

pub(super) fn page(
    page: &NavigationPage,
    filter: &NavigationFilter,
    after: Option<&NavigationCursor>,
    epoch: &HostEpoch,
) -> Result<()> {
    revision(&page.metadata_revision)?;
    if page.entries.len() > 64
        || page.scanned > 256
        || usize::from(page.scanned) < page.entries.len()
        || page
            .newest
            .as_ref()
            .is_some_and(|key| key.last_activity_ms == 0)
        || page.scanned > 0 && page.newest.is_none()
        || after.is_some_and(|cursor| {
            cursor.filter != *filter
                || cursor.host_epoch != *epoch
                || cursor.metadata_revision != page.metadata_revision
                || cursor.after.last_activity_ms == 0
        })
    {
        return Err(ApiError::Invalid(
            "invalid navigation page bounds or identity".into(),
        ));
    }
    let mut seen = BTreeSet::new();
    let mut previous = after.map(|cursor| cursor.after.clone());
    for value in &page.entries {
        let key = entry(value, filter)?;
        if !seen.insert(&value.session)
            || value.metadata.pinned
            || previous.as_ref().is_some_and(|previous| &key >= previous)
            || page.newest.as_ref().is_none_or(|newest| &key > newest)
        {
            return Err(ApiError::Invalid("invalid navigation ordering".into()));
        }
        previous = Some(key);
    }
    if let Some(next) = &page.next
        && (next.filter != *filter
            || next.host_epoch != *epoch
            || next.metadata_revision != page.metadata_revision
            || page.scanned == 0
            || next.after.last_activity_ms == 0
            || after.is_some_and(|cursor| !cursor_advances(&cursor.after, &next.after))
            || previous.as_ref().is_some_and(|key| &next.after > key)
            || page.newest.as_ref().is_none_or(|key| &next.after > key))
    {
        return Err(ApiError::Invalid("invalid navigation continuation".into()));
    }
    Ok(())
}

pub(super) fn pins(page: &PinnedPage, filter: &NavigationFilter) -> Result<()> {
    revision(&page.metadata_revision)?;
    if page.entries.len() > 64 {
        return Err(ApiError::Invalid("too many pinned sessions".into()));
    }
    let mut seen = BTreeSet::new();
    let mut previous = None;
    let mut missing = None;
    for row in &page.entries {
        let (id, metadata) = match row {
            PinnedEntry::Available { entry: value } => {
                let key = entry(value, filter)?;
                if missing.is_some() || previous.as_ref().is_some_and(|previous| &key >= previous) {
                    return Err(ApiError::Invalid("invalid pinned ordering".into()));
                }
                previous = Some(key);
                (&value.session, &value.metadata)
            }
            PinnedEntry::Missing { session, metadata } => {
                if filter.workspace != WorkspaceFilter::All
                    || missing.is_some_and(|previous| session >= previous)
                    || !matches_query(&filter.query.to_lowercase(), session, metadata, None)
                {
                    return Err(ApiError::Invalid("invalid missing pinned match".into()));
                }
                missing = Some(session);
                (session, metadata)
            }
        };
        metadata.validate()?;
        if !metadata.pinned || metadata.archived != filter.archived || !seen.insert(id) {
            return Err(ApiError::Invalid("invalid pinned metadata".into()));
        }
    }
    Ok(())
}
