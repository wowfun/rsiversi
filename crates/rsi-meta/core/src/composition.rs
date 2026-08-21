use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rsi_meta_loader::{
    ContentHash, ExpectedHashes, LoaderError, PluginLoader, PluginPackage, compile_config_schema,
    hash_regular_file, prepare_config_with_compiled_schema, read_bounded_file_following_symlinks,
    resolve_package_relative_file,
};
use serde::de::DeserializeOwned;
use tokio::sync::mpsc;

use crate::domain::{CompositionProject, LockResult};
use crate::host::{CompositionFiles, MAX_COMPOSITION_DOCUMENT_BYTES};
use crate::model::{
    CompositionLock, CompositionManifest, GraphRevision, GraphSnapshot, InstanceId, InstanceStatus,
    LockedPackage, PackageId, PackageSource, RoutingSnapshot, ServiceKey, ServiceRequirement,
    ValidationReport,
};
use crate::persistence::Persistence;
use crate::protocol::{CompositionChangeSource, Event, PluginInspection};
use crate::resolver::{ResolvedInstanceSpec, resolve};
use crate::runtime::{
    HostServiceCall, PreparedRuntimeInstance, RuntimeLaunchContext, launch_and_prepare,
};
use crate::{HostError, Result};

mod atomic_file;
mod package_files;
pub(crate) use atomic_file::{install_pair, write_bytes_atomic, write_lock_create_new};
use package_files::{
    config_schema_bytes as package_config_schema_bytes,
    config_schema_hash as package_config_schema_hash,
    config_schema_path as package_config_schema_path,
};

pub(crate) fn validate_project(project: &CompositionProject) -> Result<ValidationReport> {
    let loader = rsi_meta_loader::PluginLoader::for_current_process(std::env::temp_dir());
    validate_project_paths(
        &project.manifest_path,
        project.lock_path.as_deref(),
        &loader,
    )
}

pub(crate) fn lock_project(project: &CompositionProject) -> Result<LockResult> {
    let lock_path = project
        .lock_path
        .as_deref()
        .ok_or_else(|| HostError::OperationRejected {
            code: "lock_path_required".to_owned(),
            message: "locking a composition project requires lock_path".to_owned(),
            details: BTreeMap::new(),
        })?;
    let loader = rsi_meta_loader::PluginLoader::for_current_process(std::env::temp_dir());
    let lock = build_lock(&project.manifest_path, &loader)?;
    match write_lock_create_new(lock_path, &lock) {
        Ok(()) => Ok(LockResult::Created { lock }),
        Err(HostError::LockAlreadyExists { .. }) => {
            let (existing, _) = read_toml::<CompositionLock>(lock_path)?;
            if existing == lock {
                Ok(LockResult::Unchanged { lock })
            } else {
                Err(HostError::OperationRejected {
                    code: "lock_conflict".to_owned(),
                    message: format!(
                        "existing lock {} differs from the resolved candidate",
                        lock_path.display()
                    ),
                    details: BTreeMap::new(),
                })
            }
        }
        Err(error) => Err(error),
    }
}

