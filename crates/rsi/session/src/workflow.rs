use super::{Arc, HandleState, LocalSessionHandle, map_store_error, map_turn_error};
use rsi_agent_turn_protocol::{ProgramCancelReceipt, ResidentComposition};
use rsi_session_protocol::{
    MAXIMUM_WORKFLOW_FRAGMENT_BYTES, Result, SessionError, WorkflowCursor, WorkflowDetail,
    WorkflowList, WorkflowPage, WorkflowPlan, WorkflowRead, WorkflowReadiness,
    WorkflowReadinessSource, WorkflowResultFragment, WorkflowRuntimeKind, WorkflowRuntimeStatus,
    WorkflowTools,
};
pub(super) fn runtime(source: Option<&dyn WorkflowReadinessSource>) -> WorkflowRuntimeStatus {
    source.map_or(
        WorkflowRuntimeStatus {
            kind: WorkflowRuntimeKind::Absent,
            available: false,
            host_restart_required: false,
            desired_revision: 0,
            observed_revision: 0,
        },
        WorkflowReadinessSource::runtime,
    )
}
pub(super) fn require_preset(
    source: Option<&dyn WorkflowReadinessSource>,
    preset: &rsi_agent_session_protocol::AgentPresetId,
) -> Result<()> {
    if preset.as_str() == rsi_session_protocol::WORKFLOW_PRESET_ID
        && !source.is_some_and(WorkflowReadinessSource::available)
    {
        return Err(SessionError::WorkflowUnavailable(runtime(source).kind));
    }
    Ok(())
}
fn tools(
    manifest: Option<Arc<rsi_agent_composition_protocol::CompositionManifest>>,
) -> WorkflowTools {
    let Some(manifest) = manifest else {
        return WorkflowTools::Unavailable;
    };
    if manifest
        .instances()
        .iter()
        .any(|row| row.plugin == rsi_session_protocol::PROGRAM_TOOLS_PLUGIN_ID && row.enabled)
    {
        WorkflowTools::Enabled
    } else {
        WorkflowTools::Disabled
    }
}
impl LocalSessionHandle {
    pub(super) async fn read_workflow_readiness(&self) -> Result<WorkflowReadiness> {
        let _activity = self.begin_activity()?;
        let _admission = self.admit()?;
        self.reconcile_fresh_read().await?;
        let (tool_state, generation, fresh_plan) = {
            let state = self.state.lock().await;
            match &*state {
                HandleState::Fresh(draft) => (
                    tools(draft.composition().manifest()),
                    Some(draft.composition().source_digest().to_owned()),
                    Some(
                        draft
                            .baseline()
                            .initial_states()
                            .into_iter()
                            .find(|s| {
                                s.identity().id() == rsi_agent_plan_policy::PLAN_POLICY_DOMAIN
                            })
                            .and_then(|s| s.state().value().as_bool()),
                    ),
                ),
                HandleState::Attached(header) => match self
                    .projection_service
                    .resident_composition(self.session_id())
                    .map_err(map_turn_error)?
                {
                    ResidentComposition::Resident {
                        header: resident,
                        source_digest,
                        manifest,
                    } if resident.as_ref() == header.as_ref() => {
                        (tools(manifest), Some(source_digest), None)
                    }
                    ResidentComposition::NotResident => (WorkflowTools::NotResident, None, None),
                    _ => (WorkflowTools::Unavailable, None, None),
                },
                HandleState::Expired => return Err(SessionError::NotFound("draft lease".into())),
            }
        };
        let plan = if let Some(plan) = fresh_plan {
            plan
        } else {
            self.store
                .read_domain_states(self.session_id(), None)
                .await
                .ok()
                .and_then(|page| {
                    page.states
                        .into_iter()
                        .find(|s| {
                            s.snapshot.identity().id() == rsi_agent_plan_policy::PLAN_POLICY_DOMAIN
                        })
                        .and_then(|s| s.snapshot.state().value().as_bool())
                })
        };
        self.admit()?;
        Ok(WorkflowReadiness {
            runtime: runtime(self.workflow.as_deref()),
            tools: tool_state,
            plan: match plan {
                Some(true) => WorkflowPlan::On,
                Some(false) => WorkflowPlan::Off,
                None => WorkflowPlan::Unavailable,
            },
            local_execution: *self.coordinates.location()
                == rsi_workspace_protocol::ExecutionLocation::Local,
            generation,
        })
    }
    pub(super) async fn workflow_list(&self, request: WorkflowList) -> Result<WorkflowPage> {
        request.validate()?;
        let _activity = self.begin_activity()?;
        let _admission = self.admit()?;
        if !self.reconcile_fresh_read().await? {
            if request
                .cursor
                .as_ref()
                .is_some_and(|cursor| cursor.seed_control_seq != 0)
            {
                return Err(SessionError::Invalid(
                    "workflow seed exceeds draft control tail".into(),
                ));
            }
            return Ok(WorkflowPage {
                seed_control_seq: 0,
                runs: vec![],
                next: None,
            });
        }
        let watermark = self
            .store
            .read_watermarks(self.session_id())
            .await
            .map_err(map_store_error)?;
        let cursor = request.cursor.unwrap_or(WorkflowCursor {
            seed_control_seq: watermark.durable_control_seq,
            before_accepted_control_seq: None,
        });
        if cursor.seed_control_seq > watermark.durable_control_seq {
            return Err(SessionError::Invalid(
                "workflow seed exceeds durable control tail".into(),
            ));
        }
        let page = self
            .turns
            .list_session_programs(
                self.session_id(),
                cursor.seed_control_seq,
                cursor.before_accepted_control_seq,
                request.limit,
            )
            .await
            .map_err(map_turn_error)?;
        self.admit()?;
        let next = if page.has_more {
            Some(WorkflowCursor {
                seed_control_seq: cursor.seed_control_seq,
                before_accepted_control_seq: page.runs.last().map(|run| run.accepted_control_seq),
            })
        } else {
            None
        };
        Ok(WorkflowPage {
            seed_control_seq: cursor.seed_control_seq,
            runs: page.runs,
            next,
        })
    }
    pub(super) async fn workflow_detail(&self, request: WorkflowRead) -> Result<WorkflowDetail> {
        request.validate()?;
        let _activity = self.begin_activity()?;
        let _admission = self.admit()?;
        let details = self
            .turns
            .read_session_program(
                self.session_id(),
                &request.run_id,
                rsi_agent_turn_protocol::ProgramRead {
                    expected_control_seq: request.expected_control_seq,
                    children_offset: request.children_offset,
                    result: request.result_offset.is_some(),
                    script: request.script_offset.is_some(),
                },
            )
            .await
            .map_err(map_turn_error)?;
        let result = if let Some(offset) = request.result_offset {
            Some(fragment_page(
                details
                    .result
                    .as_ref()
                    .ok_or_else(|| SessionError::Invalid("workflow result unavailable".into()))?,
                details
                    .overview
                    .result_ref
                    .as_ref()
                    .ok_or_else(|| SessionError::Invalid("workflow has no result".into()))?,
                offset,
            )?)
        } else {
            None
        };
        let script = if let Some(offset) = request.script_offset {
            Some(fragment_page(
                details
                    .script
                    .as_ref()
                    .ok_or_else(|| SessionError::Invalid("workflow script unavailable".into()))?,
                &details.overview.script_ref,
                offset,
            )?)
        } else {
            None
        };
        self.admit()?;
        Ok(WorkflowDetail {
            run: details.overview,
            children: details.children,
            children_offset: details.children_offset,
            next_children_offset: details.next_children_offset,
            result,
            script,
        })
    }
    pub(super) async fn workflow_cancel(
        &self,
        run: &rsi_agent_session_protocol::ProgramRunId,
    ) -> Result<ProgramCancelReceipt> {
        self.draft_commands.cancel_workflow(self, run.clone()).await
    }
}

