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
    ABI_MAJOR, ABI_MINOR, Buffer, HostApi, PLUGIN_ENTRY_SYMBOL, PluginApi, PluginEntryFn,
    STATUS_OK, borrow_abi_input,
};
use serde_json::Value;
use std::ffi::c_void;
use std::fmt;
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use thiserror::Error;

mod catalog;
mod loader_plugin;
mod returned_buffer;
mod worker;

use catalog::StagedArtifact;
pub use catalog::{CatalogOptions, NativeCatalog};
pub use loader_plugin::{LoaderConfig, LoaderEntry, LoaderFactory};
use returned_buffer::ReturnedPluginBuffer;
use worker::{
    CallbackCompletion, CallbackWaitError, CompletionOnDrop, run_bounded_callback,
    spawn_native_worker,
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
    artifact: Option<File>,
    factory_gate: Mutex<()>,
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
        artifact: StagedArtifact,
        digest: String,
    ) -> std::result::Result<Self, LoaderError> {
        let path = artifact.loader_path();
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
        let mut module = Self {
            api,
            descriptor: PluginDescriptor::new(FactoryIdentity::builtin("unread", "0")),
            library: Some(library),
            artifact: Some(artifact.file),
            factory_gate: Mutex::new(()),
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

    fn read_descriptor(&self) -> std::result::Result<PluginDescriptor, LoaderError> {
        let _gate = self.lock_factory("descriptor")?;
        let mut output = Buffer::EMPTY;
        // SAFETY: The validated callback owns a live factory; output is writable.
        let status = unsafe {
            self.api.descriptor.expect("validated API")(self.api.factory_handle, &raw mut output)
        };
        let bytes = self.take_buffer(status, output, MAX_DESCRIPTOR_BYTES, "descriptor")?;
        serde_json::from_slice(&bytes).map_err(LoaderError::InvalidDescriptor)
    }

    fn transform_config(&self, config: &Value) -> std::result::Result<Value, LoaderError> {
        let _gate = self.lock_factory("validate_config")?;
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
    ) -> std::result::Result<NativeInstance, LoaderError> {
        let _gate = self.lock_factory("create")?;
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
            call_gate: Mutex::new(()),
            poisoned: AtomicBool::new(false),
            destroyed: AtomicBool::new(false),
        })
    }

    fn lock_factory(
        &self,
        operation: &'static str,
    ) -> std::result::Result<std::sync::MutexGuard<'_, ()>, LoaderError> {
        if self.factory_poisoned.load(Ordering::Acquire) {
            return Err(LoaderError::Callback {
                operation,
                message: "factory was poisoned by a timed-out callback".to_owned(),
            });
        }
        let guard = self
            .factory_gate
            .lock()
            .map_err(|_| LoaderError::Callback {
                operation,
                message: "factory callback lock was poisoned".to_owned(),
            })?;
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
        let resources = Arc::new(Mutex::new(Some(FactoryResources {
            api: self.api,
            _library: library,
            _artifact: self.artifact.take(),
        })));
        let worker_resources = Arc::clone(&resources);
        let spawn = std::thread::Builder::new()
            .name("rsi-meta-native-destroy-factory".to_owned())
            .spawn(move || {
                let resources = worker_resources
                    .lock()
                    .expect("factory destruction state poisoned")
                    .take()
                    .expect("factory destruction runs once");
                // SAFETY: NativeModule exclusively owned the factory. The
                // resource bundle keeps the library mapped through callback exit.
                unsafe {
                    resources.api.destroy_factory.expect("validated API")(
                        resources.api.factory_handle,
                    );
                }
                drop(resources);
            });
        if spawn.is_err() {
            // Thread creation failure cannot justify running foreign code on an
            // arbitrary dropping thread. Leak the still-live mapping safely.
            std::mem::forget(resources);
        }
    }
}

struct FactoryResources {
    api: PluginApi,
    _library: Library,
    _artifact: Option<File>,
}

