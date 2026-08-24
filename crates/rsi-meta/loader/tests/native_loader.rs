use async_trait::async_trait;
use rsi_meta::{
    Context, ContractVersion, DeadlineLimits, ExecutionLimits, FactoryIdentity, FiberState,
    MetaError, PluginDescriptor, PluginFactory, ProviderChannel, Provision, Requirement, Result,
    Runtime, RuntimeLimits, ServiceEndpoint, ServiceFrame, ServiceHandle, TopologyLimits,
};
use rsi_meta_loader::{
    CatalogOptions, LOADER_CONTRACT_ID, LOADER_CONTRACT_VERSION, LOADER_SERVICE_KEY, LoaderError,
    LoaderFactory, NativeCatalog, NativeCatalogLimits,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const V1: ContractVersion = ContractVersion(1);

fn native_fixture() -> &'static PathBuf {
    static ARTIFACT: OnceLock<PathBuf> = OnceLock::new();
    ARTIFACT.get_or_init(|| {
        let loader = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let root = loader.join("../../..");
        let manifest = root.join("fixtures/rsi-meta/echo-bidi/Cargo.toml");
        let target = root.join("target/native-fixture-test");
        let status = std::process::Command::new(env!("CARGO"))
            .args(["build", "--locked", "--manifest-path"])
            .arg(&manifest)
            .arg("--target-dir")
            .arg(&target)
            .status()
            .expect("build native fixture");
        assert!(status.success(), "native fixture build failed");
        target.join("debug").join(format!(
            "{}rsi_meta_fixture_echo_bidi{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_SUFFIX
        ))
    })
}

fn catalog() -> (tempfile::TempDir, NativeCatalog) {
    let directory = tempfile::tempdir().unwrap();
    let catalog = NativeCatalog::new(CatalogOptions::new(directory.path())).unwrap();
    (directory, catalog)
}

fn catalog_with_timeout(timeout: Duration) -> (tempfile::TempDir, NativeCatalog) {
    let directory = tempfile::tempdir().unwrap();
    let mut options = CatalogOptions::new(directory.path());
    options.callback_timeout = timeout;
    let catalog = NativeCatalog::new(options).unwrap();
    (directory, catalog)
}

fn wait_for_staging_release(catalog: &NativeCatalog) {
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while catalog.snapshot().staging_bytes != 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(catalog.snapshot().staging_bytes, 0);
}

fn wait_for_callback_quiescence(catalog: &NativeCatalog) {
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while catalog.snapshot().active_callbacks != 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(catalog.snapshot().active_callbacks, 0);
}

fn wait_for_catalog_ownership_release(cache: &std::path::Path) -> NativeCatalog {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        match NativeCatalog::new(CatalogOptions::new(cache)) {
            Ok(catalog) => return catalog,
            Err(LoaderError::CacheLocked(_)) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("cache ownership was not released cleanly: {error}"),
        }
    }
}

#[cfg(target_os = "linux")]
fn failed_entry_fixture(status: u32) -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("failed_entry.c");
    let library = directory.path().join("libfailed_entry.so");
    let code = format!(
        r#"
#include "rsi_meta_plugin.h"

uint32_t rsi_meta_plugin_entry_v1(rsi_meta_plugin_api *output,
                                  size_t capacity) {{
  (void)output;
  (void)capacity;
  return {status};
}}
"#
    );
    std::fs::write(&source, code).unwrap();
    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let status = std::process::Command::new(compiler)
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-pedantic"])
        .args(["-shared", "-fPIC"])
        .arg(&source)
        .arg("-I")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../plugin/include"))
        .arg("-o")
        .arg(&library)
        .status()
        .expect("compile failed-entry native fixture");
    assert!(
        status.success(),
        "failed-entry native fixture failed to build"
    );
    (directory, library)
}

