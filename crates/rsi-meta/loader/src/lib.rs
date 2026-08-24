//! Native execution adapter and the Loader plugin for `rsi-meta`.
//!
//! The core runtime never maps libraries or interprets platform artifacts.
//! This crate validates and content-addresses an artifact, adapts its narrow C
//! ABI to `PluginFactory`, and exposes catalog mutation through an ordinary
//! `rsi.meta.loader` service.

#![deny(unsafe_op_in_unsafe_fn, clippy::undocumented_unsafe_blocks)]
#![allow(unsafe_code)] // Audited dynamic-library and C-ABI adapter.
#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]

use async_trait::async_trait;
use libloading::Library;
use rsi_meta::{
    CleanupFuture, ConfigValue, Context, ContractVersion, FactoryIdentity, MetaError,
    PluginDescriptor, PluginFactory, ProviderChannel, Result, ServiceEndpoint, ServiceFrame,
};
use rsi_meta_plugin::{
    ABI_MAJOR, ABI_MINOR, Buffer, PLUGIN_ENTRY_SYMBOL, PluginApi, PluginEntryFn, STATUS_OK,
};
use serde_json::Value;
use std::ffi::c_void;
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Weak};
use std::time::Duration;
use thiserror::Error;

mod catalog;
mod catalog_io;
mod catalog_resources;
mod host_bridge;
mod loader_plugin;
mod returned_buffer;
mod worker;

use catalog::{CatalogInner, StagedArtifact, StagedModuleLoad};
pub use catalog::{CatalogOptions, NativeCatalog};
pub use catalog_resources::{NativeCatalogLimits, NativeCatalogSnapshot};
use host_bridge::call_native;
pub use loader_plugin::{LoaderConfig, LoaderEntry, LoaderFactory};
use returned_buffer::ReturnedPluginBuffer;
use worker::{
    CallbackCompletion, CallbackWaitError, CompletionOnDrop, DestructionReservation,
    InstanceReservation, NativeExecutor, run_bounded_callback,
};

pub const LOADER_SERVICE_KEY: &str = "rsi.meta.loader";
pub const LOADER_CONTRACT_ID: &str = "rsi.meta.loader.v1";
pub const LOADER_CONTRACT_VERSION: ContractVersion = ContractVersion(1);
pub const MAX_ARTIFACT_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_DESCRIPTOR_BYTES: usize = 1024 * 1024;

struct NativeModule {
    api: PluginApi,
    descriptor: PluginDescriptor,
    library: Option<Library>,
    artifact: Option<StagedArtifact>,
    catalog: Option<Arc<CatalogInner>>,
    factory_destruction_permit: Option<DestructionReservation>,
    executor: NativeExecutor,
    factory_gate: Arc<tokio::sync::Semaphore>,
    factory_poisoned: AtomicBool,
}

// SAFETY: ABI callbacks are required to be thread-safe at the factory level;
// the adapter-owned gates serialize factory and instance callbacks.
unsafe impl Send for NativeModule {}
// SAFETY: Same contract as the Send implementation above.
unsafe impl Sync for NativeModule {}

impl fmt::Debug for NativeModule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeModule")
            .field("abi", &(self.api.abi_major, self.api.abi_minor))
            .finish_non_exhaustive()
    }
}