// SAFETY: The ABI requires factory destruction to be callable from any host
// thread. The bundle retains the library and pinned artifact until it returns.
unsafe impl Send for FactoryResources {}

pub struct NativeFactory {
    module: Arc<NativeModule>,
    descriptor: PluginDescriptor,
    callback_timeout: Duration,
}

impl fmt::Debug for NativeFactory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeFactory")
            .field("descriptor", &self.descriptor)
            .finish_non_exhaustive()
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
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let worker_module = Arc::clone(&module);
        std::thread::Builder::new()
            .name("rsi-meta-native-validate".to_owned())
            .spawn(move || {
                let _ = sender.send(worker_module.transform_config(&config));
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

    async fn activate(&self, context: Context, config: ConfigValue) -> Result<()> {
        let module = Arc::clone(&self.module);
        let worker_module = Arc::clone(&module);
        let completed = Arc::new(CallbackCompletion::new());
        let worker_completed = Arc::clone(&completed);
        let callback_result_rx = spawn_native_worker("rsi-meta-native-create", move || {
            let result = worker_module.create(&config, &worker_completed);
            worker_completed.complete();
            result
        })
        .map_err(|error| MetaError::Activation(error.to_string()))?;
        let timeout = self.callback_timeout;
        let timeout_module = Arc::clone(&module);
        let timeout_runtime = context.runtime().clone();
        let on_timeout: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            timeout_module.poison_factory();
            timeout_runtime.mark_terminal("trusted native plugin create callback timed out");
        });
        let callback_result = match run_bounded_callback(
            callback_result_rx,
            Arc::clone(&completed),
            timeout,
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
        for provision in &self.descriptor.provides {
            context.provide(
                provision.key.clone(),
                provision.contract.clone(),
                provision.version,
                Arc::new(NativeEndpoint {
                    instance: Arc::downgrade(&instance),
                    service: provision.key.clone(),
                    callback_timeout: self.callback_timeout,
                }),
            )?;
        }
        context.defer(
            "native instance",
            Box::new(move || {
                Box::pin(async move {
                    let destruction =
                        spawn_native_worker("rsi-meta-native-destroy-instance", move || {
                            instance.destroy_once();
                        })
                        .map_err(|error| error.to_string())?;
                    match tokio::time::timeout(timeout, destruction).await {
                        Ok(Ok(())) => Ok(()),
                        Ok(Err(error)) => Err(format!("native destruction worker failed: {error}")),
                        Err(_) => Err("native instance destruction timed out".to_owned()),
                    }
                }) as CleanupFuture
            }),
        )
    }
}

struct NativeInstance {
    module: Arc<NativeModule>,
    handle: *mut c_void,
    call_gate: Mutex<()>,
    poisoned: AtomicBool,
    destroyed: AtomicBool,
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
        // The cleanup task may outlive the core admission lease after a timed-
        // out callback. Join the adapter-owned instance gate before claiming
        // destruction so foreign mutable access and destruction never overlap.
        let _gate = self
            .call_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        let resources = Arc::new(Mutex::new(Some(InstanceResources {
            module: Arc::clone(&self.module),
            handle: self.handle,
        })));
        let worker_resources = Arc::clone(&resources);
        let spawn = std::thread::Builder::new()
            .name("rsi-meta-native-destroy-instance".to_owned())
            .spawn(move || {
                let resources = worker_resources
                    .lock()
                    .expect("instance destruction state poisoned")
                    .take()
                    .expect("fallback instance destruction runs once");
                // SAFETY: Drop atomically claimed the create-owned handle once;
                // the resource bundle keeps its callback code mapped.
                unsafe {
                    resources
                        .module
                        .api
                        .destroy_instance
                        .expect("validated API")(resources.handle);
                }
            });
        if spawn.is_err() {
            // Foreign code must not run on an arbitrary dropping thread. Keep
            // both the handle and its mapped callback code alive instead.
            std::mem::forget(resources);
        }
    }
}