#[cfg(target_os = "linux")]
fn malformed_buffer_fixture(release_marker: &std::path::Path) -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("malformed_buffer.c");
    let library = directory.path().join("libmalformed_buffer.so");
    let code = format!(
        r#"
#include <stdio.h>
#include <string.h>
#include "rsi_meta_plugin.h"

static uint32_t descriptor(void *handle, rsi_meta_buffer *output) {{
  (void)handle;
  output->ptr = NULL;
  output->len = 1;
  output->capacity = 1;
  return RSI_META_STATUS_OK;
}}

static uint32_t validate_config(void *handle, const uint8_t *input,
                                size_t input_len, rsi_meta_buffer *output) {{
  (void)handle; (void)input; (void)input_len;
  *output = (rsi_meta_buffer){{NULL, 0, 0}};
  return RSI_META_STATUS_FAILED;
}}

static uint32_t create_instance(void *handle, const uint8_t *input,
                                size_t input_len, void **instance,
                                rsi_meta_buffer *output) {{
  (void)handle; (void)input; (void)input_len;
  *instance = NULL;
  *output = (rsi_meta_buffer){{NULL, 0, 0}};
  return RSI_META_STATUS_FAILED;
}}

static uint32_t call_instance(void *instance, const rsi_meta_host_api *host,
                              const uint8_t *service, size_t service_len,
                              const uint8_t *request, size_t request_len,
                              rsi_meta_buffer *output) {{
  (void)instance; (void)host; (void)service; (void)service_len;
  (void)request; (void)request_len;
  *output = (rsi_meta_buffer){{NULL, 0, 0}};
  return RSI_META_STATUS_FAILED;
}}

static void destroy_handle(void *handle) {{ (void)handle; }}

static void release_buffer(rsi_meta_buffer buffer) {{
  (void)buffer;
  FILE *marker = fopen("{}", "wb");
  if (marker != NULL) {{
    fputs("released", marker);
    fclose(marker);
  }}
}}

uint32_t rsi_meta_plugin_entry_v1(rsi_meta_plugin_api *output,
                                  size_t capacity) {{
  if (output == NULL || capacity < sizeof(*output)) {{
    return RSI_META_STATUS_INVALID_ARGUMENT;
  }}
  memset(output, 0, sizeof(*output));
  output->abi_major = RSI_META_ABI_MAJOR;
  output->abi_minor = RSI_META_ABI_MINOR;
  output->struct_size = sizeof(*output);
  output->factory_handle = (void *)(uintptr_t)1;
  output->descriptor = descriptor;
  output->validate_config = validate_config;
  output->create = create_instance;
  output->call = call_instance;
  output->destroy_instance = destroy_handle;
  output->destroy_factory = destroy_handle;
  output->release_buffer = release_buffer;
  return RSI_META_STATUS_OK;
}}
"#,
        release_marker.display()
    );
    std::fs::write(&source, code).unwrap();
    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let status = std::process::Command::new(compiler)
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-pedantic"])
        .args(["-shared", "-fPIC"])
        .arg(&source)
        .arg("-I")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../plugin/include"))
        .arg("-o")
        .arg(&library)
        .status()
        .expect("compile malformed native fixture");
    assert!(status.success(), "malformed native fixture failed to build");
    (directory, library)
}