impl NativeModule {
    /// # Safety
    ///
    /// The library is trusted native code and must uphold every contract in
    /// `rsi_meta_plugin.h`, including callback lifetime and no unwinding.
    unsafe fn load(
        resources: StagedModuleLoad,
        digest: String,
        executor: NativeExecutor,
        factory_destruction_permit: DestructionReservation,
    ) -> std::result::Result<Self, LoaderError> {
        let path = resources.artifact().loader_path();
        // SAFETY: Caller accepts execution of this verified trusted artifact.
        let library = unsafe { Library::new(&path) }?;
        // SAFETY: Symbol type and name are the complete v1 entry contract.
        let entry = unsafe { library.get::<PluginEntryFn>(PLUGIN_ENTRY_SYMBOL) }?;
        let mut api = PluginApi::EMPTY;
        // SAFETY: api is writable for the checked capacity during this call.
        let status = unsafe { entry(&raw mut api, size_of::<PluginApi>()) };
        if status != STATUS_OK {
            destroy_partial_factory(&api);
            return Err(LoaderError::PluginEntry { status });
        }
        if !api.is_compatible() {
            destroy_partial_factory(&api);
            return Err(LoaderError::IncompatibleAbi {
                host_major: ABI_MAJOR,
                host_minor: ABI_MINOR,
                plugin_major: api.abi_major,
                plugin_minor: api.abi_minor,
            });
        }
        let (artifact, catalog) = resources.into_parts();
        let mut module = Self {
            api,
            descriptor: PluginDescriptor::new(FactoryIdentity::builtin("unread", "0")),
            library: Some(library),
            artifact: Some(artifact),
            catalog: Some(catalog),
            factory_destruction_permit: Some(factory_destruction_permit),
            executor,
            factory_gate: Arc::new(tokio::sync::Semaphore::new(1)),
            factory_poisoned: AtomicBool::new(false),
        };
        let mut descriptor = module.read_descriptor()?;
        descriptor.identity = FactoryIdentity::Artifact {
            plugin: descriptor.identity.to_string(),
            sha256: digest,
        };
        module.descriptor = descriptor;
        Ok(module)
    }

    fn staged_artifact(&self) -> &StagedArtifact {
        self.artifact
            .as_ref()
            .expect("a loaded native module retains its staged artifact")
    }

    fn read_descriptor(&self) -> std::result::Result<PluginDescriptor, LoaderError> {
        let _gate = self.try_lock_factory("descriptor")?;
        let mut output = Buffer::EMPTY;
        // SAFETY: The validated callback owns a live factory; output is writable.
        let status = unsafe {
            self.api.descriptor.expect("validated API")(self.api.factory_handle, &raw mut output)
        };
        let bytes = self.take_buffer(status, output, MAX_DESCRIPTOR_BYTES, "descriptor")?;
        serde_json::from_slice(&bytes).map_err(LoaderError::InvalidDescriptor)
    }

    fn transform_config(&self, config: &Value) -> std::result::Result<Value, LoaderError> {
        let input = serde_json::to_vec(&config)?;
        if input.len() > MAX_DESCRIPTOR_BYTES {
            return Err(LoaderError::InvalidInput(
                "plugin config is too large".to_owned(),
            ));
        }
        let mut output = Buffer::EMPTY;
        // SAFETY: Validated callback, live factory, borrowed input, writable output.
        let status = unsafe {
            self.api.validate_config.expect("validated API")(
                self.api.factory_handle,
                input.as_ptr(),
                input.len(),
                &raw mut output,
            )
        };
        let bytes = self.take_buffer(status, output, MAX_DESCRIPTOR_BYTES, "validate_config")?;
        serde_json::from_slice(&bytes).map_err(LoaderError::InvalidDescriptor)
    }

    fn create(
        self: &Arc<Self>,
        config: &Value,
        completed: &CallbackCompletion,
        instance_reservation: InstanceReservation,
    ) -> std::result::Result<NativeInstance, LoaderError> {
        let _completion = CompletionOnDrop(completed);
        let input = serde_json::to_vec(&config)?;
        let mut instance = core::ptr::null_mut();
        let mut error = Buffer::EMPTY;
        // SAFETY: Validated callback, live factory, borrowed input, writable outputs.
        let status = unsafe {
            self.api.create.expect("validated API")(
                self.api.factory_handle,
                input.as_ptr(),
                input.len(),
                &raw mut instance,
                &raw mut error,
            )
        };
        if status != STATUS_OK || instance.is_null() {
            let message = self
                .take_buffer(status, error, MAX_DESCRIPTOR_BYTES, "create")
                .err()
                .map_or_else(
                    || "plugin returned a null instance".to_owned(),
                    |error| error.to_string(),
                );
            if !instance.is_null() {
                // SAFETY: The ABI transfers every non-null create output to
                // the host, including failure returns, and this runs once.
                unsafe {
                    self.api.destroy_instance.expect("validated API")(instance);
                }
            }
            return Err(LoaderError::Callback {
                operation: "create",
                message,
            });
        }
        if error.capacity != 0 {
            // SAFETY: The plugin owns the optional buffer and this is its release callback.
            unsafe { self.api.release_buffer.expect("validated API")(error) };
        }
        Ok(NativeInstance {
            module: Arc::clone(self),
            handle: instance,
            call_gate: Arc::new(tokio::sync::Semaphore::new(1)),
            poisoned: AtomicBool::new(false),
            destroyed: AtomicBool::new(false),
            instance_reservation: Some(instance_reservation),
        })
    }