struct InstanceResources {
    module: Arc<NativeModule>,
    handle: *mut c_void,
}

// SAFETY: The ABI requires instance destruction after serialized callbacks to
// be callable from any host thread. The module Arc retains the callback code.
unsafe impl Send for InstanceResources {}

struct NativeEndpoint {
    instance: Weak<NativeInstance>,
    service: rsi_meta::ServiceKey,
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
        mut channel: ProviderChannel,
    ) -> Result<()> {
        while let Some(request) = channel.recv().await {
            let instance = self
                .instance
                .upgrade()
                .ok_or_else(|| MetaError::Service("native instance is retiring".to_owned()))?;
            let service = self.service.to_string();
            let provider_context = invocation.provider_context().clone();
            let maximum = provider_context.runtime().limits().maximum_frame_bytes;
            let runtime = provider_context.runtime().clone();
            let runtime_handle = tokio::runtime::Handle::current();
            let completed = Arc::new(CallbackCompletion::new());
            let worker_completed = Arc::clone(&completed);
            let worker_instance = Arc::clone(&instance);
            let callback_result_rx = spawn_native_worker("rsi-meta-native-call", move || {
                let result = call_native(
                    &worker_instance,
                    &worker_completed,
                    runtime_handle,
                    provider_context,
                    maximum,
                    &service,
                    request.as_bytes(),
                );
                worker_completed.complete();
                result
            })
            .map_err(|error| MetaError::Service(error.to_string()))?;
            let timeout_instance = Arc::clone(&instance);
            let timeout_runtime = runtime.clone();
            let on_timeout: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                timeout_instance.poison();
                timeout_runtime.mark_terminal("trusted native plugin service callback timed out");
            });
            let callback_result = match run_bounded_callback(
                callback_result_rx,
                Arc::clone(&completed),
                self.callback_timeout,
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

struct HostCallContext {
    runtime: tokio::runtime::Handle,
    context: Context,
    maximum_frame_bytes: usize,
}

unsafe extern "C" fn host_call_service(
    handle: *mut c_void,
    service_ptr: *const u8,
    service_len: usize,
    request_ptr: *const u8,
    request_len: usize,
    output: *mut Buffer,
) -> u32 {
    if handle.is_null()
        || output.is_null()
        || (service_ptr.is_null() && service_len != 0)
        || (request_ptr.is_null() && request_len != 0)
    {
        return rsi_meta_plugin::STATUS_INVALID_ARGUMENT;
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: The caller borrows these pointers for this synchronous call.
        let service = unsafe { borrow_abi_input(service_ptr, service_len) };
        let service = std::str::from_utf8(service).map_err(|error| error.to_string())?;
        // SAFETY: The caller borrows these pointers for this synchronous call.
        let request = unsafe { borrow_abi_input(request_ptr, request_len) };
        // SAFETY: call_native owns this exact context for the callback duration.
        let host = unsafe { &*(handle.cast::<HostCallContext>()) };
        if request.len() > host.maximum_frame_bytes {
            return Err("outbound service request exceeds the host frame limit".to_owned());
        }
        host.runtime.block_on(async {
            host.context
                .service(service)
                .map_err(|error| error.to_string())?
                .open()
                .map_err(|error| error.to_string())?
                .unary(ServiceFrame::new(request.to_vec()))
                .await
                .map(ServiceFrame::into_bytes)
                .map_err(|error| error.to_string())
        })
    }));
    let (status, bytes) = match result {
        Ok(Ok(bytes)) => (STATUS_OK, bytes),
        Ok(Err(error)) => (rsi_meta_plugin::STATUS_FAILED, error.into_bytes()),
        Err(_) => (
            rsi_meta_plugin::STATUS_PANICKED,
            b"host service bridge panicked".to_vec(),
        ),
    };
    // SAFETY: output is non-null and exclusively borrowed for this call.
    unsafe { output.write(Buffer::from_vec(bytes)) };
    status
}

