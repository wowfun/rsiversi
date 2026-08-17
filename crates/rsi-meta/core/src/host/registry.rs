use super::plugin_control::{
    command_hash, plugin_candidate_lock_path, plugin_effect_command_id,
    plugin_provenance_command_id, validate_plugin_command_admission, write_plugin_candidate_lock,
};
mod retirement;
mod state;
use super::{
    Arc, ArcSwap, BTreeMap, BTreeSet, CasResult, Command, CommandEnvelope, CommandOutcome,
    CommandOutcomeEnvelope, CompositionChangeSource, CompositionDigest, CompositionFiles,
    CompositionMode, ContentHash, DesiredState, Event, EventEnvelope, GraphRevision, HostError,
    HostServiceCall, InstanceFingerprint, InstanceId, Persistence, PluginCommandRequest,
    PluginFrame, PluginInspection, PluginLoader, RegistryMessage, Result, RetirementRegistry,
    RoutingSnapshot, RuntimeFault, RuntimeLaunchContext, StdMutex, StoredCommand,
    SubscriptionStart, abort_prepared_reverse, affected_instances, broadcast, build_inspections,
    build_lock, composition_event, dependency_waves, include_affected_dependents, install_pair,
    instance_fingerprints, launch_and_prepare_pumping_services, mpsc,
    normalize_prepared_for_install, prepare_pair, publish_routing_cutover, read_optional_bytes,
    remove_file_and_sync_parent, resolve_prepared, restore_previous_pair,
};
use retirement::register_retirement_waves;
use state::{validate_state_key, validate_state_value};

pub(super) struct RegistryActor {
    pub(super) persistence: Persistence,
    pub(super) loader: PluginLoader,
    pub(super) routing: Arc<ArcSwap<RoutingSnapshot>>,
    pub(super) cutover: Arc<StdMutex<()>>,
    pub(super) events: broadcast::Sender<EventEnvelope>,
    pub(super) current_hashes: Option<(ContentHash, ContentHash)>,
    pub(super) installed_files: Option<CompositionFiles>,
    pub(super) next_generation: u64,
    pub(super) current_fingerprints: BTreeMap<InstanceId, InstanceFingerprint>,
    pub(super) plugin_inspections: BTreeMap<InstanceId, PluginInspection>,
    pub(super) launch_context: RuntimeLaunchContext,
    pub(super) plugin_command_receiver: mpsc::Receiver<PluginCommandRequest>,
    pub(super) host_service_receiver: mpsc::Receiver<HostServiceCall>,
    pub(super) runtime_fault_receiver: mpsc::Receiver<RuntimeFault>,
    pub(super) retirements: RetirementRegistry,
    pub(super) current_mode: CompositionMode,
    pub(super) fatal: bool,
    pub(super) _workspace_lease: crate::workspace::WorkspaceLease,
}

impl RegistryActor {
    pub(super) async fn run(mut self, mut receiver: mpsc::Receiver<RegistryMessage>) {
        loop {
            tokio::select! {
                message = receiver.recv() => {
                    let Some(message) = message else {
                        break;
                    };
                    match message {
                        RegistryMessage::Subscribe { reply } => {
                            let live = self.events.subscribe();
                            let result = self.persistence.latest_cursor().map(|through_cursor| {
                                SubscriptionStart {
                                    live,
                                    through_cursor,
                                }
                            });
                            let _ = reply.send(result);
                        }
                        RegistryMessage::ReplayEvents {
                            after_cursor,
                            through_cursor,
                            limit,
                            reply,
                        } => {
                            let result = self.persistence.query_events_through(
                                after_cursor,
                                through_cursor,
                                limit,
                            );
                            let _ = reply.send(result);
                        }
                        RegistryMessage::InspectPlugin { instance_id, reply } => {
                            let _ = reply.send(self.plugin_inspections.get(&instance_id).cloned());
                        }
                        RegistryMessage::Submit { command, reply } => {
                            let (result, stop) = self.handle_command(command).await;
                            let _ = reply.send(result);
                            if stop {
                                break;
                            }
                        }
                        #[cfg(test)]
                        RegistryMessage::Pause { entered, release } => {
                            let _ = entered.send(());
                            let _ = release.await;
                        }
                    }
                }
                Some(mut command) = self.plugin_command_receiver.recv() => {
                    let reply = command.reply.take();
                    let (result, stop) = self.handle_plugin_command(command).await;
                    if let Some(reply) = reply {
                        let _ = reply.send(result);
                    }
                    if stop {
                        break;
                    }
                }
                Some(call) = self.host_service_receiver.recv() => self.handle_host_service(call),
                Some(fault) = self.runtime_fault_receiver.recv() => self.handle_runtime_fault(fault),
                else => break,
            }
        }
        if !self.fatal {
            self.stop_current_runtimes().await;
        }
    }