    fn try_lock_factory(
        &self,
        operation: &'static str,
    ) -> std::result::Result<tokio::sync::OwnedSemaphorePermit, LoaderError> {
        if self.factory_poisoned.load(Ordering::Acquire) {
            return Err(LoaderError::Callback {
                operation,
                message: "factory was poisoned by a timed-out callback".to_owned(),
            });
        }
        let guard = Arc::clone(&self.factory_gate)
            .try_acquire_owned()
            .map_err(|_| LoaderError::Busy { operation })?;
        if self.factory_poisoned.load(Ordering::Acquire) {
            return Err(LoaderError::Callback {
                operation,
                message: "factory was poisoned by a timed-out callback".to_owned(),
            });
        }
        Ok(guard)
    }

    fn poison_factory(&self) {
        self.factory_poisoned.store(true, Ordering::Release);
    }

    fn take_buffer(
        &self,
        status: u32,
        output: Buffer,
        maximum: usize,
        operation: &'static str,
    ) -> std::result::Result<Vec<u8>, LoaderError> {
        let output =
            ReturnedPluginBuffer::new(output, self.api.release_buffer.expect("validated API"));
        let bytes = output.copy(maximum, operation)?;
        if status == STATUS_OK {
            Ok(bytes)
        } else {
            Err(LoaderError::Callback {
                operation,
                message: String::from_utf8_lossy(&bytes).into_owned(),
            })
        }
    }
}

fn destroy_partial_factory(api: &PluginApi) {
    if !api.factory_handle.is_null()
        && let Some(destroy) = api.destroy_factory
    {
        // SAFETY: The ABI transfers a partially published factory when it
        // publishes both the handle and matching destructor, even on failure.
        unsafe { destroy(api.factory_handle) };
    }
}

impl Drop for NativeModule {
    fn drop(&mut self) {
        let Some(library) = self.library.take() else {
            return;
        };
        let resources = FactoryResources {
            api: self.api,
            _library: library,
            _artifact: self.artifact.take(),
            _catalog: self.catalog.take(),
        };
        let permit = self
            .factory_destruction_permit
            .take()
            .expect("mapped factories retain reserved finalizer admission");
        self.executor
            .submit_reserved_destruction(permit, move |_permit| {
                // SAFETY: NativeModule exclusively owned the factory. The
                // resource bundle keeps the library mapped through callback exit.
                unsafe {
                    resources.api.destroy_factory.expect("validated API")(
                        resources.api.factory_handle,
                    );
                }
                drop(resources);
            });
    }
}

struct FactoryResources {
    api: PluginApi,
    _library: Library,
    _artifact: Option<StagedArtifact>,
    _catalog: Option<Arc<CatalogInner>>,
}

// SAFETY: The ABI requires factory destruction to be callable from any host
// thread. The bundle retains the library and pinned artifact until it returns.
unsafe impl Send for FactoryResources {}

pub struct NativeFactory {
    module: Arc<NativeModule>,
    descriptor: PluginDescriptor,
    callback_timeout: Duration,
    executor: NativeExecutor,
}

impl fmt::Debug for NativeFactory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeFactory")
            .field("descriptor", &self.descriptor)
            .finish_non_exhaustive()
    }
}

impl NativeFactory {
    pub(crate) fn module_digest(&self) -> &str {
        match &self.descriptor.identity {
            FactoryIdentity::Artifact { sha256, .. } => sha256,
            FactoryIdentity::Builtin { .. } => {
                unreachable!("a NativeFactory always has artifact identity")
            }
        }
    }

