use super::{CatalogInner, LoadGate, NativeCatalog, StagedArtifact};
use crate::{LoaderError, NativeFactory, NativeModule};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Weak};

pub(crate) struct StagedModuleLoad {
    artifact: Option<StagedArtifact>,
    catalog: Option<Arc<CatalogInner>>,
}

impl StagedModuleLoad {
    pub(crate) fn new(artifact: StagedArtifact, catalog: Arc<CatalogInner>) -> Self {
        Self {
            artifact: Some(artifact),
            catalog: Some(catalog),
        }
    }

    pub(crate) fn artifact(&self) -> &StagedArtifact {
        self.artifact
            .as_ref()
            .expect("a staged module load retains its artifact")
    }

    pub(crate) fn into_parts(mut self) -> (StagedArtifact, Arc<CatalogInner>) {
        let artifact = self
            .artifact
            .take()
            .expect("a staged module load retains its artifact");
        let catalog = self
            .catalog
            .take()
            .expect("a staged module load retains its catalog lease");
        (artifact, catalog)
    }
}

impl Drop for StagedModuleLoad {
    fn drop(&mut self) {
        // Owned staging must disappear before the final Catalog lease can
        // unlock a cache that rejects unmanaged temporary entries.
        drop(self.artifact.take());
        drop(self.catalog.take());
    }
}

impl NativeCatalog {
    pub(super) fn load_inner(&self, source: &Path) -> Result<Arc<NativeFactory>, LoaderError> {
        self.inner
            .modules
            .lock()
            .expect("catalog poisoned")
            .retain(|_, module| module.strong_count() != 0);
        let mut digest = Self::source_digest(source)?;
        if let Some(module) = self.live_module(&digest) {
            return Ok(self.factory_for(module));
        }

        let mut staged = None;
        loop {
            let load_gate = self.load_gate(&digest);
            let load = load_gate
                .callback
                .lock()
                .expect("native load gate poisoned");
            if load_gate.timed_out.load(Ordering::Acquire) {
                return Err(LoaderError::Callback {
                    operation: "load",
                    message: "a previous timed-out worker is still inside this artifact".to_owned(),
                });
            }
            if let Some(module) = self.live_module(&digest) {
                if staged.is_some() {
                    return Ok(self.factory_for(module));
                }
                let confirmed = Self::source_digest(source)?;
                if confirmed == digest {
                    return Ok(self.factory_for(module));
                }
                let authoritative = self.stage_source(source)?;
                if authoritative.digest == digest {
                    return Ok(self.factory_for(module));
                }
                digest.clone_from(&authoritative.digest);
                staged = Some(authoritative);
                drop(load);
                continue;
            }

            let authoritative = match staged.take() {
                Some(staged) => staged,
                None => self.stage_source(source)?,
            };
            if authoritative.digest != digest {
                digest.clone_from(&authoritative.digest);
                staged = Some(authoritative);
                drop(load);
                continue;
            }
            let module = self.load_staged(authoritative, &digest, &load_gate)?;
            return Ok(self.factory_for(module));
        }
    }

    fn live_module(&self, digest: &str) -> Option<Arc<NativeModule>> {
        self.inner
            .modules
            .lock()
            .expect("catalog poisoned")
            .get(digest)
            .and_then(Weak::upgrade)
    }

    fn load_gate(&self, digest: &str) -> Arc<LoadGate> {
        let mut gates = self.inner.load_gates.lock().expect("catalog poisoned");
        gates.retain(|_, gate| gate.strong_count() != 0);
        if let Some(gate) = gates.get(digest).and_then(Weak::upgrade) {
            gate
        } else {
            let gate = Arc::new(LoadGate::default());
            gates.insert(digest.to_owned(), Arc::downgrade(&gate));
            gate
        }
    }

    fn load_staged(
        &self,
        staged: StagedArtifact,
        digest: &str,
        load_gate: &Arc<LoadGate>,
    ) -> Result<Arc<NativeModule>, LoaderError> {
        let factory_destruction_permit = self.inner.executor.reserve_factory_destruction()?;
        let timeout = self.inner.options.callback_timeout;
        let worker_digest = digest.to_owned();
        let worker_gate = Arc::clone(load_gate);
        let executor = self.inner.executor.clone();
        let catalog = Arc::clone(&self.inner);
        let receiver = self
            .inner
            .executor
            .spawn_blocking_callback("load", move || {
                // SAFETY: The private staged file contains the exact bytes
                // hashed by this catalog and the caller trusts its code.
                let result = unsafe {
                    NativeModule::load(
                        StagedModuleLoad::new(staged, catalog),
                        worker_digest,
                        executor,
                        factory_destruction_permit,
                    )
                }
                .map(Arc::new);
                drop(worker_gate);
                result
            })?;
        let loaded = match receiver.recv_timeout(timeout) {
            Ok(result) => result?,
            Err(RecvTimeoutError::Timeout) => {
                load_gate.timed_out.store(true, Ordering::Release);
                return Err(LoaderError::Timeout("library load, entry, or descriptor"));
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(LoaderError::Callback {
                    operation: "load",
                    message: "native load worker disconnected".to_owned(),
                });
            }
        };
        self.commit_cache(digest, loaded.staged_artifact())?;
        let mut modules = self.inner.modules.lock().expect("catalog poisoned");
        if let Some(existing) = modules.get(digest).and_then(Weak::upgrade) {
            Ok(existing)
        } else {
            modules.insert(digest.to_owned(), Arc::downgrade(&loaded));
            Ok(loaded)
        }
    }

    fn factory_for(&self, module: Arc<NativeModule>) -> Arc<NativeFactory> {
        Arc::new(NativeFactory {
            descriptor: module.descriptor.clone(),
            module,
            callback_timeout: self.inner.options.callback_timeout,
            executor: self.inner.executor.clone(),
        })
    }
}
