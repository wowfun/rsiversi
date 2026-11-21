use super::{LocalSessionService, Result, SessionError, map_store_error, map_turn_error};
use futures_util::{StreamExt as _, TryStreamExt as _};
use rsi_session_protocol::{
    RecentSessionCursor, SCHEDULE_CATALOG_PAGE_SIZE, ScheduleCatalogStream, ScheduleCatalogUpdate,
    ScheduledSession,
};
use sha2::{Digest, Sha256};

impl LocalSessionService {
    pub(super) async fn edit_reminder(
        &self,
        task: rsi_session_protocol::ScheduledTaskId,
        command: &'static str,
    ) -> Result<bool> {
        use rsi_agent_schedule::{SCHEDULE_DELETE, SCHEDULE_DOMAIN, ScheduleState};
        use rsi_agent_session_protocol::{
            CommandArguments, CommandRevision, ContributionId, DomainRequestId,
            SessionCommandInvocation,
        };
        // Sample existing owners before this management request attaches its own handle.
        let attached = command == SCHEDULE_DELETE && self.lifecycles.attached(&task.session_id);
        let handle = self.attach_local(&task.session_id).await?;
        let _operation = handle.admit_mutation()?;
        let _activity = handle.begin_activity()?;
        let mut notify = command == SCHEDULE_DELETE
            && (attached
                || self
                    .projection_service
                    .resident_composition(&task.session_id)
                    .is_ok_and(|state| {
                        matches!(
                            state,
                            rsi_agent_turn_protocol::ResidentComposition::Resident { .. }
                        )
                    }));
        loop {
            let Some((control_seq, row)) = self
                .turns
                .domain_state(&task.session_id, SCHEDULE_DOMAIN)
                .await
                .map_err(map_turn_error)?
            else {
                return Ok(false);
            };
            let state =
                ScheduleState::from_snapshot(&row.snapshot).map_err(SessionError::Backend)?;
            if !state.reminders.iter().any(|r| r.id == task.id) {
                return Ok(false);
            }
            let mut hash = Sha256::new();
            hash.update(control_seq.to_be_bytes());
            hash.update([u8::from(notify)]);
            hash.update(task.id.as_str().as_bytes());
            let invocation = SessionCommandInvocation {
                command: ContributionId::new(command).expect("registered command"),
                request_id: DomainRequestId::new(format!(
                    "schedule-{}-{:x}",
                    if command == SCHEDULE_DELETE {
                        "delete"
                    } else {
                        "run"
                    },
                    hash.finalize()
                ))
                .expect("SHA-256 identity"),
                expected_revision: CommandRevision::Durable { control_seq },
                arguments: CommandArguments::new(if command == SCHEDULE_DELETE {
                    serde_json::json!({"id":task.id,"notify":notify})
                } else {
                    serde_json::json!({"id":task.id})
                })
                .map_err(|e| SessionError::Invalid(e.to_string()))?,
            };
            match handle.dispatch_command(invocation).await {
                Ok(_) => return Ok(true),
                Err(SessionError::CommandRevisionConflict { .. }) => continue,
                Err(SessionError::Capacity) if notify => notify = false,
                Err(error) => return Err(error),
            }
        }
    }

    pub(super) fn navigation(&self) -> Option<std::sync::Arc<rsi_navigation::Navigation>> {
        self.plugin_context
            .as_ref()?
            .lookup_local::<rsi_navigation::NavigationContract>()
    }
    fn schedule_controller(
        &self,
    ) -> Option<std::sync::Arc<dyn rsi_agent_schedule::ScheduleController>> {
        self.plugin_context
            .as_ref()?
            .lookup_local::<rsi_agent_schedule::ScheduleControllerContract>()
    }

    pub(super) async fn catalog_page(
        &self,
        after: Option<&RecentSessionCursor>,
        archived: Option<&rsi_navigation::ArchivedSessions>,
    ) -> Result<rsi_session_protocol::ScheduleCatalogPage> {
        use rsi_agent_schedule::{SCHEDULE_DOMAIN, ScheduleState};
        let controller = self
            .schedule_controller()
            .map(|owner| owner.delivery_holds());
        let (sessions, next) = self
            .visible_recent(
                after,
                rsi_session_protocol::SCHEDULE_CATALOG_PAGE_SIZE,
                Some(SCHEDULE_DOMAIN),
                |summary| {
                    let holds = &controller;
                    async move {
                        let position = summary.cursor();
                        let page = self
                            .store
                            .read_domain_states(
                                &position.session_id,
                                Some(summary.durable_control_seq),
                                Some(SCHEDULE_DOMAIN),
                            )
                            .await
                            .map_err(map_store_error)?;
                        let row = page.states.into_iter().next().ok_or_else(|| {
                            SessionError::Backend(
                                "scheduled domain is absent at its catalog cut".into(),
                            )
                        })?;
                        let state = ScheduleState::from_snapshot(&row.snapshot)
                            .map_err(SessionError::Backend)?;
                        let exhausted = state.exhausted();
                        let holds = holds.as_ref().map(|rx| rx.borrow());
                        let held = holds
                            .as_ref()
                            .and_then(|sessions| sessions.get(&position.session_id));
                        let reminders = state
                            .reminders
                            .into_iter()
                            .map(|rule| rsi_session_protocol::ScheduledReminder {
                                executing: held.is_some_and(|ids| ids.contains(&rule.id))
                                    || state.reservation.as_ref().is_some_and(|last| {
                                        !last.settled && last.reminder_id == rule.id
                                    }),
                                rule,
                            })
                            .collect();
                        Ok(rsi_session_protocol::ScheduledSession {
                            archived: archived
                                .is_some_and(|ids| ids.contains(&position.session_id)),
                            position,
                            exhausted,
                            reminders,
                        })
                    }
                },
            )
            .await?;
        Ok(rsi_session_protocol::ScheduleCatalogPage { sessions, next })
    }

