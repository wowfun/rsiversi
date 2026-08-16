use super::composition::{
    install_pair, normalize_prepared_for_install, prepare_pair, write_bytes_atomic,
};
use super::domain::{CompositionDigest, InstallRequest, InstallResult};
use super::host::{CompositionFiles, MAX_COMPOSITION_DOCUMENT_BYTES, project_files};
use super::model::DesiredState;
use super::persistence::{PendingEffect, Persistence, StoredCommand};
use super::protocol::{CommandOutcome, CommandOutcomeEnvelope, Event};
use super::{HostError, Result};
use rsi_meta_loader::{
    ContentHash, LoaderError, PluginLoader, read_bounded_file_following_symlinks,
};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions as FileOpenOptions};
use std::path::Path;

mod identity;

#[allow(clippy::too_many_lines)] // one journaled pair transaction keeps commit order visible
pub(crate) fn install_offline(request: &InstallRequest) -> Result<InstallResult> {
    request.operation_id.validate()?;
    let project_files = project_files(&request.project)?;
    let request_hash = identity::offline_install_hash(request, &project_files)?;
    let _lease = crate::workspace::WorkspaceLease::acquire(&request.workspace)?;
    let loader = PluginLoader::for_current_process(&request.workspace.cache_root);
    let mut persistence = Persistence::open(&request.workspace.database_path)?;
    recover_pending_applies(&mut persistence, &loader)?;

    match persistence.find_command(&request.operation_id.0)? {
        Some(StoredCommand::Terminal {
            request_hash: stored,
            outcome,
        }) if stored == request_hash => return install_result_from_outcome(outcome.payload),
        Some(StoredCommand::Legacy) => {
            return Err(HostError::OperationRejected {
                code: "legacy_operation_id".to_owned(),
                message: "operation id was used by a pre-v5 side effect".to_owned(),
                details: BTreeMap::new(),
            });
        }
        Some(StoredCommand::Pending {
            request_hash: stored,
        }) if stored == request_hash => {
            return Err(HostError::OperationRejected {
                code: "operation_recovery_incomplete".to_owned(),
                message: "offline install is still pending crash recovery".to_owned(),
                details: BTreeMap::new(),
            });
        }
        Some(_) => {
            return Err(HostError::OperationRejected {
                code: "operation_id_conflict".to_owned(),
                message: "operation id was already used with different parameters".to_owned(),
                details: BTreeMap::new(),
            });
        }
        None => {}
    }

    let mut prepared = prepare_pair(&project_files, &loader, false)?;
    normalize_prepared_for_install(&mut prepared)?;
    crate::workspace::require_fresh_process_for_changed_fixed(
        &request.workspace,
        &prepared.process_fixed_packages,
    )?;
    let candidate = CompositionDigest {
        composition_id: prepared.manifest.composition.id.clone(),
        manifest_sha256: prepared.manifest_hash.to_string(),
        lock_sha256: prepared.lock_hash.to_string(),
    };
    let graph_revision = persistence.latest_graph_revision()?;
    let current_files = crate::workspace::installed_files(&request.workspace)?;
    let unchanged = if let Some(installed) = current_files.as_ref() {
        read_optional_bytes(&installed.manifest_path)?.map(ContentHash::digest)
            == Some(prepared.manifest_hash)
            && read_optional_bytes(&installed.lock_path)?.map(ContentHash::digest)
                == Some(prepared.lock_hash)
    } else {
        false
    };
    if unchanged {
        let outcome = CommandOutcomeEnvelope::new(
            request.operation_id.0.clone(),
            graph_revision,
            CommandOutcome::Installed {
                candidate: candidate.clone(),
                changed: false,
            },
        );
        persistence.store_operation_outcome(
            &prepared.manifest.composition.id,
            &request.operation_id.0,
            "install",
            &request_hash,
            &outcome,
        )?;
        return Ok(InstallResult::Unchanged { candidate });
    }

    persistence.reserve_install(
        &prepared.manifest.composition.id,
        &request.operation_id.0,
        &request_hash,
        &project_files.manifest_path,
        &project_files.lock_path,
        &prepared.manifest_bytes,
        &prepared.lock_bytes,
        graph_revision,
    )?;
    let installed = CompositionFiles::new(
        &request.workspace.manifest_path,
        &request.workspace.lock_path,
    );
    let outcome = CommandOutcomeEnvelope::new(
        request.operation_id.0.clone(),
        graph_revision,
        CommandOutcome::Installed {
            candidate: candidate.clone(),
            changed: true,
        },
    );
    let desired = persistence.desired_state()?;
    persistence.begin_apply(
        "install",
        &request.operation_id.0,
        &prepared.manifest.composition.id,
        &installed.manifest_path,
        &installed.lock_path,
        &project_files.manifest_path,
        &project_files.lock_path,
        &prepared.manifest_hash.to_string(),
        &prepared.lock_hash.to_string(),
        graph_revision,
        &Event::HostShuttingDown,
        &outcome,
        &desired,
    )?;
    if let Err(error) = install_pair(&prepared, &installed, &request.operation_id.0) {
        if let Some(pending) = persistence
            .pending_applies()?
            .into_iter()
            .find(|pending| pending.command_id == request.operation_id.0)
        {
            restore_previous_pair(&pending)?;
        }
        let rejected = CommandOutcomeEnvelope::rejected(
            request.operation_id.0.clone(),
            graph_revision,
            "install_pair_failed",
            error.to_string(),
        );
        persistence.abort_pending_apply(&request.operation_id.0, &rejected, &desired)?;
        return Err(HostError::OperationRejected {
            code: "install_pair_failed".to_owned(),
            message: error.to_string(),
            details: BTreeMap::new(),
        });
    }
    #[cfg(feature = "test-failpoints")]
    crate::test_failpoints::gate(
        &request.operation_id.0,
        crate::test_failpoints::CrashPoint::LockPublishedBeforeTerminal,
    );
    persistence.commit_pending_install(&request.operation_id.0, &outcome)?;
    #[cfg(feature = "test-failpoints")]
    crate::test_failpoints::gate(
        &request.operation_id.0,
        crate::test_failpoints::CrashPoint::TerminalCommittedBeforePublish,
    );
    Ok(InstallResult::Installed { candidate })
}