#[cfg(target_os = "linux")]
fn destruction_order_fixture(
    call_entered: &std::path::Path,
    call_release: &std::path::Path,
    destroy_entered: &std::path::Path,
) -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("destruction_order.c");
    let library = directory.path().join("libdestruction_order.so");
    let code = format!(
        r#"
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "rsi_meta_plugin.h"

static uint32_t copy_output(const uint8_t *bytes, size_t len,
                            rsi_meta_buffer *output) {{
  uint8_t *copy = malloc(len);
  if (copy == NULL) return RSI_META_STATUS_FAILED;
  memcpy(copy, bytes, len);
  *output = (rsi_meta_buffer){{copy, len, len}};
  return RSI_META_STATUS_OK;
}}

static uint32_t descriptor(void *handle, rsi_meta_buffer *output) {{
  (void)handle;
  static const uint8_t value[] =
      "{{\"identity\":{{\"kind\":\"builtin\",\"name\":\"fixture.destroy-order\",\"revision\":\"1\"}},"
      "\"requires\":[],\"provides\":[{{\"key\":\"echo\",\"contract\":\"fixture.echo\",\"version\":1}}]}}";
  return copy_output(value, sizeof(value) - 1, output);
}}

static uint32_t validate_config(void *handle, const uint8_t *input,
                                size_t input_len, rsi_meta_buffer *output) {{
  (void)handle;
  return copy_output(input, input_len, output);
}}

static uint32_t create_instance(void *handle, const uint8_t *input,
                                size_t input_len, void **instance,
                                rsi_meta_buffer *output) {{
  (void)handle; (void)input; (void)input_len;
  *instance = (void *)(uintptr_t)1;
  *output = (rsi_meta_buffer){{NULL, 0, 0}};
  return RSI_META_STATUS_OK;
}}

static uint32_t call_instance(void *instance, const rsi_meta_host_api *host,
                              const uint8_t *service, size_t service_len,
                              const uint8_t *request, size_t request_len,
                              rsi_meta_buffer *output) {{
  (void)instance; (void)host; (void)service; (void)service_len;
  (void)request; (void)request_len;
  FILE *entered = fopen("{}", "wb");
  if (entered != NULL) {{ fputs("entered", entered); fclose(entered); }}
  while (access("{}", F_OK) != 0) sched_yield();
  static const uint8_t response[] = "released";
  return copy_output(response, sizeof(response) - 1, output);
}}

static void destroy_instance(void *instance) {{
  (void)instance;
  FILE *entered = fopen("{}", "wb");
  if (entered != NULL) {{ fputs("entered", entered); fclose(entered); }}
}}

static void destroy_factory(void *handle) {{ (void)handle; }}
static void release_buffer(rsi_meta_buffer buffer) {{ free(buffer.ptr); }}

uint32_t rsi_meta_plugin_entry_v1(rsi_meta_plugin_api *output,
                                  size_t capacity) {{
  if (output == NULL || capacity < sizeof(*output)) {{
    return RSI_META_STATUS_INVALID_ARGUMENT;
  }}
  memset(output, 0, sizeof(*output));
  output->abi_major = RSI_META_ABI_MAJOR;
  output->abi_minor = RSI_META_ABI_MINOR;
  output->struct_size = sizeof(*output);
  output->factory_handle = (void *)(uintptr_t)1;
  output->descriptor = descriptor;
  output->validate_config = validate_config;
  output->create = create_instance;
  output->call = call_instance;
  output->destroy_instance = destroy_instance;
  output->destroy_factory = destroy_factory;
  output->release_buffer = release_buffer;
  return RSI_META_STATUS_OK;
}}
"#,
        call_entered.display(),
        call_release.display(),
        destroy_entered.display(),
    );
    std::fs::write(&source, code).unwrap();
    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let status = std::process::Command::new(compiler)
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-pedantic"])
        .args(["-shared", "-fPIC"])
        .arg(&source)
        .arg("-I")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../plugin/include"))
        .arg("-o")
        .arg(&library)
        .status()
        .expect("compile destruction-order native fixture");
    assert!(
        status.success(),
        "destruction-order native fixture failed to build"
    );
    (directory, library)
}

#[cfg(target_os = "linux")]
fn blocking_entry_fixture(
    entry_log: &std::path::Path,
    entry_release: &std::path::Path,
) -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("blocking_entry.c");
    let library = directory.path().join("libblocking_entry.so");
    let code = format!(
        r#"
#include <sched.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include "rsi_meta_plugin.h"

uint32_t rsi_meta_plugin_entry_v1(rsi_meta_plugin_api *output,
                                  size_t capacity) {{
  (void)output; (void)capacity;
  FILE *entered = fopen("{}", "ab");
  if (entered != NULL) {{ fputc('x', entered); fclose(entered); }}
  while (access("{}", F_OK) != 0) sched_yield();
  return RSI_META_STATUS_FAILED;
}}
"#,
        entry_log.display(),
        entry_release.display(),
    );
    std::fs::write(&source, code).unwrap();
    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let status = std::process::Command::new(compiler)
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-pedantic"])
        .args(["-shared", "-fPIC"])
        .arg(&source)
        .arg("-I")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../plugin/include"))
        .arg("-o")
        .arg(&library)
        .status()
        .expect("compile blocking-entry native fixture");
    assert!(
        status.success(),
        "blocking-entry native fixture failed to build"
    );
    (directory, library)
}