unsafe extern "C" fn release_host_buffer(buffer: Buffer) {
    // SAFETY: Host bridge buffers are allocated by Buffer::from_vec here.
    unsafe { buffer.reclaim() };
}

fn call_native(
    instance: &NativeInstance,
    completed: &CallbackCompletion,
    runtime: tokio::runtime::Handle,
    context: Context,
    maximum_frame_bytes: usize,
    service: &str,
    request: &[u8],
) -> std::result::Result<Vec<u8>, LoaderError> {
    if instance.poisoned.load(Ordering::Acquire) {
        return Err(LoaderError::Callback {
            operation: "call",
            message: "instance was poisoned by a timed-out callback".to_owned(),
        });
    }
    let _gate = instance
        .call_gate
        .lock()
        .map_err(|_| LoaderError::Callback {
            operation: "call",
            message: "instance callback lock was poisoned".to_owned(),
        })?;
    let _completion = CompletionOnDrop(completed);
    if instance.poisoned.load(Ordering::Acquire) {
        return Err(LoaderError::Callback {
            operation: "call",
            message: "instance was poisoned by a timed-out callback".to_owned(),
        });
    }
    let mut host_context = HostCallContext {
        runtime,
        context,
        maximum_frame_bytes,
    };
    let host_api = HostApi {
        abi_major: ABI_MAJOR,
        abi_minor: ABI_MINOR,
        struct_size: HostApi::STRUCT_SIZE,
        reserved: 0,
        host_handle: (&raw mut host_context).cast(),
        call_service: Some(host_call_service),
        release_buffer: Some(release_host_buffer),
    };
    let mut output = Buffer::EMPTY;
    // SAFETY: Instance/module are live, host context and inputs are borrowed for
    // this synchronous call, and output is writable.
    let status = unsafe {
        instance.module.api.call.expect("validated API")(
            instance.handle,
            &raw const host_api,
            service.as_ptr(),
            service.len(),
            request.as_ptr(),
            request.len(),
            &raw mut output,
        )
    };
    instance
        .module
        .take_buffer(status, output, maximum_frame_bytes, "call")
}

#[derive(Debug, Error)]
pub enum LoaderError {
    #[error("native operation timed out: {0}")]
    Timeout(&'static str),
    #[error("invalid loader input: {0}")]
    InvalidInput(String),
    #[error("native artifact exceeds the {MAX_ARTIFACT_BYTES}-byte limit")]
    ArtifactTooLarge,
    #[error("content-addressed cache collision at {0}")]
    CacheCollision(PathBuf),
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

    #[test]
    fn callback_completion_and_timeout_have_one_winner() {
        let completed = CallbackCompletion::new();
        assert!(completed.complete());
        assert!(!completed.time_out());
        assert!(!completed.is_timed_out());

        let timed_out = CallbackCompletion::new();
        assert!(timed_out.time_out());
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
    fn fallback_instance_drop_offloads_foreign_destruction() {
        DESTROYED_SLOW_INSTANCES.store(0, Ordering::Relaxed);
        let instance = NativeInstance {
            module: Arc::new(NativeModule {
                api: PluginApi {
                    destroy_instance: Some(destroy_slow_instance),
                    ..PluginApi::EMPTY
                },
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("slow-drop", "1")),
                library: None,
                artifact: None,
                factory_gate: Mutex::new(()),
                factory_poisoned: AtomicBool::new(false),
            }),
            handle: Box::into_raw(Box::new(1_u8)).cast(),
            call_gate: Mutex::new(()),
            poisoned: AtomicBool::new(false),
            destroyed: AtomicBool::new(false),
        };

        let started = std::time::Instant::now();
        drop(instance);
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "fallback Drop ran foreign destruction inline"
        );
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
            factory_gate: Mutex::new(()),
            factory_poisoned: AtomicBool::new(false),
        });

        let completed = CallbackCompletion::new();
        let error = module
            .create(&Value::Null, &completed)
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