fn fragment_page(
    bytes: &[u8],
    binding: &rsi_agent_session_protocol::ProgramBlob,
    offset: usize,
) -> Result<WorkflowResultFragment> {
    let source = std::str::from_utf8(bytes)
        .map_err(|_| SessionError::Invalid("workflow CAS data is not UTF-8".into()))?;
    let end = rsi_tools_protocol::bounded_json_fragment_end(
        source,
        offset,
        MAXIMUM_WORKFLOW_FRAGMENT_BYTES,
        "",
        |end| WorkflowResultFragment {
            sha256: binding.sha256.clone(),
            offset,
            next_offset: (end < source.len()).then_some(end),
            fragment: &source[offset..end],
        },
    )
    .map_err(|error| SessionError::Invalid(error.to_string()))?;
    Ok(WorkflowResultFragment {
        sha256: binding.sha256.clone(),
        offset,
        next_offset: (end < source.len()).then_some(end),
        fragment: source[offset..end].to_owned(),
    })
}

#[cfg(test)]
mod fragment_tests {
    use super::*;
    #[test]
    fn typed_fragments_reconstruct_escaped_utf8_with_bounded_wire_pages() {
        let source = "中\n\"\\\u{1}".repeat(5_000);
        let binding = rsi_agent_session_protocol::ProgramBlob {
            sha256: "a".repeat(64),
            bytes: source.len() as u64,
        };
        let mut restored = String::new();
        let mut offset = 0;
        loop {
            let page = fragment_page(source.as_bytes(), &binding, offset).unwrap();
            assert!(serde_json::to_vec(&page).unwrap().len() <= MAXIMUM_WORKFLOW_FRAGMENT_BYTES);
            assert_eq!(page.offset, offset);
            assert_eq!(page.sha256, binding.sha256);
            restored.push_str(&page.fragment);
            if let Some(next) = page.next_offset {
                assert!(next > offset);
                offset = next;
            } else {
                break;
            }
        }
        assert_eq!(restored, source);
        assert!(fragment_page(source.as_bytes(), &binding, 1).is_err());
        let final_page = fragment_page(source.as_bytes(), &binding, source.len()).unwrap();
        assert!(final_page.fragment.is_empty() && final_page.next_offset.is_none());
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    #[derive(Debug)]
    struct Available;
    impl WorkflowReadinessSource for Available {
        fn available(&self) -> bool {
            true
        }
        fn runtime(&self) -> WorkflowRuntimeStatus {
            panic!("successful admission must not rebuild Profile observations")
        }
    }
    #[test]
    fn workflow_admission_only_checks_the_applied_supply() {
        let preset = rsi_agent_session_protocol::AgentPresetId::new(
            rsi_session_protocol::WORKFLOW_PRESET_ID,
        )
        .unwrap();
        require_preset(Some(&Available), &preset).unwrap();
        assert!(matches!(
            require_preset(None, &preset),
            Err(SessionError::WorkflowUnavailable(
                WorkflowRuntimeKind::Absent
            ))
        ));
    }
}