    /// Page boundaries depend only on visible, nonempty groups. A hidden Store
    /// cursor or a retained empty Schedule state cannot displace visible rows.
    fn catalog_pages(
        &self,
        archived: Option<rsi_navigation::ArchivedSessions>,
    ) -> impl futures_util::Stream<Item = Result<Vec<ScheduledSession>>> + '_ {
        async_stream::try_stream! {
            let mut cursor = None;
            let mut groups = Vec::new();
            loop {
                let page = self.catalog_page(cursor.as_ref(), archived.as_ref()).await?;
                for group in page.sessions {
                    if group.reminders.is_empty() { continue; }
                    groups.push(group);
                    if groups.len() == SCHEDULE_CATALOG_PAGE_SIZE {
                        yield std::mem::take(&mut groups);
                    }
                }
                cursor = page.next;
                if cursor.is_none() { break; }
            }
            if !groups.is_empty() { yield groups; }
        }
    }

    pub(super) fn observe_catalog(&self) -> Result<ScheduleCatalogStream> {
        self.check_origin()?;
        let mut schedule_edits = self
            .projection_service
            .watch_edited_domain(rsi_agent_schedule::SCHEDULE_DOMAIN)
            .map_err(map_turn_error)?;
        let mut visibility = self.visibility_changes();
        let revoked = match &self.origin {
            rsi_api_protocol::CallOrigin::Local => None,
            rsi_api_protocol::CallOrigin::Device(device) => Some(device.revoked.clone()),
        };
        let stopped = self.projection_stopped.clone();
        let cancelled = async move {
            match revoked {
                Some(device) => tokio::select! {
                    () = device.cancelled() => {},
                    () = stopped.cancelled() => {},
                },
                None => stopped.cancelled().await,
            }
        };
        let mut registry = self
            .plugin_context
            .as_ref()
            .map(|ctx| ctx.runtime().subscribe());
        let service = self.clone();
        Ok(async_stream::try_stream! {
            // Keep a digest per page; prompt text lives only in the page being yielded.
            let mut digests: Vec<[u8; 32]> = Vec::new();
            let mut initial = true;
            loop {
                let mut count = 0u32;
                let mut changed = initial;
                // Subscribe before the snapshot so edits during the scan wake the next pass.
                let navigation = service.navigation();
                let controller = service.schedule_controller();
                let mut holds = controller.as_ref().map(|owner| owner.delivery_holds());
                let mut archive_changes = navigation.as_ref().map(|owner| owner.archive_changes());
                let archived = navigation.as_ref().map(|owner| owner.archived_sessions());
                let pages = service.catalog_pages(archived);
                futures_util::pin_mut!(pages);
                while let Some(sessions) = pages.try_next().await? {
                    let mut hash = Sha256::new();
                    serde_json::to_writer(&mut hash, &sessions)
                        .map_err(|reason| SessionError::Backend(reason.to_string()))?;
                    let digest = hash.finalize().into();
                    let index = count;
                    count = count.checked_add(1).ok_or(SessionError::Capacity)?;
                    if digests.get(index as usize) == Some(&digest) { continue; }
                    if let Some(slot) = digests.get_mut(index as usize) {
                        *slot = digest;
                    } else {
                        digests.push(digest);
                    }
                    changed = true;
                    yield ScheduleCatalogUpdate::Page { index, sessions };
                }
                if changed || digests.len() != count as usize {
                    digests.truncate(count as usize);
                    yield ScheduleCatalogUpdate::Complete { pages: count };
                }
                initial = false;
                loop {
                    tokio::select! {
                        update = schedule_edits.next() => if update.is_some() { break; } else { return; },
                        _ = async { holds.as_mut().expect("available controller").changed().await }, if holds.is_some() => break,
                        _ = visibility.next() => break,
                        _ = async { archive_changes.as_mut().expect("available owner").changed().await }, if archive_changes.is_some() => break,
                        change = async { registry.as_mut().expect("plugin context").changed().await }, if registry.is_some() => {
                            if change.is_err() { return; }
                            match (controller.as_ref(), service.schedule_controller()) {
                                (None, None) => {}
                                (Some(old), Some(current)) if std::sync::Arc::ptr_eq(old, &current) => {}
                                _ => break,
                            }
                            match (navigation.as_ref(), service.navigation()) {
                                (None, None) => {},
                                (Some(old), Some(current)) if std::sync::Arc::ptr_eq(old, &current) => {},
                                _ => break,
                            }
                        }
                    }
                }
            }
        }
        .take_until(cancelled)
        .boxed())
    }
}