    fn register_instance(&self, context: &Context, instance: &Arc<NativeInstance>) -> Result<()> {
        for provision in &self.descriptor.provides {
            context.provide(
                provision.key.clone(),
                provision.contract.clone(),
                provision.version,
                Arc::new(NativeEndpoint {
                    instance: Arc::downgrade(instance),
                    service: Arc::from(provision.key.as_str()),
                    callback_timeout: self.callback_timeout,
                }),
            )?;
        }
        let destruction_executor = self.executor.clone();
        let cleanup_instance = Arc::clone(instance);
        context.defer(
            "native instance",
            Box::new(move || {
                let executor = destruction_executor.clone();
                let instance = Arc::clone(&cleanup_instance);
                Box::pin(async move {
                    let gate = Arc::clone(&instance.call_gate)
                        .acquire_owned()
                        .await
                        .map_err(|_| "native instance gate is closed".to_owned())?;
                    let destruction = executor
                        .spawn_destruction(move || {
                            let _gate = gate;
                            instance.destroy_once();
                        })
                        .await
                        .map_err(|error| error.to_string())?;
                    destruction
                        .await
                        .map_err(|error| format!("native destruction worker failed: {error}"))
                }) as CleanupFuture
            }),
        )?;
        Ok(())
    }
}

#[async_trait]
impl PluginFactory for NativeFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn validate_config(&self, config: ConfigValue) -> Result<ConfigValue> {
        let module = Arc::clone(&self.module);
        if module.factory_poisoned.load(Ordering::Acquire) {
            return Err(MetaError::Activation(
                "native factory was poisoned by a timed-out callback".to_owned(),
            ));
        }
        let timeout = self.callback_timeout;
        let gate = module
            .try_lock_factory("validate_config")
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let worker_module = Arc::clone(&module);
        let receiver = self
            .executor
            .spawn_blocking_callback("validate_config", move || {
                let _gate = gate;
                worker_module.transform_config(&config)
            })
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        match receiver.recv_timeout(timeout) {
            Ok(result) => result.map_err(|error| MetaError::Activation(error.to_string())),
            Err(RecvTimeoutError::Timeout) => {
                module.poison_factory();
                Err(MetaError::Timeout("native config validation"))
            }
            Err(RecvTimeoutError::Disconnected) => Err(MetaError::Activation(
                "native config validation worker disconnected".to_owned(),
            )),
        }
    }

    async fn activate(&self, context: Context, config: Arc<ConfigValue>) -> Result<()> {
        let module = Arc::clone(&self.module);
        let gate = module
            .try_lock_factory("create")
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let instance_reservation = self
            .executor
            .reserve_instance()
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let worker_module = Arc::clone(&module);
        let completed = Arc::new(CallbackCompletion::new());
        let worker_completed = Arc::clone(&completed);
        let callback_result_rx = self
            .executor
            .spawn_callback("create", move || {
                let _gate = gate;
                let result =
                    worker_module.create(config.as_ref(), &worker_completed, instance_reservation);
                worker_completed.complete();
                result
            })
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let timeout = self.callback_timeout;
        let deadline = tokio::time::Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| {
                MetaError::InvalidInput("native callback deadline overflow".to_owned())
            })?;
        let timeout_module = Arc::clone(&module);
        let timeout_runtime = context.runtime().clone();
        let on_timeout: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            timeout_module.poison_factory();
            timeout_runtime.mark_terminal("trusted native plugin create callback timed out");
        });
        let callback_result = match run_bounded_callback(
            callback_result_rx,
            Arc::clone(&completed),
            deadline,
            on_timeout,
        )
        .await
        {
            Ok(result) => result,
            Err(CallbackWaitError::TimedOut) => {
                return Err(MetaError::Timeout("native create callback"));
            }
            Err(CallbackWaitError::Disconnected(error)) => {
                return Err(MetaError::Activation(error.to_string()));
            }
        };
        let instance = match callback_result {
            Ok(instance) => Arc::new(instance),
            Err(error) => return Err(MetaError::Activation(error.to_string())),
        };
        self.register_instance(&context, &instance)
    }
}

struct NativeInstance {
    module: Arc<NativeModule>,
    handle: *mut c_void,
    call_gate: Arc<tokio::sync::Semaphore>,
    poisoned: AtomicBool,
    destroyed: AtomicBool,
    instance_reservation: Option<InstanceReservation>,
}