fn install_result_from_outcome(outcome: CommandOutcome) -> Result<InstallResult> {
    match outcome {
        CommandOutcome::Installed { candidate, changed } => {
            if changed {
                Ok(InstallResult::Installed { candidate })
            } else {
                Ok(InstallResult::Unchanged { candidate })
            }
        }
        CommandOutcome::Rejected { code, message } => Err(HostError::OperationRejected {
            code,
            message,
            details: BTreeMap::new(),
        }),
        other => Err(HostError::InvalidEnvelope(format!(
            "stored install operation has incompatible result {other:?}"
        ))),
    }
}

pub(crate) fn recover_pending_applies(
    persistence: &mut Persistence,
    loader: &PluginLoader,
) -> Result<()> {
    recover_unjournaled_commands(persistence)?;
    for pending in persistence.pending_applies()? {
        let installed = CompositionFiles::new(
            &pending.installed_manifest_path,
            &pending.installed_lock_path,
        );
        let installed_lock_hash = read_optional_bytes(&installed.lock_path)?
            .as_ref()
            .map(ContentHash::digest)
            .map(|hash| hash.to_string());
        let installed_manifest_hash = read_optional_bytes(&installed.manifest_path)?
            .as_ref()
            .map(ContentHash::digest)
            .map(|hash| hash.to_string());
        let candidate_pair_installed = installed_lock_hash.as_deref()
            == Some(&pending.candidate_lock_hash)
            && installed_manifest_hash.as_deref() == Some(&pending.candidate_manifest_hash);
        if !candidate_pair_installed {
            restore_previous_pair(&pending)?;
            let is_install = pending.operation_kind == "install";
            let desired = if is_install {
                persistence.desired_state()?
            } else {
                DesiredState {
                    manifest_sha256: Some(pending.candidate_manifest_hash.clone()),
                    lock_sha256: Some(pending.candidate_lock_hash.clone()),
                    applied: false,
                    last_rejection_code: Some("apply_not_committed".to_owned()),
                    plugin_restart_requested: false,
                }
            };
            let code = if is_install {
                "install_not_committed"
            } else {
                "apply_not_committed"
            };
            let outcome = CommandOutcomeEnvelope::rejected(
                pending.command_id.clone(),
                persistence.latest_graph_revision()?,
                code,
                "candidate lock commit marker was not installed; previous pair was restored",
            );
            persistence.abort_pending_apply(&pending.command_id, &outcome, &desired)?;
            continue;
        }

        // The installed lock is the commit marker. Candidate source paths are
        // deliberately irrelevant here: they may be gone or may have changed.
        let prepared = prepare_pair(&installed, loader, pending.operation_kind != "install")?;
        if prepared.manifest_hash.to_string() != pending.candidate_manifest_hash
            || prepared.lock_hash.to_string() != pending.candidate_lock_hash
        {
            return Err(HostError::LockMismatch(format!(
                "installed pair for pending command {:?} does not match its commit marker",
                pending.command_id
            )));
        }
        if prepared.manifest.composition.id != pending.composition_id {
            return Err(HostError::LockMismatch(
                "installed composition id differs from the durable apply journal".to_owned(),
            ));
        }
        if pending.operation_kind == "install" {
            persistence.commit_pending_install(&pending.command_id, &pending.terminal_outcome)?;
        } else {
            persistence.commit_pending_apply(
                &pending.command_id,
                pending.terminal_graph_revision,
                pending.terminal_event.clone(),
                &pending.terminal_outcome,
                &pending.terminal_desired,
            )?;
        }
    }
    Ok(())
}