#[cfg(target_os = "linux")]
fn blocking_descriptor_fixture(
    descriptor_entered: &std::path::Path,
    descriptor_release: &std::path::Path,
) -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("blocking_descriptor.c");
    let library = directory.path().join("libblocking_descriptor.so");
    let code = format!(
        r#"
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "rsi_meta_plugin.h"

static uint32_t copy_output(const uint8_t *bytes, size_t len,
                            rsi_meta_buffer *output) {{
  uint8_t *copy = malloc(len);
  if (copy == NULL) return RSI_META_STATUS_FAILED;
  memcpy(copy, bytes, len);
  *output = (rsi_meta_buffer){{copy, len, len}};
  return RSI_META_STATUS_OK;
}}

static uint32_t descriptor(void *handle, rsi_meta_buffer *output) {{
  (void)handle;
  FILE *entered = fopen("{}", "wb");
  if (entered != NULL) {{ fputs("entered", entered); fclose(entered); }}
  while (access("{}", F_OK) != 0) sched_yield();
  static const uint8_t value[] =
      "{{\"identity\":{{\"kind\":\"builtin\",\"name\":\"fixture.blocking-descriptor\",\"revision\":\"1\"}},"
      "\"requires\":[],\"provides\":[]}}";
  return copy_output(value, sizeof(value) - 1, output);
}}

static uint32_t validate_config(void *handle, const uint8_t *input,
                                size_t input_len, rsi_meta_buffer *output) {{
  (void)handle;
  return copy_output(input, input_len, output);
}}

static uint32_t create_instance(void *handle, const uint8_t *input,
                                size_t input_len, void **instance,
                                rsi_meta_buffer *output) {{
  (void)handle; (void)input; (void)input_len;
  *instance = (void *)(uintptr_t)1;
  *output = (rsi_meta_buffer){{NULL, 0, 0}};
  return RSI_META_STATUS_OK;
}}

static uint32_t call_instance(void *instance, const rsi_meta_host_api *host,
                              const uint8_t *service, size_t service_len,
                              const uint8_t *request, size_t request_len,
                              rsi_meta_buffer *output) {{
  (void)instance; (void)host; (void)service; (void)service_len;
  return copy_output(request, request_len, output);
}}

static void destroy_handle(void *handle) {{ (void)handle; }}
static void release_buffer(rsi_meta_buffer buffer) {{ free(buffer.ptr); }}

uint32_t rsi_meta_plugin_entry_v1(rsi_meta_plugin_api *output,
                                  size_t capacity) {{
  if (output == NULL || capacity < sizeof(*output)) {{
    return RSI_META_STATUS_INVALID_ARGUMENT;
  }}
  memset(output, 0, sizeof(*output));
  output->abi_major = RSI_META_ABI_MAJOR;
  output->abi_minor = RSI_META_ABI_MINOR;
  output->struct_size = sizeof(*output);
  output->factory_handle = (void *)(uintptr_t)1;
  output->descriptor = descriptor;
  output->validate_config = validate_config;
  output->create = create_instance;
  output->call = call_instance;
  output->destroy_instance = destroy_handle;
  output->destroy_factory = destroy_handle;
  output->release_buffer = release_buffer;
  return RSI_META_STATUS_OK;
}}
"#,
        descriptor_entered.display(),
        descriptor_release.display(),
    );
    std::fs::write(&source, code).unwrap();
    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let status = std::process::Command::new(compiler)
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-pedantic"])
        .args(["-shared", "-fPIC"])
        .arg(&source)
        .arg("-I")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../plugin/include"))
        .arg("-o")
        .arg(&library)
        .status()
        .expect("compile blocking-descriptor native fixture");
    assert!(
        status.success(),
        "blocking-descriptor native fixture failed to build"
    );
    (directory, library)
}