// SAFETY: The adapter-owned call gate serializes access to the opaque mutable
// instance even if a core future is canceled or times out.
unsafe impl Send for NativeInstance {}
// SAFETY: Access to the opaque handle is serialized by `call_gate`; every
// blocking callback owns an Arc that retains the instance and module.
unsafe impl Sync for NativeInstance {}

impl NativeInstance {
    fn poison(&self) {
        self.poisoned.store(true, Ordering::Release);
    }

    fn destroy_once(&self) {
        if self.destroyed.swap(true, Ordering::AcqRel) {
            return;
        }
        // SAFETY: This instance handle came from create, the call gate proves
        // every serialized callback has returned, and the atomic claim gives
        // this callback its one destruction invocation.
        unsafe {
            self.module.api.destroy_instance.expect("validated API")(self.handle);
        }
    }
}

impl Drop for NativeInstance {
    fn drop(&mut self) {
        if self.destroyed.swap(true, Ordering::AcqRel) {
            return;
        }
        let gate = Arc::clone(&self.call_gate)
            .try_acquire_owned()
            .expect("the final instance owner cannot race a serialized callback");
        let resources = InstanceResources {
            module: Arc::clone(&self.module),
            handle: self.handle,
            _gate: gate,
        };
        let reservation = self
            .instance_reservation
            .take()
            .expect("an undestroyed native instance retains finalizer admission");
        self.module.executor.submit_reserved_instance_destruction(
            reservation,
            move |_reservation| {
                // SAFETY: NativeInstance::drop claimed this create-owned
                // handle exactly once and moved its exclusive call-gate
                // permit here. The module Arc retains the callback code.
                unsafe {
                    resources
                        .module
                        .api
                        .destroy_instance
                        .expect("validated API")(resources.handle);
                }
                drop(resources);
            },
        );
    }
}

struct InstanceResources {
    module: Arc<NativeModule>,
    handle: *mut c_void,
    _gate: tokio::sync::OwnedSemaphorePermit,
}

// SAFETY: The ABI requires instance destruction after serialized callbacks to
// be callable from any host thread. The module Arc retains the callback code.
unsafe impl Send for InstanceResources {}

struct NativeEndpoint {
    instance: Weak<NativeInstance>,
    service: Arc<str>,
    callback_timeout: Duration,
}