    fn handle_runtime_fault(&mut self, fault: RuntimeFault) {
        let snapshot = self.routing.load_full();
        let Some(generation) = snapshot.generation(&fault.instance) else {
            return;
        };
        if generation.id != fault.generation || generation.has_healthy_runtime() {
            return;
        }
        generation.stop_admission();
        let command_id = format!("system:runtime-fault:{}", fault.generation);
        let event = match self.persistence.append_event(
            snapshot.graph().composition_id.as_str(),
            &command_id,
            snapshot.revision(),
            Event::RuntimeFaulted {
                instance_id: fault.instance,
                reason: fault.reason,
            },
        ) {
            Ok(event) => event,
            Err(error) => {
                tracing::error!(%error, "failed to persist runtime fault; stopping admission");
                self.fail_stop_admission();
                return;
            }
        };
        let _cutover = self.cutover.lock().expect("routing cutover mutex poisoned");
        let mut updated = (*self.routing.load_full()).clone();
        updated.set_event_cursor(event.cursor);
        self.routing.store(Arc::new(updated));
        let _ = self.events.send(event);
    }

    async fn handle_command(
        &mut self,
        command: CommandEnvelope,
    ) -> (Result<CommandOutcomeEnvelope>, bool) {
        self.handle_command_from(command, CompositionChangeSource::Apply)
            .await
    }

    #[allow(clippy::too_many_lines)]
    async fn handle_command_from(
        &mut self,
        command: CommandEnvelope,
        apply_source: CompositionChangeSource,
    ) -> (Result<CommandOutcomeEnvelope>, bool) {
        let request_hash = match command_hash(&command) {
            Ok(hash) => hash,
            Err(error) => return (Err(error), false),
        };
        match self.persistence.find_command(&command.command_id) {
            Ok(Some(StoredCommand::Terminal {
                request_hash: stored_hash,
                outcome,
            })) if stored_hash == request_hash => {
                // A terminal row is the exactly-once boundary. Its outcome is
                // replayable, while lifecycle effects belong exclusively to
                // the fresh execution branch below.
                return (Ok(*outcome), false);
            }
            Ok(Some(StoredCommand::Pending {
                request_hash: stored_hash,
            })) if stored_hash == request_hash => {
                return (
                    Err(HostError::InvalidEnvelope(format!(
                        "command {:?} is pending crash recovery",
                        command.command_id
                    ))),
                    false,
                );
            }
            Ok(Some(StoredCommand::Expired {
                request_hash: stored_hash,
                classification,
            })) if stored_hash == request_hash => {
                return (
                    Err(HostError::OperationRejected {
                        code: "operation_expired".to_owned(),
                        message: format!(
                            "operation result expired after terminal state {classification:?}"
                        ),
                        details: BTreeMap::new(),
                    }),
                    false,
                );
            }
            Ok(Some(_)) => {
                return (
                    Err(HostError::CommandIdConflict {
                        command_id: command.command_id,
                    }),
                    false,
                );
            }
            Ok(None) => {}
            Err(error) => return (Err(error), false),
        }

        let current_revision = self.routing.load().revision();
        if let Some(expected) = command.expected_graph_revision
            && expected != current_revision
        {
            let outcome = CommandOutcomeEnvelope::rejected(
                command.command_id.clone(),
                current_revision,
                "graph_revision_mismatch",
                format!(
                    "expected graph revision {}, current is {}",
                    expected.0, current_revision.0
                ),
            );
            let result = self
                .persistence
                .store_outcome(
                    self.routing.load().graph().composition_id.as_str(),
                    &command.command_id,
                    &request_hash,
                    &outcome,
                )
                .map(|()| outcome);
            return (result, false);
        }

        let command_id = command.command_id;
        let result = match command.payload {
            Command::ApplyManifestPath {
                manifest_path,
                lock_path,
            } => {
                self.apply(
                    &command_id,
                    &request_hash,
                    &CompositionFiles::new(manifest_path, lock_path),
                    apply_source,
                )
                .await
            }
            Command::RotateToken => self.rotate_token(&command_id, &request_hash, current_revision),
            Command::Shutdown => {
                let outcome = CommandOutcomeEnvelope::new(
                    command_id.clone(),
                    current_revision,
                    CommandOutcome::ShuttingDown,
                );
                match self.persistence.commit_event_and_outcome(
                    self.routing.load().graph().composition_id.as_str(),
                    &command_id,
                    &request_hash,
                    current_revision,
                    Event::HostShuttingDown,
                    &outcome,
                ) {
                    Ok(event) => {
                        let _ = self.events.send(event);
                        Ok(outcome)
                    }
                    Err(error) => Err(error),
                }
            }
            Command::Unknown { command_type, .. } => self.store_rejection(
                &command_id,
                &request_hash,
                "unsupported_command",
                format!("command type {command_type:?} is not supported by this host"),
            ),
        };
        let stop = self.fatal
            || matches!(&result, Ok(outcome) if matches!(
                outcome.payload,
                CommandOutcome::ShuttingDown
            ))
            || matches!(&result, Err(HostError::PairRestoreFailed { .. }));
        (result, stop)
    }