#[derive(Debug)]
struct UpstreamFactory(PluginDescriptor);

#[derive(Debug)]
struct EchoCollisionFactory(PluginDescriptor);

#[derive(Debug)]
struct Upstream;

#[async_trait]
impl ServiceEndpoint for Upstream {
    async fn serve(
        &self,
        _: rsi_meta::InvocationContext,
        mut channel: ProviderChannel<'_>,
    ) -> Result<()> {
        while let Some(frame) = channel.recv().await {
            let mut bytes = b"upstream:".to_vec();
            bytes.extend(frame.into_bytes());
            channel.send(ServiceFrame::new(bytes)).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl PluginFactory for UpstreamFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.0
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        context.provide("upstream", "fixture.upstream", V1, Arc::new(Upstream))
    }
}

#[async_trait]
impl PluginFactory for EchoCollisionFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.0
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        context.provide("echo", "fixture.echo", V1, Arc::new(Upstream))
    }
}

fn upstream_factory() -> Arc<UpstreamFactory> {
    Arc::new(UpstreamFactory(
        PluginDescriptor::new(FactoryIdentity::builtin("upstream", "1")).providing(Provision::new(
            "upstream",
            "fixture.upstream",
            V1,
        )),
    ))
}

#[derive(Debug)]
struct CaptureFactory {
    descriptor: PluginDescriptor,
    slot: Arc<Mutex<Option<ServiceHandle>>>,
}

#[async_trait]
impl PluginFactory for CaptureFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
        let key = self.descriptor.requires[0].key.clone();
        *self.slot.lock().expect("capture poisoned") = Some(context.service(key)?);
        Ok(())
    }
}

async fn wait_active(handle: &rsi_meta::FiberHandle) {
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        handle.wait_active(&CancellationToken::new()),
    )
    .await
    .expect("activation timeout")
    .expect("fiber should activate");
}

#[cfg(target_os = "linux")]
async fn apply_loader_service(
    runtime: &Runtime,
    catalog: NativeCatalog,
    client_name: &str,
) -> ServiceHandle {
    let loader = runtime
        .root()
        .apply(
            Arc::new(LoaderFactory::new(catalog)),
            json!({ "entries": [] }),
        )
        .await
        .unwrap();
    wait_active(&loader).await;
    let slot = Arc::new(Mutex::new(None));
    let client = runtime
        .root()
        .apply(
            Arc::new(CaptureFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    client_name.to_owned(),
                    "1",
                ))
                .requiring(Requirement::new(
                    LOADER_SERVICE_KEY,
                    LOADER_CONTRACT_ID,
                    LOADER_CONTRACT_VERSION,
                )),
                slot: Arc::clone(&slot),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&client).await;
    slot.lock().unwrap().clone().unwrap()
}

async fn wait_for_file(path: &std::path::Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("native fixture did not publish its callback marker");
}

async fn apply_delayed_native(
    runtime: &Runtime,
    catalog: &NativeCatalog,
    config: Value,
) -> (rsi_meta::FiberHandle, ServiceHandle) {
    let upstream = runtime
        .root()
        .apply(upstream_factory(), Value::Null)
        .await
        .unwrap();
    wait_active(&upstream).await;
    let factory = catalog.load(native_fixture()).unwrap();
    let native = runtime.root().apply(factory, config).await.unwrap();
    wait_active(&native).await;
    let slot = Arc::new(Mutex::new(None));
    let consumer = runtime
        .root()
        .apply(
            Arc::new(CaptureFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "delayed-native-client",
                    "1",
                ))
                .requiring(Requirement::new("echo", "fixture.echo", V1)),
                slot: Arc::clone(&slot),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&consumer).await;
    let service = slot.lock().unwrap().clone().unwrap();
    (native, service)
}

#[path = "native_loader/abi_e2e.rs"]
mod abi_e2e;
#[path = "native_loader/catalog_cache.rs"]
mod catalog_cache;
#[path = "native_loader/executor_lifecycle.rs"]
mod executor_lifecycle;
#[path = "native_loader/loader_service.rs"]
mod loader_service;