impl fmt::Debug for NativeEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeEndpoint")
            .field("service", &self.service)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl ServiceEndpoint for NativeEndpoint {
    async fn serve(
        &self,
        invocation: rsi_meta::InvocationContext,
        mut channel: ProviderChannel<'_>,
    ) -> Result<()> {
        let provider_context = invocation.provider_context().clone();
        let maximum = provider_context
            .runtime()
            .limits()
            .payloads
            .maximum_frame_bytes;
        let runtime = provider_context.runtime().clone();
        let runtime_handle = tokio::runtime::Handle::current();
        while let Some(request) = channel.recv().await {
            let instance = self
                .instance
                .upgrade()
                .ok_or_else(|| MetaError::Service("native instance is retiring".to_owned()))?;
            let service = Arc::clone(&self.service);
            let frame_context = provider_context.clone();
            let frame_runtime_handle = runtime_handle.clone();
            // Fail fast before waiting behind a callback that timed out while
            // retaining the serialized instance gate.
            if instance.poisoned.load(Ordering::Acquire) {
                return Err(MetaError::Service(
                    "native instance was poisoned by a timed-out callback".to_owned(),
                ));
            }
            let gate = Arc::clone(&instance.call_gate)
                .acquire_owned()
                .await
                .map_err(|_| MetaError::Service("native instance gate is closed".to_owned()))?;
            // Close the race in which the previous callback timed out while
            // this frame was waiting for the gate, then eventually returned.
            if instance.poisoned.load(Ordering::Acquire) {
                return Err(MetaError::Service(
                    "native instance was poisoned by a timed-out callback".to_owned(),
                ));
            }
            let completed = Arc::new(CallbackCompletion::new());
            let worker_completed = Arc::clone(&completed);
            let worker_instance = Arc::clone(&instance);
            let callback_result_rx = instance
                .module
                .executor
                .spawn_callback("call", move || {
                    let result = call_native(
                        &worker_instance,
                        &worker_completed,
                        frame_runtime_handle,
                        frame_context,
                        maximum,
                        service.as_ref(),
                        request.as_bytes(),
                    );
                    worker_completed.complete();
                    // NativeInstance::drop may run as the callback-owned Arc
                    // is released, so publish the gate first.
                    drop(gate);
                    result
                })
                .map_err(|error| MetaError::Service(error.to_string()))?;
            let timeout_instance = Arc::clone(&instance);
            let timeout_runtime = runtime.clone();
            let on_timeout: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                timeout_instance.poison();
                timeout_runtime.mark_terminal("trusted native plugin service callback timed out");
            });
            let deadline = tokio::time::Instant::now()
                .checked_add(self.callback_timeout)
                .ok_or_else(|| {
                    MetaError::InvalidInput("native callback deadline overflow".to_owned())
                })?;
            let callback_result = match run_bounded_callback(
                callback_result_rx,
                Arc::clone(&completed),
                deadline,
                on_timeout,
            )
            .await
            {
                Ok(result) => result,
                Err(CallbackWaitError::TimedOut) => {
                    return Err(MetaError::Timeout("native service callback"));
                }
                Err(CallbackWaitError::Disconnected(error)) => {
                    return Err(MetaError::Service(error.to_string()));
                }
            };
            let response = match callback_result {
                Ok(bytes) => bytes,
                Err(error) => return Err(MetaError::Service(error.to_string())),
            };
            channel.send(ServiceFrame::new(response)).await?;
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum LoaderError {
    #[error("native operation timed out: {0}")]
    Timeout(&'static str),
    #[error("native operation is busy: {operation}")]
    Busy { operation: &'static str },
    #[error("invalid loader input: {0}")]
    InvalidInput(String),
    #[error("native artifact exceeds the {MAX_ARTIFACT_BYTES}-byte limit")]
    ArtifactTooLarge,
    #[error("private staged artifact changed after its digest was computed")]
    StagedArtifactChanged,
    #[error("content-addressed cache collision at {0}")]
    CacheCollision(PathBuf),
    #[error("native cache directory is already owned by another catalog: {0}")]
    CacheLocked(PathBuf),
    #[error("native cache durability is poisoned after an unprovable rollback")]
    CachePoisoned,
    #[error("native {resource} capacity is exhausted at limit {limit}")]
    CapacityExhausted { resource: &'static str, limit: u64 },
    #[error("native plugin entry failed with status {status}")]
    PluginEntry { status: u32 },
    #[error(
        "native plugin ABI is incompatible (host {host_major}.{host_minor}, plugin {plugin_major}.{plugin_minor})"
    )]
    IncompatibleAbi {
        host_major: u32,
        host_minor: u32,
        plugin_major: u32,
        plugin_minor: u32,
    },
    #[error("native {operation} callback failed: {message}")]
    Callback {
        operation: &'static str,
        message: String,
    },
    #[error("native descriptor is invalid: {0}")]
    InvalidDescriptor(serde_json::Error),
    #[error("native library error: {0}")]
    Library(#[from] libloading::Error),
    #[error("native artifact I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("native JSON failed: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    static DESTROYED_PARTIAL_INSTANCES: AtomicUsize = AtomicUsize::new(0);
    static DESTROYED_SLOW_INSTANCES: AtomicUsize = AtomicUsize::new(0);

    fn test_executor() -> NativeExecutor {
        NativeExecutor::new(8, 2, 8, 8).unwrap()
    }

    #[test]
    fn callback_completion_and_timeout_have_one_winner() {
        let completed = CallbackCompletion::new();
        assert!(completed.complete());
        assert!(!completed.time_out(&|| {}));
        assert!(!completed.is_timed_out());

        let timed_out = CallbackCompletion::new();
        assert!(timed_out.time_out(&|| {}));
        assert!(!timed_out.complete());
        assert!(timed_out.is_timed_out());
    }

    unsafe extern "C" fn create_partial_failure(
        _: *mut c_void,
        _: *const u8,
        _: usize,
        instance_out: *mut *mut c_void,
        error_out: *mut Buffer,
    ) -> u32 {
        // SAFETY: The test passes writable out-pointers for this callback.
        unsafe {
            instance_out.write(Box::into_raw(Box::new(7_u8)).cast());
            error_out.write(Buffer::from_vec(b"create failed".to_vec()));
        }
        rsi_meta_plugin::STATUS_FAILED
    }

    unsafe extern "C" fn destroy_partial_instance(instance: *mut c_void) {
        DESTROYED_PARTIAL_INSTANCES.fetch_add(1, Ordering::Relaxed);
        // SAFETY: `create_partial_failure` allocated this exact byte once.
        drop(unsafe { Box::from_raw(instance.cast::<u8>()) });
    }

    unsafe extern "C" fn release_test_buffer(buffer: Buffer) {
        // SAFETY: The callback receives a buffer created by `Buffer::from_vec`.
        unsafe { buffer.reclaim() };
    }

    unsafe extern "C" fn destroy_slow_instance(instance: *mut c_void) {
        std::thread::sleep(Duration::from_millis(200));
        DESTROYED_SLOW_INSTANCES.fetch_add(1, Ordering::Relaxed);
        // SAFETY: The test allocated this exact byte once.
        drop(unsafe { Box::from_raw(instance.cast::<u8>()) });
    }

    #[test]
    fn fallback_instance_drop_uses_reserved_capacity_when_the_queue_is_full() {
        DESTROYED_SLOW_INSTANCES.store(0, Ordering::Relaxed);
        let executor = NativeExecutor::new(8, 1, 1, 1).unwrap();
        let (release_sender, release_receiver) = std::sync::mpsc::sync_channel(1);
        assert!(executor.try_submit_destruction(move || {
            release_receiver.recv().unwrap();
        }));
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while executor.snapshot().active_destructions != 1 && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(executor.snapshot().active_destructions, 1);
        assert!(executor.try_submit_destruction(|| {}));
        let instance_reservation = executor.reserve_instance().unwrap();
        let instance = NativeInstance {
            module: Arc::new(NativeModule {
                api: PluginApi {
                    destroy_instance: Some(destroy_slow_instance),
                    ..PluginApi::EMPTY
                },
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("slow-drop", "1")),
                library: None,
                artifact: None,
                catalog: None,
                factory_destruction_permit: None,
                executor: executor.clone(),
                factory_gate: Arc::new(tokio::sync::Semaphore::new(1)),
                factory_poisoned: AtomicBool::new(false),
            }),
            handle: Box::into_raw(Box::new(1_u8)).cast(),
            call_gate: Arc::new(tokio::sync::Semaphore::new(1)),
            poisoned: AtomicBool::new(false),
            destroyed: AtomicBool::new(false),
            instance_reservation: Some(instance_reservation),
        };

        let started = std::time::Instant::now();
        drop(instance);
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "fallback Drop ran foreign destruction inline"
        );
        release_sender.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while DESTROYED_SLOW_INSTANCES.load(Ordering::Relaxed) == 0
            && std::time::Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        assert_eq!(DESTROYED_SLOW_INSTANCES.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn failed_create_destroys_a_nonnull_partial_instance() {
        DESTROYED_PARTIAL_INSTANCES.store(0, Ordering::Relaxed);
        let executor = test_executor();
        let instance_reservation = executor.reserve_instance().unwrap();
        let module = Arc::new(NativeModule {
            api: PluginApi {
                create: Some(create_partial_failure),
                destroy_instance: Some(destroy_partial_instance),
                release_buffer: Some(release_test_buffer),
                ..PluginApi::EMPTY
            },
            descriptor: PluginDescriptor::new(FactoryIdentity::builtin("partial", "1")),
            library: None,
            artifact: None,
            catalog: None,
            factory_destruction_permit: None,
            executor,
            factory_gate: Arc::new(tokio::sync::Semaphore::new(1)),
            factory_poisoned: AtomicBool::new(false),
        });

        let completed = CallbackCompletion::new();
        let error = module
            .create(&Value::Null, &completed, instance_reservation)
            .err()
            .expect("create must fail");
        assert!(!completed.is_timed_out());
        assert!(
            !completed.complete(),
            "create published completion before return"
        );
        assert!(error.to_string().contains("create failed"), "{error}");
        assert_eq!(DESTROYED_PARTIAL_INSTANCES.load(Ordering::Relaxed), 1);
    }
}