    #[allow(clippy::too_many_lines)]
    async fn handle_plugin_command(
        &mut self,
        request: PluginCommandRequest,
    ) -> (Result<CommandOutcomeEnvelope>, bool) {
        let revision = self.routing.load().revision();
        let provenance_id = plugin_provenance_command_id(&request, revision);
        let mut audit_envelope = request.envelope.clone();
        audit_envelope.command_id.clone_from(&provenance_id);
        audit_envelope.expected_graph_revision = Some(revision);

        let reject = |host: &mut Self, code: &'static str, message: String| {
            host.record_plugin_rejection(&audit_envelope, code, message)
        };
        let installed = match validate_plugin_command_admission(
            &self.routing.load(),
            self.current_mode,
            &self.plugin_inspections,
            self.installed_files.as_ref(),
            &request,
        ) {
            Ok(installed) => installed,
            Err(rejection) => {
                return (
                    reject(self, rejection.code, rejection.message.to_owned()),
                    false,
                );
            }
        };

        // Rebuild before deriving the durable effect identity. A plugin-local
        // command ID is only a correlation handle; changed manifest/artifact
        // content must create a distinct operation instead of replaying an old
        // result for a new candidate.
        let rebuilt = match build_lock(&installed.manifest_path, &self.loader) {
            Ok(lock) => lock,
            Err(error) => {
                return (
                    reject(
                        self,
                        "plugin_command_lock_failed",
                        format!("cannot resolve plugin-origin candidate lock: {error}"),
                    ),
                    false,
                );
            }
        };
        let effect_id = match plugin_effect_command_id(&request, &rebuilt) {
            Ok(effect_id) => effect_id,
            Err(error) => {
                return (
                    reject(
                        self,
                        "plugin_command_lock_failed",
                        format!("cannot identify plugin-origin candidate lock: {error}"),
                    ),
                    false,
                );
            }
        };
        let candidate_lock = plugin_candidate_lock_path(&installed.lock_path, &effect_id);
        let mut effective = request.envelope;
        effective.command_id = effect_id;
        effective.expected_graph_revision = None;
        effective.payload = Command::ApplyManifestPath {
            manifest_path: installed.manifest_path.clone(),
            lock_path: candidate_lock.clone(),
        };
        match self.replay_plugin_effect(&effective) {
            Ok(Some(outcome)) => return (Ok(outcome), false),
            Ok(None) => {}
            Err(error) => return (Err(error), false),
        }
        if let Err(error) = write_plugin_candidate_lock(&candidate_lock, &rebuilt) {
            return (
                reject(
                    self,
                    "plugin_command_lock_failed",
                    format!("cannot publish plugin-origin candidate lock: {error}"),
                ),
                false,
            );
        }
        let result = self
            .handle_command_from(effective, CompositionChangeSource::PluginApply)
            .await;
        let _ = remove_file_and_sync_parent(&candidate_lock);
        result
    }

    fn record_plugin_rejection(
        &mut self,
        command: &CommandEnvelope,
        code: &str,
        message: String,
    ) -> Result<CommandOutcomeEnvelope> {
        let request_hash = command_hash(command)?;
        match self.persistence.find_command(&command.command_id)? {
            Some(StoredCommand::Terminal {
                request_hash: stored_hash,
                outcome,
            }) if stored_hash == request_hash => Ok(*outcome),
            Some(StoredCommand::Pending {
                request_hash: stored_hash,
            }) if stored_hash == request_hash => Err(HostError::InvalidEnvelope(format!(
                "plugin command {:?} is pending crash recovery",
                command.command_id
            ))),
            Some(_) => Err(HostError::CommandIdConflict {
                command_id: command.command_id.clone(),
            }),
            None => self.store_rejection(&command.command_id, &request_hash, code, message),
        }
    }

    fn replay_plugin_effect(
        &self,
        command: &CommandEnvelope,
    ) -> Result<Option<CommandOutcomeEnvelope>> {
        let request_hash = command_hash(command)?;
        match self.persistence.find_command(&command.command_id)? {
            Some(StoredCommand::Terminal {
                request_hash: stored_hash,
                outcome,
            }) if stored_hash == request_hash => Ok(Some(*outcome)),
            Some(StoredCommand::Pending {
                request_hash: stored_hash,
            }) if stored_hash == request_hash => Err(HostError::InvalidEnvelope(format!(
                "plugin effect {:?} is pending crash recovery",
                command.command_id
            ))),
            Some(_) => Err(HostError::CommandIdConflict {
                command_id: command.command_id.clone(),
            }),
            None => Ok(None),
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn apply(
        &mut self,
        command_id: &str,
        request_hash: &[u8],
        files: &CompositionFiles,
        source: CompositionChangeSource,
    ) -> Result<CommandOutcomeEnvelope> {
        let requested_desired = DesiredState {
            manifest_sha256: read_optional_bytes(&files.manifest_path)?
                .map(ContentHash::digest)
                .map(|hash| hash.to_string()),
            lock_sha256: read_optional_bytes(&files.lock_path)?
                .map(ContentHash::digest)
                .map(|hash| hash.to_string()),
            applied: false,
            last_rejection_code: None,
            plugin_restart_requested: false,
        };
        self.persistence.reserve_apply(
            self.routing.load().graph().composition_id.as_str(),
            command_id,
            request_hash,
            &requested_desired,
            self.routing.load().revision(),
        )?;
        let mut prepared = match prepare_pair(files, &self.loader, false) {
            Ok(prepared) => prepared,
            Err(error) => {
                return self.finish_reserved_rejection(
                    command_id,
                    "apply_prepare_failed",
                    error.to_string(),
                );
            }
        };
        if let Err(error) = normalize_prepared_for_install(&mut prepared) {
            return self.finish_reserved_rejection(
                command_id,
                "apply_prepare_failed",
                error.to_string(),
            );
        }
        let current_runtimes_healthy = self
            .routing
            .load()
            .generations()
            .all(|generation| generation.has_healthy_runtime());
        if self.current_hashes == Some((prepared.manifest_hash, prepared.lock_hash))
            && current_runtimes_healthy
        {
            let desired = DesiredState {
                manifest_sha256: Some(prepared.manifest_hash.to_string()),
                lock_sha256: Some(prepared.lock_hash.to_string()),
                applied: true,
                last_rejection_code: None,
                plugin_restart_requested: false,
            };
            return self.finish_reserved(command_id, CommandOutcome::NoChange, &desired);
        }

        let revision = GraphRevision(self.routing.load().revision().0.saturating_add(1));
        let old_routing = self.routing.load_full();
        let old_dependency_waves = match dependency_waves(old_routing.graph()) {
            Ok(waves) => waves,
            Err(error) => {
                return self.finish_reserved_rejection(
                    command_id,
                    "graph_validation_failed",
                    error.to_string(),
                );
            }
        };
        let mut preview_generation = self.next_generation;
        let preview = match resolve_prepared(&prepared, revision, None, &mut preview_generation) {
            Ok(routing) => routing,
            Err(error) => {
                return self.finish_reserved_rejection(
                    command_id,
                    "graph_validation_failed",
                    error.to_string(),
                );
            }
        };
        let candidate_fingerprints = instance_fingerprints(&prepared);
        let mut affected = affected_instances(
            &self.current_fingerprints,
            &candidate_fingerprints,
            old_routing.graph(),
            preview.graph(),
        );
        affected.extend(
            old_routing
                .generations()
                .filter(|generation| !generation.has_healthy_runtime())
                .map(|generation| generation.instance.clone()),
        );
        include_affected_dependents(&mut affected, old_routing.graph(), preview.graph());
        let reusable: BTreeMap<_, _> = old_routing
            .generations()
            .filter(|generation| !affected.contains(&generation.instance))
            .filter(|generation| generation.has_healthy_runtime())
            .map(|generation| (generation.instance.clone(), Arc::clone(generation)))
            .collect();
        let mut candidate_next_generation = self.next_generation;
        let mut routing = match resolve_prepared(
            &prepared,
            revision,
            Some(&reusable),
            &mut candidate_next_generation,
        ) {
            Ok(routing) => routing,
            Err(error) => {
                return self.finish_reserved_rejection(
                    command_id,
                    "graph_validation_failed",
                    error.to_string(),
                );
            }
        };
        let restart_packages: BTreeSet<_> = affected
            .iter()
            .filter_map(|instance| {
                candidate_fingerprints
                    .get(instance)
                    .or_else(|| self.current_fingerprints.get(instance))
            })
            .filter(|fingerprint| fingerprint.process_fixed)
            .map(|fingerprint| fingerprint.package_id.clone())
            .collect();
        let outcome =
            CommandOutcomeEnvelope::new(command_id.to_owned(), revision, CommandOutcome::Applied);
        let installed_files = self
            .installed_files
            .clone()
            .unwrap_or_else(|| files.clone());

        if !restart_packages.is_empty() {
            let packages: Vec<_> = restart_packages.into_iter().collect();
            if source == CompositionChangeSource::PluginApply {
                return self.finish_reserved_rejection(
                    command_id,
                    "process_fixed_requires_external_install",
                    "plugin-origin apply cannot cross a process-fixed install boundary".to_owned(),
                );
            }
            let candidate = CompositionDigest {
                composition_id: prepared.manifest.composition.id.clone(),
                manifest_sha256: prepared.manifest_hash.to_string(),
                lock_sha256: prepared.lock_hash.to_string(),
            };
            let outcome = CommandOutcomeEnvelope::new(
                command_id.to_owned(),
                old_routing.revision(),
                CommandOutcome::RestartRequired {
                    current: self.current_digest(),
                    candidate,
                    packages,
                },
            );
            self.persistence
                .finish_pending_outcome(command_id, &outcome, None)?;
            return Ok(outcome);
        }

        let preflight_manifest_hash = prepared.manifest_hash;
        let preflight_lock_hash = prepared.lock_hash;
        let preflight_fingerprints = prepared.fingerprints.clone();
        let mut staged = match prepare_pair(files, &self.loader, true) {
            Ok(prepared) => prepared,
            Err(error) => {
                self.persistence.abandon_uncommitted_operation(command_id)?;
                return Err(error);
            }
        };
        if let Err(error) = normalize_prepared_for_install(&mut staged) {
            self.persistence.abandon_uncommitted_operation(command_id)?;
            return Err(error);
        }
        if staged.manifest_hash != preflight_manifest_hash
            || staged.lock_hash != preflight_lock_hash
            || staged.fingerprints != preflight_fingerprints
        {
            return self.finish_reserved_rejection(
                command_id,
                "candidate_changed_during_apply",
                "candidate inputs changed between preflight and staging".to_owned(),
            );
        }
        prepared = staged;

        let waves = match dependency_waves(routing.graph()) {
            Ok(waves) => waves,
            Err(error) => {
                return self.finish_reserved_rejection(
                    command_id,
                    "graph_validation_failed",
                    error.to_string(),
                );
            }
        };
        let prepared_runtimes = match launch_and_prepare_pumping_services(
            &self.loader,
            &prepared.manifest.composition.id,
            &routing,
            &prepared.runtimes,
            &waves,
            &self.launch_context,
            &mut self.host_service_receiver,
            &mut self.persistence,
        )
        .await
        {
            Ok(runtimes) => runtimes,
            Err(error) => {
                if matches!(
                    error,
                    HostError::PluginRuntimeClosed { .. }
                        | HostError::PluginQueueFull { .. }
                        | HostError::PluginCallFailed { .. }
                        | HostError::PluginLifecycleTimeout { .. }
                ) {
                    self.persistence.abandon_uncommitted_operation(command_id)?;
                    return Err(error);
                }
                return self.finish_reserved_rejection(
                    command_id,
                    "plugin_prepare_failed",
                    error.to_string(),
                );
            }
        };
        #[cfg(feature = "test-failpoints")]
        crate::test_failpoints::gate(
            command_id,
            crate::test_failpoints::CrashPoint::PreparedBeforeJournal,
        );
        let applied_desired = DesiredState {
            manifest_sha256: Some(prepared.manifest_hash.to_string()),
            lock_sha256: Some(prepared.lock_hash.to_string()),
            applied: true,
            last_rejection_code: None,
            plugin_restart_requested: false,
        };
        let commit_event = composition_event(&prepared, &routing, source);
        if let Err(error) = self.persistence.begin_apply(
            "apply",
            command_id,
            &prepared.manifest.composition.id,
            &installed_files.manifest_path,
            &installed_files.lock_path,
            &files.manifest_path,
            &files.lock_path,
            &prepared.manifest_hash.to_string(),
            &prepared.lock_hash.to_string(),
            revision,
            &commit_event,
            &outcome,
            &applied_desired,
        ) {
            abort_prepared_reverse(&prepared_runtimes).await;
            return self.finish_reserved_rejection(
                command_id,
                "apply_journal_failed",
                error.to_string(),
            );
        }
        if let Err(error) = install_pair(&prepared, &installed_files, command_id) {
            abort_prepared_reverse(&prepared_runtimes).await;
            let rejected = CommandOutcomeEnvelope::rejected(
                command_id,
                self.routing.load().revision(),
                "lock_install_failed",
                error.to_string(),
            );
            self.restore_pending_apply(command_id)?;
            let rejected_desired = self.persistence.desired_state()?;
            self.persistence
                .abort_pending_apply(command_id, &rejected, &rejected_desired)?;
            return Ok(rejected);
        }
        let event = match self.persistence.commit_pending_apply(
            command_id,
            revision,
            commit_event,
            &outcome,
            &applied_desired,
        ) {
            Ok(event) => event,
            Err(error) => {
                abort_prepared_reverse(&prepared_runtimes).await;
                return self.reject_installed_apply(
                    command_id,
                    "apply_commit_failed",
                    error.to_string(),
                );
            }
        };
        #[cfg(feature = "test-failpoints")]
        crate::test_failpoints::gate(
            command_id,
            crate::test_failpoints::CrashPoint::TerminalCommittedBeforePublish,
        );
        routing.set_event_cursor(event.cursor);
        routing.set_token_generation(old_routing.token_generation());
        routing.set_active(Some(CompositionDigest {
            composition_id: prepared.manifest.composition.id.clone(),
            manifest_sha256: prepared.manifest_hash.to_string(),
            lock_sha256: prepared.lock_hash.to_string(),
        }));
        let inspections = build_inspections(&prepared, &routing);
        let retired: Vec<_> = old_routing
            .generations()
            .filter(|old_generation| {
                routing
                    .generation(&old_generation.instance)
                    .is_none_or(|new_generation| !Arc::ptr_eq(old_generation, new_generation))
            })
            .cloned()
            .collect();
        let retired_by_instance: BTreeMap<_, _> = retired
            .iter()
            .map(|generation| (generation.instance.clone(), Arc::clone(generation)))
            .collect();
        let retirement_waves = old_dependency_waves
            .iter()
            .rev()
            .map(|wave| {
                wave.iter()
                    .filter_map(|instance| retired_by_instance.get(instance).cloned())
                    .collect::<Vec<_>>()
            })
            .filter(|wave| !wave.is_empty())
            .collect();
        let committed = futures_util::future::join_all(
            prepared_runtimes
                .iter()
                .map(crate::runtime::RuntimeHandle::committed),
        )
        .await;
        if let Some(error) = committed.into_iter().find_map(Result::err) {
            self.fail_stop_admission();
            return Err(HostError::PostCommitLifecycleFailure {
                message: error.to_string(),
            });
        }
        for runtime in &prepared_runtimes {
            if let Some(generation) = routing.generation(runtime.instance()) {
                generation.mark_admitting();
            }
        }
        publish_routing_cutover(
            &self.cutover,
            &self.routing,
            &old_routing,
            &retired,
            routing,
            || {},
        );
        register_retirement_waves(&self.retirements, retirement_waves);
        self.current_hashes = Some((prepared.manifest_hash, prepared.lock_hash));
        self.installed_files = Some(installed_files);
        self.next_generation = candidate_next_generation;
        self.current_fingerprints = candidate_fingerprints;
        self.plugin_inspections = inspections;
        self.current_mode = prepared.manifest.composition.mode;
        let _ = self.events.send(event);
        Ok(outcome)
    }

    fn fail_stop_admission(&mut self) {
        self.fatal = true;
        let _cutover = self.cutover.lock().expect("routing cutover mutex poisoned");
        let snapshot = self.routing.load_full();
        snapshot.stop_admission();
        for generation in snapshot.generations() {
            generation.stop_admission();
        }
        let retirements = self
            .retirements
            .lock()
            .expect("retirement registry mutex poisoned");
        for retirement in retirements.values() {
            let _ = retirement.cancel.send(true);
        }
    }

    async fn stop_current_runtimes(&mut self) {
        let runtimes: Vec<_> = {
            let _cutover = self.cutover.lock().expect("routing cutover mutex poisoned");
            let snapshot = self.routing.load_full();
            snapshot.stop_admission();
            snapshot
                .generations()
                .filter_map(|generation| {
                    generation.stop_admission();
                    generation.runtime_opt().cloned()
                })
                .collect()
        };
        let mut retirement_done = {
            let retirements = self
                .retirements
                .lock()
                .expect("retirement registry mutex poisoned");
            retirements
                .values()
                .map(|entry| {
                    let _ = entry.cancel.send(true);
                    entry.done.clone()
                })
                .collect::<Vec<_>>()
        };
        let stop_current = futures_util::future::join_all(
            runtimes.iter().map(crate::runtime::RuntimeHandle::stop),
        );
        let stop_retiring = async move {
            for done in &mut retirement_done {
                while !*done.borrow() {
                    if done.changed().await.is_err() {
                        break;
                    }
                }
            }
        };
        let _ = tokio::join!(stop_current, stop_retiring);
    }

    fn handle_host_service(&mut self, call: HostServiceCall) {
        let result = execute_host_service(&mut self.persistence, &call);
        let _ = call.reply.send(result);
    }

    fn finish_reserved(
        &mut self,
        command_id: &str,
        payload: CommandOutcome,
        desired: &DesiredState,
    ) -> Result<CommandOutcomeEnvelope> {
        let outcome = CommandOutcomeEnvelope::new(
            command_id.to_owned(),
            self.routing.load().revision(),
            payload,
        );
        self.persistence
            .finish_pending_outcome(command_id, &outcome, Some(desired))?;
        Ok(outcome)
    }

    fn finish_reserved_rejection(
        &mut self,
        command_id: &str,
        code: &str,
        message: String,
    ) -> Result<CommandOutcomeEnvelope> {
        let outcome = CommandOutcomeEnvelope::rejected(
            command_id,
            self.routing.load().revision(),
            code,
            message,
        );
        self.persistence
            .finish_pending_outcome(command_id, &outcome, None)?;
        Ok(outcome)
    }

    fn restore_pending_apply(&self, command_id: &str) -> Result<()> {
        let pending = self
            .persistence
            .pending_applies()?
            .into_iter()
            .find(|pending| pending.command_id == command_id)
            .ok_or_else(|| {
                HostError::InvalidEnvelope(format!(
                    "pending apply journal {command_id:?} disappeared before restore"
                ))
            })?;
        restore_previous_pair(&pending).map_err(|error| HostError::PairRestoreFailed {
            command_id: command_id.to_owned(),
            message: error.to_string(),
        })
    }

    fn reject_installed_apply(
        &mut self,
        command_id: &str,
        code: &str,
        message: String,
    ) -> Result<CommandOutcomeEnvelope> {
        self.restore_pending_apply(command_id)?;
        let desired = self.persistence.desired_state()?;
        let outcome = CommandOutcomeEnvelope::rejected(
            command_id,
            self.routing.load().revision(),
            code,
            message,
        );
        self.persistence
            .abort_pending_apply(command_id, &outcome, &desired)?;
        Ok(outcome)
    }

    fn current_digest(&self) -> Option<CompositionDigest> {
        let (manifest, lock) = self.current_hashes?;
        Some(CompositionDigest {
            composition_id: self.routing.load().graph().composition_id.clone(),
            manifest_sha256: manifest.to_string(),
            lock_sha256: lock.to_string(),
        })
    }

    fn rotate_token(
        &mut self,
        command_id: &str,
        request_hash: &[u8],
        graph_revision: GraphRevision,
    ) -> Result<CommandOutcomeEnvelope> {
        let outcome = self.persistence.allocate_token_generation(
            self.routing.load().graph().composition_id.as_str(),
            command_id,
            request_hash,
            graph_revision,
        )?;
        let CommandOutcome::TokenRotated { generation } = outcome.payload else {
            unreachable!("token allocator returns TokenRotated");
        };
        let mut routing = (*self.routing.load_full()).clone();
        routing.set_token_generation(generation);
        self.routing.store(Arc::new(routing));
        Ok(outcome)
    }

    fn store_rejection(
        &mut self,
        command_id: &str,
        request_hash: &[u8],
        code: &str,
        message: String,
    ) -> Result<CommandOutcomeEnvelope> {
        let outcome = CommandOutcomeEnvelope::rejected(
            command_id,
            self.routing.load().revision(),
            code,
            message,
        );
        self.persistence.store_outcome(
            self.routing.load().graph().composition_id.as_str(),
            command_id,
            request_hash,
            &outcome,
        )?;
        Ok(outcome)
    }
}

pub(crate) fn execute_host_service(
    persistence: &mut Persistence,
    call: &HostServiceCall,
) -> Result<PluginFrame> {
    if call.service != crate::runtime::STATE_SERVICE {
        return Err(HostError::Unsupported("unknown host-owned service"));
    }
    let key = validate_state_key(&call.payload)?;
    let response = match call.operation.as_str() {
        "get" => {
            let current =
                persistence.get_plugin_state(&call.composition_id, &call.instance_id, key)?;
            let (version, value) = current.map_or((0, serde_json::Value::Null), |entry| {
                (
                    entry.version,
                    entry.value.unwrap_or(serde_json::Value::Null),
                )
            });
            PluginFrame::service_event(
                Some(call.request_id.clone()),
                &call.service,
                "value",
                serde_json::json!({"key": key, "version": version, "value": value}),
            )
        }
        "compare_and_swap" | "delete" => {
            let expected = call
                .payload
                .get("expected_version")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| {
                    HostError::InvalidEnvelope("state.cas expected_version is missing".to_owned())
                })?;
            let value = if call.operation == "delete" {
                None
            } else {
                Some(call.payload.get("value").cloned().ok_or_else(|| {
                    HostError::InvalidEnvelope("state.cas value is missing".to_owned())
                })?)
            };
            if let Some(value) = &value {
                validate_state_value(value)?;
            }
            let result = persistence.compare_and_swap_plugin_state(
                &call.composition_id,
                &call.instance_id,
                key,
                (expected != 0).then_some(expected),
                value.as_ref(),
            )?;
            let (event, state) = match result {
                CasResult::Applied(state) => (
                    if call.operation == "delete" {
                        "deleted"
                    } else {
                        "applied"
                    },
                    Some(state),
                ),
                CasResult::Conflict(state) => ("conflict", state),
            };
            let (version, value) = state.map_or((0, serde_json::Value::Null), |entry| {
                (
                    entry.version,
                    entry.value.unwrap_or(serde_json::Value::Null),
                )
            });
            PluginFrame::service_event(
                Some(call.request_id.clone()),
                &call.service,
                event,
                serde_json::json!({"key": key, "version": version, "value": value}),
            )
        }
        _ => {
            return Err(HostError::InvalidEnvelope(format!(
                "unknown state.cas operation {:?}",
                call.operation
            )));
        }
    };
    Ok(response)
}