fn recover_unjournaled_commands(persistence: &mut Persistence) -> Result<()> {
    for pending in persistence.unjournaled_commands()? {
        match pending.effect {
            Some(PendingEffect::Lock {
                lock_path,
                lock,
                graph_revision,
            }) => {
                let expected = toml::to_string_pretty(&lock)
                    .map_err(|error| HostError::InvalidManifest(error.to_string()))?;
                let outcome = if read_bounded_file_following_symlinks(
                    &lock_path,
                    "read reserved lock",
                    MAX_COMPOSITION_DOCUMENT_BYTES,
                )
                .is_ok_and(|actual| actual == expected.as_bytes())
                {
                    CommandOutcomeEnvelope::new(
                        pending.command_id.clone(),
                        graph_revision,
                        CommandOutcome::LockResolved { lock },
                    )
                } else {
                    CommandOutcomeEnvelope::rejected(
                        pending.command_id.clone(),
                        graph_revision,
                        "lock_not_committed",
                        "lock target was not atomically published with the reserved command",
                    )
                };
                persistence.finish_pending_outcome(&pending.command_id, &outcome, None)?;
            }
            Some(PendingEffect::Apply {
                mut requested_desired,
                graph_revision,
            }) => {
                requested_desired.applied = false;
                requested_desired.last_rejection_code = Some("apply_not_committed".to_owned());
                let outcome = CommandOutcomeEnvelope::rejected(
                    pending.command_id.clone(),
                    graph_revision,
                    "apply_not_committed",
                    "apply was reserved but no filesystem commit journal was installed",
                );
                persistence.finish_pending_outcome(
                    &pending.command_id,
                    &outcome,
                    Some(&requested_desired),
                )?;
            }
            Some(PendingEffect::Install { graph_revision, .. }) => {
                let outcome = CommandOutcomeEnvelope::rejected(
                    pending.command_id.clone(),
                    graph_revision,
                    "install_not_committed",
                    "offline install did not publish an installed pair",
                );
                persistence.finish_pending_outcome(&pending.command_id, &outcome, None)?;
            }
            None => {
                let mut desired = persistence.desired_state()?;
                desired.applied = false;
                desired.last_rejection_code = Some("apply_not_committed".to_owned());
                let outcome = CommandOutcomeEnvelope::rejected(
                    pending.command_id.clone(),
                    persistence.latest_graph_revision()?,
                    "apply_not_committed",
                    "legacy pending command had no filesystem commit journal",
                );
                persistence.finish_pending_outcome(
                    &pending.command_id,
                    &outcome,
                    Some(&desired),
                )?;
            }
        }
    }
    Ok(())
}

pub(crate) fn restore_previous_pair(pending: &crate::persistence::PendingApply) -> Result<()> {
    match (
        pending.previous_manifest_bytes.as_deref(),
        pending.previous_lock_bytes.as_deref(),
    ) {
        (Some(manifest), Some(lock)) => {
            if pending.previous_manifest_hash.as_deref()
                != Some(ContentHash::digest(manifest).to_string().as_str())
                || pending.previous_lock_hash.as_deref()
                    != Some(ContentHash::digest(lock).to_string().as_str())
            {
                return Err(HostError::LockMismatch(
                    "previous apply journal backup hash is invalid".to_owned(),
                ));
            }
            write_bytes_atomic(&pending.installed_manifest_path, manifest)?;
            write_bytes_atomic(&pending.installed_lock_path, lock)
        }
        (None, None) => {
            remove_file_and_sync_parent(&pending.installed_lock_path)?;
            remove_file_and_sync_parent(&pending.installed_manifest_path)
        }
        _ => Err(HostError::LockMismatch(
            "apply journal contains only one half of the previous pair".to_owned(),
        )),
    }
}

pub(crate) fn read_optional_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    match read_bounded_file_following_symlinks(
        path,
        "read installed composition document",
        MAX_COMPOSITION_DOCUMENT_BYTES,
    ) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(LoaderError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn remove_file_and_sync_parent(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(HostError::Io {
                path: path.to_owned(),
                source,
            });
        }
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        FileOpenOptions::new()
            .read(true)
            .open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| HostError::Io {
                path: parent.to_owned(),
                source,
            })?;
    }
    Ok(())
}