pub(crate) struct PreparedComposition {
    pub(crate) manifest: CompositionManifest,
    pub(crate) lock: CompositionLock,
    pub(crate) manifest_bytes: Vec<u8>,
    pub(crate) manifest_hash: ContentHash,
    pub(crate) lock_hash: ContentHash,
    pub(crate) lock_bytes: Vec<u8>,
    resolved: Vec<ResolvedInstanceSpec>,
    pub(crate) runtimes: BTreeMap<InstanceId, PreparedRuntimeInstance>,
    pub(crate) process_fixed_packages: BTreeSet<(PackageId, String)>,
    pub(crate) fingerprints: BTreeMap<InstanceId, InstanceFingerprint>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InstanceFingerprint {
    pub(crate) semantic_hash: ContentHash,
    pub(crate) artifact_hash: ContentHash,
    pub(crate) process_fixed: bool,
    pub(crate) package_id: PackageId,
}

pub(crate) fn instance_fingerprints(
    prepared: &PreparedComposition,
) -> BTreeMap<InstanceId, InstanceFingerprint> {
    prepared.fingerprints.clone()
}

pub(crate) fn build_inspections(
    prepared: &PreparedComposition,
    routing: &RoutingSnapshot,
) -> BTreeMap<InstanceId, PluginInspection> {
    prepared
        .runtimes
        .iter()
        .filter_map(|(instance_id, runtime)| {
            routing
                .graph()
                .instances
                .get(instance_id)
                .cloned()
                .map(|instance| {
                    (
                        instance_id.clone(),
                        PluginInspection {
                            instance,
                            process_fixed: runtime.process_fixed,
                            capabilities: runtime.capabilities.clone(),
                            config_schema_path: runtime.config_schema_path.clone(),
                            config_schema: runtime.config_schema.clone(),
                        },
                    )
                })
        })
        .collect()
}

pub(crate) fn affected_instances(
    current: &BTreeMap<InstanceId, InstanceFingerprint>,
    candidate: &BTreeMap<InstanceId, InstanceFingerprint>,
    old_graph: &GraphSnapshot,
    new_graph: &GraphSnapshot,
) -> BTreeSet<InstanceId> {
    let mut affected: BTreeSet<_> = current
        .keys()
        .chain(candidate.keys())
        .filter(|instance| current.get(*instance) != candidate.get(*instance))
        .cloned()
        .collect();
    if old_graph.composition_id != new_graph.composition_id {
        affected.extend(current.keys().chain(candidate.keys()).cloned());
        return affected;
    }
    let old_routes: BTreeMap<_, _> = old_graph
        .bindings
        .iter()
        .map(|binding| {
            (
                (binding.consumer.clone(), binding.service.clone()),
                binding.provider.clone(),
            )
        })
        .collect();
    let new_routes: BTreeMap<_, _> = new_graph
        .bindings
        .iter()
        .map(|binding| {
            (
                (binding.consumer.clone(), binding.service.clone()),
                binding.provider.clone(),
            )
        })
        .collect();
    for route in old_routes.keys().chain(new_routes.keys()) {
        if old_routes.get(route) != new_routes.get(route) {
            affected.insert(route.0.clone());
        }
    }
    include_affected_dependents(&mut affected, old_graph, new_graph);
    affected
}

pub(crate) fn include_affected_dependents(
    affected: &mut BTreeSet<InstanceId>,
    old_graph: &GraphSnapshot,
    new_graph: &GraphSnapshot,
) {
    let mut dependents = BTreeMap::<&InstanceId, BTreeSet<&InstanceId>>::new();
    for binding in old_graph.bindings.iter().chain(&new_graph.bindings) {
        dependents
            .entry(&binding.provider)
            .or_default()
            .insert(&binding.consumer);
    }
    let mut pending: VecDeque<_> = affected.iter().cloned().collect();
    while let Some(provider) = pending.pop_front() {
        let Some(consumers) = dependents.get(&provider) else {
            continue;
        };
        for consumer in consumers {
            if affected.insert((*consumer).clone()) {
                pending.push_back((*consumer).clone());
            }
        }
    }
}

pub(crate) fn prepare_pair(
    files: &CompositionFiles,
    loader: &PluginLoader,
    stage: bool,
) -> Result<PreparedComposition> {
    let (manifest, manifest_bytes) = read_toml::<CompositionManifest>(&files.manifest_path)?;
    let report = manifest.validate();
    if !report.is_valid() {
        return Err(HostError::InvalidManifest(format_diagnostics(&report)));
    }
    let (lock, lock_bytes) = read_toml::<CompositionLock>(&files.lock_path)?;
    let manifest_hash = ContentHash::digest(&manifest_bytes);
    lock.validate_for_host(&manifest, loader.host_target(), manifest_hash)?;
    let resolved = resolve_instances(&manifest, &lock, &files.manifest_path, loader, stage)?;
    Ok(PreparedComposition {
        manifest,
        lock,
        manifest_bytes,
        manifest_hash,
        lock_hash: ContentHash::digest(&lock_bytes),
        lock_bytes,
        resolved: resolved.instances,
        runtimes: resolved.runtimes,
        process_fixed_packages: resolved.process_fixed_packages,
        fingerprints: resolved.fingerprints,
    })
}

pub(crate) fn normalize_prepared_for_install(prepared: &mut PreparedComposition) -> Result<()> {
    if prepared
        .manifest
        .instances
        .iter()
        .all(|instance| instance.package.is_absolute())
    {
        return Ok(());
    }
    let canonical_by_instance: BTreeMap<_, _> = prepared
        .resolved
        .iter()
        .map(|instance| {
            (
                instance.mount.id.clone(),
                instance.package.manifest_path.clone(),
            )
        })
        .collect();
    for instance in &mut prepared.manifest.instances {
        instance.package = canonical_by_instance
            .get(&instance.id)
            .expect("every resolved instance has package provenance")
            .clone();
    }

    let source = std::str::from_utf8(&prepared.manifest_bytes).map_err(|error| {
        HostError::InvalidManifest(format!("composition manifest is not UTF-8: {error}"))
    })?;
    let mut document: toml::Value = toml::from_str(source).map_err(|error| {
        HostError::InvalidManifest(format!("cannot normalize composition manifest: {error}"))
    })?;
    let instances = document
        .get_mut("instances")
        .and_then(toml::Value::as_array_mut)
        .ok_or_else(|| {
            HostError::InvalidManifest("composition instances are missing".to_owned())
        })?;
    if instances.len() != prepared.manifest.instances.len() {
        return Err(HostError::InvalidManifest(
            "composition instance count changed during normalization".to_owned(),
        ));
    }
    for (document_instance, parsed_instance) in
        instances.iter_mut().zip(&prepared.manifest.instances)
    {
        let table = document_instance.as_table_mut().ok_or_else(|| {
            HostError::InvalidManifest("composition instance is not a TOML table".to_owned())
        })?;
        table.insert(
            "package".to_owned(),
            toml::Value::String(
                parsed_instance
                    .package
                    .to_str()
                    .ok_or_else(|| {
                        HostError::InvalidManifest(
                            "canonical plugin manifest path is not UTF-8".to_owned(),
                        )
                    })?
                    .to_owned(),
            ),
        );
    }
    prepared.manifest_bytes = toml::to_string_pretty(&document)
        .map_err(|error| HostError::InvalidManifest(error.to_string()))?
        .into_bytes();
    prepared.manifest_hash = ContentHash::digest(&prepared.manifest_bytes);
    prepared.lock.manifest_sha256 = prepared.manifest_hash;
    prepared.lock_bytes = toml::to_string_pretty(&prepared.lock)
        .map_err(|error| HostError::InvalidManifest(error.to_string()))?
        .into_bytes();
    prepared.lock_hash = ContentHash::digest(&prepared.lock_bytes);
    Ok(())
}

struct ResolvedInputs {
    instances: Vec<ResolvedInstanceSpec>,
    pub(crate) runtimes: BTreeMap<InstanceId, PreparedRuntimeInstance>,
    pub(crate) process_fixed_packages: BTreeSet<(PackageId, String)>,
    pub(crate) fingerprints: BTreeMap<InstanceId, InstanceFingerprint>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn launch_and_prepare_pumping_services(
    loader: &PluginLoader,
    composition_id: &str,
    routing: &RoutingSnapshot,
    instances: &BTreeMap<InstanceId, PreparedRuntimeInstance>,
    waves: &[Vec<InstanceId>],
    context: &RuntimeLaunchContext,
    host_services: &mut mpsc::Receiver<HostServiceCall>,
    persistence: &mut Persistence,
) -> Result<Vec<crate::runtime::RuntimeHandle>> {
    let launch = launch_and_prepare(loader, composition_id, routing, instances, waves, context);
    tokio::pin!(launch);
    let result = loop {
        tokio::select! {
            call = host_services.recv() => {
                let Some(call) = call else {
                    break launch.await;
                };
                let result = crate::host::registry::execute_host_service(persistence, &call);
                let _ = call.reply.send(result);
            }
            result = &mut launch => break result,
        }
    };
    tokio::task::yield_now().await;
    for _ in 0..host_services.len() {
        let call = host_services
            .try_recv()
            .expect("the composition launcher is the sole host-service receiver");
        let result = crate::host::registry::execute_host_service(persistence, &call);
        let _ = call.reply.send(result);
    }
    result
}

#[allow(clippy::too_many_lines)] // one bounded pass correlates each instance with its pinned package
fn resolve_instances(
    manifest: &CompositionManifest,
    lock: &CompositionLock,
    manifest_path: &Path,
    loader: &PluginLoader,
    stage: bool,
) -> Result<ResolvedInputs> {
    let base = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let locked: BTreeMap<_, _> = lock
        .packages
        .iter()
        .map(|package| (package.path.as_path(), package))
        .collect();
    let mut packages = BTreeMap::new();
    let mut process_fixed_packages = BTreeSet::new();
    for instance in &manifest.instances {
        let package_path = canonical_package_path(base, &instance.package)?;
        if packages.contains_key(&package_path) {
            continue;
        }
        let locked_package = locked.get(package_path.as_path()).ok_or_else(|| {
            HostError::LockMismatch(format!(
                "resolved package path {} has no lock entry",
                package_path.display()
            ))
        })?;
        let (package, artifact_hash, staged) = if stage {
            let staged = loader.stage(
                &package_path,
                ExpectedHashes::new(
                    locked_package.manifest_sha256,
                    locked_package.artifact_sha256,
                ),
            )?;
            (
                staged.package().clone(),
                staged.artifact_hash(),
                Some(staged),
            )
        } else {
            let package = PluginPackage::open(&package_path)?;
            let artifact = loader.validate_manifest(package.manifest())?;
            let actual_artifact = resolve_package_relative_file(
                &package_path,
                &artifact.path,
                "artifacts.path",
                "resolve plugin artifact",
            )?;
            let artifact_hash = hash_regular_file(&actual_artifact, "hash plugin artifact")?;
            (package, artifact_hash, None)
        };
        let descriptor = package.manifest();
        if descriptor.package.id != locked_package.id.0
            || descriptor.package.version != locked_package.version
        {
            return Err(HostError::LockMismatch(format!(
                "package identity at {} differs from its lock entry",
                instance.package.display()
            )));
        }
        if descriptor.package.process_fixed {
            process_fixed_packages.insert((
                PackageId::new(descriptor.package.id.clone()),
                artifact_hash.to_string(),
            ));
        }
        let config_schema_bytes = package_config_schema_bytes(&package)?;
        let compiled_schema = Arc::new(compile_config_schema(
            &package,
            config_schema_bytes.as_deref(),
        )?);
        let schema_hash = config_schema_bytes.as_deref().map(ContentHash::digest);
        if package.manifest_hash() != locked_package.manifest_sha256
            || artifact_hash != locked_package.artifact_sha256
            || schema_hash != locked_package.config_schema_sha256
        {
            return Err(HostError::LockMismatch(format!(
                "package hashes at {} differ from the lock",
                instance.package.display()
            )));
        }
        packages.insert(
            package_path.clone(),
            (
                PackageSource {
                    package_id: PackageId::new(descriptor.package.id.clone()),
                    version: descriptor.package.version.clone(),
                    manifest_path: package_path,
                    target: lock.target.clone(),
                    manifest_sha256: locked_package.manifest_sha256,
                    artifact_sha256: locked_package.artifact_sha256,
                    config_schema_sha256: locked_package.config_schema_sha256,
                },
                descriptor.provides.clone(),
                descriptor.injects.clone(),
                descriptor.capabilities.clone(),
                descriptor.package.process_fixed,
                package_config_schema_path(&package)?,
                config_schema_bytes,
                compiled_schema,
                package,
                staged,
            ),
        );
    }
    if packages.len() != lock.packages.len() {
        return Err(HostError::LockMismatch(
            "locked package set differs from the canonically resolved composition packages"
                .to_owned(),
        ));
    }

    let mut runtimes = BTreeMap::new();
    let mut fingerprints = BTreeMap::new();
    let instances = manifest
        .instances
        .iter()
        .map(|instance| {
            let (
                package_source,
                provides,
                injects,
                capabilities,
                process_fixed,
                config_schema_path,
                config_schema_bytes,
                compiled_schema,
                package,
                staged,
            ) = packages
                .get(&canonical_package_path(base, &instance.package)?)
                .ok_or_else(|| HostError::LockMismatch("instance package changed".to_owned()))?;
            let unresolved_config = if instance.config.is_null() {
                serde_json::json!({})
            } else {
                instance.config.clone()
            };
            let prepared_config =
                prepare_config_with_compiled_schema(compiled_schema, unresolved_config)?;
            let provides = provides.iter().cloned().map(ServiceKey::new).collect();
            let requires: Vec<_> = injects
                .iter()
                .map(|inject| ServiceRequirement {
                    service: ServiceKey::new(inject.contract.clone()),
                    optional: !inject.required,
                })
                .collect();
            let schema_hash = config_schema_bytes.as_deref().map(ContentHash::digest);
            let mut semantic_mount = instance.clone();
            semantic_mount.clone_package_from(&package_source.manifest_path);
            let semantic_bytes = serde_json::to_vec(&serde_json::json!({
                "mount": semantic_mount,
                "package": package_source,
                "provides": &provides,
                "requires": &requires,
                "config_audit_sha256": prepared_config.audit_hash().to_string(),
                "capabilities": capabilities,
                "process_fixed": process_fixed,
                "config_schema_sha256": schema_hash.map(|hash| hash.to_string()),
            }))?;
            fingerprints.insert(
                instance.id.clone(),
                InstanceFingerprint {
                    semantic_hash: ContentHash::digest(semantic_bytes),
                    artifact_hash: package_source.artifact_sha256,
                    process_fixed: *process_fixed,
                    package_id: package_source.package_id.clone(),
                },
            );
            if let Some(staged) = staged {
                let schema = compiled_schema.schema().cloned().zip(schema_hash);
                runtimes.insert(
                    instance.id.clone(),
                    PreparedRuntimeInstance {
                        instance: instance.id.clone(),
                        package: package.clone(),
                        staged: staged.clone(),
                        resolved_config: prepared_config.resolved().clone(),
                        redacted_config: prepared_config.redacted().clone(),
                        config_audit_hash: prepared_config.audit_hash(),
                        capabilities: capabilities.clone(),
                        process_fixed: *process_fixed,
                        uses_state_service: injects
                            .iter()
                            .any(|inject| inject.contract == crate::runtime::STATE_SERVICE),
                        uses_runtime_tick: injects
                            .iter()
                            .any(|inject| inject.contract == crate::runtime::TICK_SERVICE),
                        config_schema_path: config_schema_path.clone(),
                        config_schema_hash: schema.as_ref().map(|(_, hash)| *hash),
                        config_schema: schema.map(|(value, _)| value),
                    },
                );
            }
            Ok(ResolvedInstanceSpec {
                mount: instance.clone(),
                package: package_source.clone(),
                provides,
                requires,
                capabilities: capabilities.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ResolvedInputs {
        instances,
        runtimes,
        process_fixed_packages,
        fingerprints,
    })
}

pub(crate) fn resolve_prepared(
    prepared: &PreparedComposition,
    revision: GraphRevision,
    reusable_generations: Option<&BTreeMap<InstanceId, Arc<crate::model::Generation>>>,
    next_generation: &mut u64,
) -> Result<RoutingSnapshot> {
    resolve(
        &prepared.manifest.composition.id,
        &prepared.resolved,
        &prepared.manifest.scope_parents(),
        revision,
        reusable_generations,
        next_generation,
    )
    .map_err(|report| HostError::InvalidManifest(format_diagnostics(&report)))
}

pub(crate) fn composition_event(
    prepared: &PreparedComposition,
    routing: &RoutingSnapshot,
    source: CompositionChangeSource,
) -> Event {
    let (active_instances, inactive_instances) =
        routing
            .graph()
            .instances
            .values()
            .fold(
                (0_u32, 0_u32),
                |(active, inactive), instance| match instance.status {
                    InstanceStatus::Active => (active.saturating_add(1), inactive),
                    InstanceStatus::Inactive { .. } | InstanceStatus::Faulted { .. } => {
                        (active, inactive.saturating_add(1))
                    }
                },
            );
    Event::CompositionCommitted {
        source,
        composition_id: prepared.manifest.composition.id.clone(),
        manifest_sha256: prepared.manifest_hash.to_string(),
        lock_sha256: prepared.lock_hash.to_string(),
        active_instances,
        inactive_instances,
    }
}

pub(crate) fn validate_project_paths(
    manifest_path: &Path,
    lock_path: Option<&Path>,
    loader: &PluginLoader,
) -> Result<ValidationReport> {
    let result = if let Some(lock_path) = lock_path {
        prepare_pair(
            &CompositionFiles::new(manifest_path, lock_path),
            loader,
            false,
        )
        .and_then(|prepared| {
            let mut next = 1;
            resolve_prepared(&prepared, GraphRevision(0), None, &mut next).map(|_| ())
        })
    } else {
        build_lock(manifest_path, loader).and_then(|lock| {
            let (manifest, _) = read_toml::<CompositionManifest>(manifest_path)?;
            let resolved = resolve_instances(&manifest, &lock, manifest_path, loader, false)?;
            let mut next = 1;
            resolve(
                &manifest.composition.id,
                &resolved.instances,
                &manifest.scope_parents(),
                GraphRevision(0),
                None,
                &mut next,
            )
            .map(|_| ())
            .map_err(|report| HostError::InvalidManifest(format_diagnostics(&report)))
        })
    };
    match result {
        Ok(()) => Ok(ValidationReport {
            diagnostics: Vec::new(),
        }),
        Err(error @ (HostError::Io { .. } | HostError::Loader(LoaderError::Io { .. }))) => {
            Err(error)
        }
        Err(error) => Ok(ValidationReport {
            diagnostics: vec![crate::model::Diagnostic::error(
                "validation_failed",
                error.to_string(),
                None,
            )],
        }),
    }
}

pub(crate) fn build_lock(manifest_path: &Path, loader: &PluginLoader) -> Result<CompositionLock> {
    let (manifest, manifest_bytes) = read_toml::<CompositionManifest>(manifest_path)?;
    let report = manifest.validate();
    if !report.is_valid() {
        return Err(HostError::InvalidManifest(format_diagnostics(&report)));
    }
    let base = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let resolved_packages = manifest
        .instances
        .iter()
        .map(|instance| canonical_package_path(base, &instance.package))
        .collect::<Result<BTreeSet<_>>>()?;
    let mut packages = Vec::new();
    for resolved_path in resolved_packages {
        let package = PluginPackage::open(&resolved_path)?;
        let artifact = loader.validate_manifest(package.manifest())?;
        let artifact_path = resolve_package_relative_file(
            &resolved_path,
            &artifact.path,
            "artifacts.path",
            "resolve plugin artifact",
        )?;
        let artifact_hash = hash_regular_file(&artifact_path, "hash plugin artifact")?;
        packages.push(LockedPackage {
            id: PackageId::new(package.manifest().package.id.clone()),
            version: package.manifest().package.version.clone(),
            path: resolved_path,
            manifest_sha256: package.manifest_hash(),
            artifact_sha256: artifact_hash,
            config_schema_sha256: package_config_schema_hash(&package)?,
        });
    }
    Ok(CompositionLock {
        format_version: 0,
        target: loader.host_target().to_owned(),
        manifest_sha256: ContentHash::digest(manifest_bytes),
        packages,
    })
}

pub(crate) fn read_toml<T: DeserializeOwned>(path: &Path) -> Result<(T, Vec<u8>)> {
    let bytes = read_bounded_file_following_symlinks(
        path,
        "read composition document",
        MAX_COMPOSITION_DOCUMENT_BYTES,
    )?;
    let source = std::str::from_utf8(&bytes).map_err(|error| HostError::DocumentParse {
        path: path.to_owned(),
        format: "TOML",
        message: error.to_string(),
    })?;
    let value = toml::from_str(source).map_err(|error| HostError::DocumentParse {
        path: path.to_owned(),
        format: "TOML",
        message: error.to_string(),
    })?;
    Ok((value, bytes))
}

pub(crate) fn resolve_package_path(base: &Path, declared: &Path) -> PathBuf {
    let path = if declared.is_absolute() {
        declared.to_owned()
    } else {
        base.join(declared)
    };
    if path.is_dir() {
        path.join("plugin.toml")
    } else {
        path
    }
}

pub(crate) fn canonical_package_path(base: &Path, declared: &Path) -> Result<PathBuf> {
    let resolved = resolve_package_path(base, declared);
    let metadata = fs::symlink_metadata(&resolved).map_err(|source| HostError::Io {
        path: resolved.clone(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(HostError::InvalidManifest(format!(
            "plugin manifest {} must be a regular non-symlink file",
            resolved.display()
        )));
    }
    fs::canonicalize(&resolved).map_err(|source| HostError::Io {
        path: resolved,
        source,
    })
}

pub(crate) fn format_diagnostics(report: &ValidationReport) -> String {
    report
        .diagnostics
        .iter()
        .map(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message))
        .collect::<Vec<_>>()
        .join("; ")
}
