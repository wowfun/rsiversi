use async_trait::async_trait;
use rsi_meta::{
    Context, ContractVersion, FactoryIdentity, FiberState, MetaError, PluginDescriptor,
    PluginFactory, ProviderChannel, Provision, Requirement, Result, Runtime, RuntimeLimits,
    ServiceEndpoint, ServiceFrame, ServiceHandle,
};
use rsi_meta_loader::{
    CatalogOptions, LOADER_CONTRACT_ID, LOADER_CONTRACT_VERSION, LOADER_SERVICE_KEY, LoaderError,
    LoaderFactory, NativeCatalog,
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
        mut channel: ProviderChannel,
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

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
        context.provide("upstream", "fixture.upstream", V1, Arc::new(Upstream))
    }
}

#[async_trait]
impl PluginFactory for EchoCollisionFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.0
    }

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
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

    async fn activate(&self, context: Context, _: Value) -> Result<()> {
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

async fn wait_for_file(path: &std::path::Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("native fixture did not publish its callback marker");
}

#[tokio::test]
async fn real_dynamic_library_runs_through_loader_plugin_and_outbound_host_bridge() {
    let (_cache, catalog) = catalog();
    let runtime = Runtime::default();
    let upstream = runtime
        .root()
        .apply(upstream_factory(), Value::Null)
        .await
        .unwrap();
    wait_active(&upstream).await;

    let loader = runtime
        .root()
        .apply(
            Arc::new(LoaderFactory::new(catalog)),
            json!({ "entries": [] }),
        )
        .await
        .unwrap();
    wait_active(&loader).await;

    let loader_slot = Arc::new(Mutex::new(None));
    let loader_client = runtime
        .root()
        .apply(
            Arc::new(CaptureFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("loader-client", "1"))
                    .requiring(Requirement::new(
                        LOADER_SERVICE_KEY,
                        LOADER_CONTRACT_ID,
                        LOADER_CONTRACT_VERSION,
                    )),
                slot: Arc::clone(&loader_slot),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&loader_client).await;
    let command = json!({
        "operation": "load",
        "id": "native-echo",
        "artifact": native_fixture(),
        "config": { "prefix": "native:" }
    });
    let loader_service = loader_slot
        .lock()
        .expect("loader capture poisoned")
        .clone()
        .unwrap();
    let response = loader_service
        .open()
        .unwrap()
        .unary(ServiceFrame::new(serde_json::to_vec(&command).unwrap()))
        .await
        .unwrap();
    let response: Value = serde_json::from_slice(response.as_bytes()).unwrap();
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["fiber"]["state"], "active");

    let echo_slot = Arc::new(Mutex::new(None));
    let consumer = runtime
        .root()
        .apply(
            Arc::new(CaptureFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("echo-client", "1"))
                    .requiring(Requirement::new("echo", "fixture.echo", V1)),
                slot: Arc::clone(&echo_slot),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&consumer).await;
    let echo_service = echo_slot
        .lock()
        .expect("echo capture poisoned")
        .clone()
        .unwrap();
    let response = echo_service
        .open()
        .unwrap()
        .unary(ServiceFrame::new(b"hello".to_vec()))
        .await
        .unwrap();
    assert_eq!(response.as_bytes(), b"native:upstream:hello");
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test(flavor = "current_thread")]
async fn outbound_native_host_bridge_supports_a_current_thread_runtime() {
    let (_cache, catalog) = catalog_with_timeout(Duration::from_secs(2));
    let runtime = Runtime::default();
    let (_native, service) =
        apply_delayed_native(&runtime, &catalog, json!({ "prefix": "current-thread:" })).await;

    let response = service
        .open()
        .unwrap()
        .unary(ServiceFrame::new(b"bridge".to_vec()))
        .await
        .unwrap();
    assert_eq!(response.as_bytes(), b"current-thread:upstream:bridge");
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn loader_preflights_every_config_before_it_applies_any_child() {
    let (_cache, catalog) = catalog();
    let runtime = Runtime::default();
    let loader = runtime
        .root()
        .apply(
            Arc::new(LoaderFactory::new(catalog)),
            json!({
                "entries": [
                    {
                        "id": "valid",
                        "artifact": native_fixture(),
                        "config": { "prefix": "ok:" }
                    },
                    {
                        "id": "invalid",
                        "artifact": native_fixture(),
                        "config": { "prefix": 42 }
                    }
                ]
            }),
        )
        .await
        .unwrap();
    assert!(matches!(loader.snapshot().state, FiberState::Failed(_)));
    assert_eq!(
        runtime.snapshot().fibers.len(),
        1,
        "preflight leaked a child Fiber"
    );
}

#[tokio::test]
async fn loader_apply_failure_rolls_back_every_previously_applied_child() {
    let (_cache, catalog) = catalog();
    let runtime = Runtime::new(RuntimeLimits {
        maximum_fibers: 3,
        ..RuntimeLimits::default()
    })
    .unwrap();
    let upstream = runtime
        .root()
        .apply(upstream_factory(), Value::Null)
        .await
        .unwrap();
    wait_active(&upstream).await;

    let loader = runtime
        .root()
        .apply(
            Arc::new(LoaderFactory::new(catalog)),
            json!({
                "entries": [
                    {
                        "id": "applied-before-failure",
                        "artifact": native_fixture(),
                        "config": { "prefix": "first:" }
                    },
                    {
                        "id": "capacity-failure",
                        "artifact": native_fixture(),
                        "config": { "prefix": "second:" }
                    }
                ]
            }),
        )
        .await
        .unwrap();

    assert!(matches!(loader.snapshot().state, FiberState::Failed(_)));
    assert_eq!(
        runtime.snapshot().fibers.len(),
        2,
        "apply-loop failure leaked an earlier child Fiber"
    );
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn concurrent_loader_commands_cannot_claim_the_same_id_twice() {
    let (_cache, catalog) = catalog();
    let runtime = Runtime::default();
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
                    "concurrent-loader-client",
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
    let service = slot.lock().unwrap().clone().unwrap();
    let command = serde_json::to_vec(&json!({
        "operation": "load",
        "id": "same-id",
        "artifact": native_fixture(),
        "config": { "prefix": "native:" }
    }))
    .unwrap();

    let first = service
        .open()
        .unwrap()
        .unary(ServiceFrame::new(command.clone()));
    let second = service.open().unwrap().unary(ServiceFrame::new(command));
    let (first, second) = tokio::join!(first, second);
    let responses = [first.unwrap(), second.unwrap()]
        .map(|frame| serde_json::from_slice::<Value>(frame.as_bytes()).unwrap());

    assert_eq!(
        responses
            .iter()
            .filter(|response| response["ok"] == true)
            .count(),
        1
    );
    assert_eq!(
        responses
            .iter()
            .filter(|response| response["error"] == "loader entry id already exists")
            .count(),
        1
    );
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One cancellation scenario proves claim, rollback, and reuse ordering.
async fn cancelling_a_loader_command_releases_its_id_reservation() {
    let (_cache, catalog) = catalog_with_timeout(Duration::from_secs(10));
    let runtime = Runtime::default();
    let upstream = runtime
        .root()
        .apply(upstream_factory(), Value::Null)
        .await
        .unwrap();
    wait_active(&upstream).await;
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
                    "cancelled-loader-client",
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
    let service = slot.lock().unwrap().clone().unwrap();
    let markers = tempfile::tempdir().unwrap();
    let entered = markers.path().join("create-entered");
    let release = markers.path().join("create-release");
    let completed = markers.path().join("create-completed");
    let task_entered = entered.clone();
    let task_release = release.clone();
    let task_completed = completed.clone();
    let first = tokio::spawn({
        let service = service.clone();
        let command = serde_json::to_vec(&json!({
            "operation": "load",
            "id": "retryable",
            "artifact": native_fixture(),
            "config": {
                "prefix": "cancelled:",
                "create_entered_path": task_entered,
                "create_release_path": task_release,
                "create_completed_path": task_completed
            }
        }))
        .unwrap();
        async move {
            service
                .open()
                .unwrap()
                .unary(ServiceFrame::new(command))
                .await
        }
    });
    wait_for_file(&entered).await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());

    let overlapping = serde_json::to_vec(&json!({
        "operation": "load",
        "id": "retryable",
        "artifact": markers.path().join("does-not-exist.so"),
        "config": {}
    }))
    .unwrap();
    let overlapping = service
        .open()
        .unwrap()
        .unary(ServiceFrame::new(overlapping))
        .await
        .unwrap();
    let overlapping: Value = serde_json::from_slice(overlapping.as_bytes()).unwrap();
    assert_eq!(
        overlapping["error"], "loader entry id already exists",
        "a cancelled load released its ID before the runtime-owned apply settled: {overlapping}"
    );

    std::fs::write(&release, b"release").unwrap();
    wait_for_file(&completed).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while runtime.snapshot().fibers.len() != 3 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelled Loader command did not roll back its child");

    let retry = serde_json::to_vec(&json!({
        "operation": "load",
        "id": "retryable",
        "artifact": native_fixture(),
        "config": { "prefix": "retry:" }
    }))
    .unwrap();
    let response = service
        .open()
        .unwrap()
        .unary(ServiceFrame::new(retry))
        .await
        .unwrap();
    let response: Value = serde_json::from_slice(response.as_bytes()).unwrap();
    assert_eq!(response["ok"], true, "{response}");
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One real-native scenario spans load, blocked unload, conflict, and reuse.
async fn loader_id_remains_reserved_until_unload_cleanup_finishes() {
    let (_cache, catalog) = catalog_with_timeout(Duration::from_secs(2));
    let runtime = Runtime::default();
    let upstream = runtime
        .root()
        .apply(upstream_factory(), Value::Null)
        .await
        .unwrap();
    wait_active(&upstream).await;
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
                    "unloading-loader-client",
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
    let service = slot.lock().unwrap().clone().unwrap();
    let markers = tempfile::tempdir().unwrap();
    let destroy_entered = markers.path().join("destroy-entered");
    let destroy_release = markers.path().join("destroy-release");
    let initial = serde_json::to_vec(&json!({
        "operation": "load",
        "id": "stable",
        "artifact": native_fixture(),
        "config": {
            "prefix": "initial:",
            "destroy_entered_path": destroy_entered,
            "destroy_release_path": destroy_release
        }
    }))
    .unwrap();
    let load_response = service
        .open()
        .unwrap()
        .unary(ServiceFrame::new(initial))
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(load_response.as_bytes()).unwrap()["ok"],
        true
    );

    let unloading = tokio::spawn({
        let service = service.clone();
        async move {
            let command = serde_json::to_vec(&json!({
                "operation": "unload",
                "id": "stable"
            }))
            .unwrap();
            service
                .open()
                .unwrap()
                .unary(ServiceFrame::new(command))
                .await
        }
    });
    wait_for_file(&destroy_entered).await;
    let conflicting = serde_json::to_vec(&json!({
        "operation": "load",
        "id": "stable",
        "artifact": native_fixture(),
        "config": { "prefix": "conflicting:" }
    }))
    .unwrap();
    let response = service
        .open()
        .unwrap()
        .unary(ServiceFrame::new(conflicting))
        .await
        .unwrap();
    let response: Value = serde_json::from_slice(response.as_bytes()).unwrap();
    assert_eq!(response["error"], "loader entry id already exists");

    std::fs::write(&destroy_release, b"release").unwrap();
    let unloaded = unloading.await.unwrap().unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(unloaded.as_bytes()).unwrap()["ok"],
        true
    );
    let retry = serde_json::to_vec(&json!({
        "operation": "load",
        "id": "stable",
        "artifact": native_fixture(),
        "config": { "prefix": "retry:" }
    }))
    .unwrap();
    let response = service
        .open()
        .unwrap()
        .unary(ServiceFrame::new(retry))
        .await
        .unwrap();
    let response: Value = serde_json::from_slice(response.as_bytes()).unwrap();
    assert_eq!(response["ok"], true, "{response}");
    assert!(runtime.shutdown().await.is_clean());
}

#[test]
fn catalog_rejects_an_oversized_artifact_before_mapping() {
    let (_cache, catalog) = catalog();
    let file = tempfile::NamedTempFile::new().unwrap();
    file.as_file()
        .set_len(rsi_meta_loader::MAX_ARTIFACT_BYTES + 1)
        .unwrap();
    assert!(matches!(
        catalog.load(file.path()),
        Err(rsi_meta_loader::LoaderError::ArtifactTooLarge)
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn catalog_preserves_a_plugin_entry_failure_status() {
    let (_fixture, artifact) = failed_entry_fixture(rsi_meta_plugin::STATUS_PANICKED);
    let (_cache, catalog) = catalog();

    let error = catalog
        .load(artifact)
        .expect_err("a panicked plugin entry must fail loading");

    assert!(
        matches!(
            error,
            LoaderError::PluginEntry {
                status: rsi_meta_plugin::STATUS_PANICKED,
            }
        ),
        "{error}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn malformed_plugin_buffer_is_released_before_rejection() {
    let markers = tempfile::tempdir().unwrap();
    let released = markers.path().join("released");
    let (_fixture, artifact) = malformed_buffer_fixture(&released);
    let (_cache, catalog) = catalog();

    let error = catalog
        .load(artifact)
        .expect_err("a nonempty null buffer must be rejected");

    assert!(
        matches!(
            error,
            rsi_meta_loader::LoaderError::Callback {
                operation: "descriptor",
                ref message,
            } if message.contains("null pointer")
        ),
        "{error}"
    );
    assert!(released.exists(), "rejection skipped release_buffer");
}

#[test]
fn catalog_rejects_existing_cache_bytes_that_do_not_match_the_source() {
    let (cache, catalog) = catalog();
    let bytes = std::fs::read(native_fixture()).unwrap();
    let digest = hex::encode(Sha256::digest(&bytes));
    std::fs::write(cache.path().join(format!("{digest}.native")), b"collision").unwrap();

    assert!(matches!(
        catalog.load(native_fixture()),
        Err(rsi_meta_loader::LoaderError::CacheCollision(_))
    ));
}

#[cfg(unix)]
#[test]
fn live_module_reuse_does_not_consult_the_durable_cache() {
    let (cache, catalog) = catalog();
    let first = catalog.load(native_fixture()).unwrap();
    let bytes = std::fs::read(native_fixture()).unwrap();
    let digest = hex::encode(Sha256::digest(&bytes));
    std::fs::write(
        cache.path().join(format!("{digest}.native")),
        b"durable cache collision",
    )
    .unwrap();

    let second = catalog
        .load(native_fixture())
        .expect("a live private mapping does not depend on the durable cache");
    assert_eq!(first.descriptor(), second.descriptor());

    drop(first);
    drop(second);
    assert!(matches!(
        catalog.load(native_fixture()),
        Err(rsi_meta_loader::LoaderError::CacheCollision(_))
    ));
}

#[cfg(unix)]
#[test]
fn catalog_rejects_a_symlink_at_the_content_addressed_cache_path() {
    let (cache, catalog) = catalog();
    let bytes = std::fs::read(native_fixture()).unwrap();
    let digest = hex::encode(Sha256::digest(&bytes));
    std::os::unix::fs::symlink(
        native_fixture(),
        cache.path().join(format!("{digest}.native")),
    )
    .unwrap();

    assert!(matches!(
        catalog.load(native_fixture()),
        Err(rsi_meta_loader::LoaderError::InvalidInput(_))
    ));
}

#[cfg(unix)]
#[test]
fn catalog_rejects_a_fifo_without_waiting_for_a_writer() {
    let (_cache, catalog) = catalog();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("artifact.fifo");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .expect("run mkfifo")
            .success()
    );
    let started = std::time::Instant::now();
    assert!(matches!(
        catalog.load(&path),
        Err(rsi_meta_loader::LoaderError::InvalidInput(_))
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn timed_out_artifact_entry_fences_reentry_until_the_worker_returns() {
    let markers = tempfile::tempdir().unwrap();
    let entry_log = markers.path().join("entry-log");
    let entry_release = markers.path().join("entry-release");
    let (_fixture, artifact) = blocking_entry_fixture(&entry_log, &entry_release);
    let (_cache, catalog) = catalog_with_timeout(Duration::from_millis(30));

    let first = tokio::task::spawn_blocking({
        let catalog = catalog.clone();
        let artifact = artifact.clone();
        move || catalog.load(artifact)
    });
    wait_for_file(&entry_log).await;
    assert!(matches!(
        first.await.unwrap(),
        Err(LoaderError::Timeout("library load, entry, or descriptor"))
    ));

    assert!(matches!(
        catalog.load(&artifact),
        Err(LoaderError::Callback {
            operation: "load",
            ..
        })
    ));
    assert_eq!(
        std::fs::read(&entry_log).unwrap(),
        b"x",
        "a second worker re-entered a still-running native entry callback"
    );
    std::fs::write(&entry_release, b"release").unwrap();
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

#[tokio::test]
async fn native_watchdog_terminalizes_even_after_core_drops_the_adapter_future() {
    let (_cache, catalog) = catalog_with_timeout(Duration::from_millis(100));
    let runtime = Runtime::new(RuntimeLimits {
        service_call_timeout: Duration::from_millis(20),
        ..RuntimeLimits::default()
    })
    .unwrap();
    let (_native, service) = apply_delayed_native(
        &runtime,
        &catalog,
        json!({ "prefix": "native:", "delay_ms": 200 }),
    )
    .await;
    assert_eq!(
        service
            .open()
            .unwrap()
            .unary(ServiceFrame::new(b"slow".to_vec()))
            .await
            .unwrap_err(),
        MetaError::Timeout("service call")
    );
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if runtime.snapshot().terminal.is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("adapter watchdog did not terminalize the runtime");
    assert!(matches!(service.open(), Err(MetaError::RuntimeTerminal(_))));
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn timed_out_native_call_defers_destruction_until_the_callback_returns() {
    let markers = tempfile::tempdir().unwrap();
    let call_entered = markers.path().join("call-entered");
    let call_release = markers.path().join("call-release");
    let destroy_entered = markers.path().join("destroy-entered");
    let (_fixture, artifact) =
        destruction_order_fixture(&call_entered, &call_release, &destroy_entered);
    let (_cache, catalog) = catalog_with_timeout(Duration::from_millis(30));
    let runtime = Runtime::default();
    let native = runtime
        .root()
        .apply(catalog.load(artifact).unwrap(), Value::Null)
        .await
        .unwrap();
    wait_active(&native).await;
    let slot = Arc::new(Mutex::new(None));
    let consumer = runtime
        .root()
        .apply(
            Arc::new(CaptureFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                    "destruction-order-client",
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
    let call = tokio::spawn(async move {
        service
            .open()
            .unwrap()
            .unary(ServiceFrame::new(b"blocked".to_vec()))
            .await
    });
    wait_for_file(&call_entered).await;
    assert_eq!(
        call.await.unwrap().unwrap_err(),
        MetaError::Timeout("native service callback")
    );

    let report = native.dispose().await;
    assert_eq!(report.failures.len(), 1, "{report:?}");
    assert!(
        report.failures[0].error.contains("destruction timed out"),
        "{report:?}"
    );
    assert!(
        !destroy_entered.exists(),
        "destruction entered while the timed-out callback still held the instance"
    );

    std::fs::write(&call_release, b"release").unwrap();
    wait_for_file(&destroy_entered).await;
}

#[tokio::test]
async fn timed_out_factory_callback_poison_prevents_later_overlap() {
    let (_cache, catalog) = catalog_with_timeout(Duration::from_millis(30));
    let factory = catalog.load(native_fixture()).unwrap();
    let runtime = Runtime::default();
    assert!(matches!(
        runtime
            .root()
            .apply(
                factory.clone(),
                json!({ "prefix": "slow:", "validate_delay_ms": 100 }),
            )
            .await,
        Err(MetaError::Timeout("native config validation"))
    ));
    let error = runtime
        .root()
        .apply(factory, json!({ "prefix": "later:" }))
        .await
        .expect_err("a timed-out factory must remain poisoned");
    assert!(error.to_string().contains("poisoned"), "{error}");
}

#[tokio::test]
async fn native_create_timeout_terminalizes_without_publishing_the_fiber() {
    let (_cache, catalog) = catalog_with_timeout(Duration::from_millis(30));
    let runtime = Runtime::default();
    let upstream = runtime
        .root()
        .apply(upstream_factory(), Value::Null)
        .await
        .unwrap();
    wait_active(&upstream).await;
    let factory = catalog.load(native_fixture()).unwrap();
    let native = runtime
        .root()
        .apply(
            factory,
            json!({ "prefix": "slow:", "create_delay_ms": 100 }),
        )
        .await
        .unwrap();

    assert!(runtime.snapshot().terminal.is_some());
    assert!(matches!(native.snapshot().state, FiberState::Failed(_)));
}

#[tokio::test(flavor = "current_thread")]
async fn publication_failure_never_runs_native_destruction_on_the_executor() {
    let (_cache, catalog) = catalog_with_timeout(Duration::from_millis(100));
    let runtime = Runtime::default();
    let upstream = runtime
        .root()
        .apply(upstream_factory(), Value::Null)
        .await
        .unwrap();
    wait_active(&upstream).await;
    let collision = runtime
        .root()
        .apply(
            Arc::new(EchoCollisionFactory(
                PluginDescriptor::new(FactoryIdentity::builtin("echo-collision", "1"))
                    .providing(Provision::new("echo", "fixture.echo", V1)),
            )),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&collision).await;

    let factory = catalog.load(native_fixture()).unwrap();
    let application = tokio::spawn({
        let root = runtime.root();
        async move {
            root.apply(
                factory,
                json!({
                    "prefix": "native:",
                    "create_delay_ms": 50,
                    "destroy_delay_ms": 200
                }),
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if runtime
                .snapshot()
                .fibers
                .iter()
                .any(|fiber| matches!(fiber.state, FiberState::Loading))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("native activation did not begin");
    let started = std::time::Instant::now();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        started.elapsed() < Duration::from_millis(180),
        "publication failure ran the foreign destructor on the current-thread executor"
    );
    let native = application.await.unwrap().unwrap();
    assert!(matches!(native.snapshot().state, FiberState::Failed(_)));
}

#[tokio::test(flavor = "current_thread")]
async fn native_instance_destruction_is_offloaded_and_bounded() {
    let (_cache, catalog) = catalog_with_timeout(Duration::from_millis(100));
    let runtime = Runtime::default();
    let (native, _service) = apply_delayed_native(
        &runtime,
        &catalog,
        json!({ "prefix": "native:", "destroy_delay_ms": 200 }),
    )
    .await;
    let started = std::time::Instant::now();
    let disposal = tokio::spawn(async move { native.dispose().await });
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "foreign destructor blocked the current-thread executor"
    );
    let report = disposal.await.unwrap();
    assert_eq!(report.failures.len(), 1);
    assert!(report.failures[0].error.contains("destruction timed out"));
}

#[test]
fn native_create_and_call_do_not_occupy_tokios_shared_blocking_pool() {
    let asynchronous = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    asynchronous.block_on(async {
        let (_cache, catalog) = catalog_with_timeout(Duration::from_secs(2));
        let runtime = Runtime::default();
        let upstream = runtime
            .root()
            .apply(upstream_factory(), Value::Null)
            .await
            .unwrap();
        wait_active(&upstream).await;

        let markers = tempfile::tempdir().unwrap();
        let create_entered = markers.path().join("create-entered");
        let create_release = markers.path().join("create-release");
        let call_entered = markers.path().join("call-entered");
        let call_release = markers.path().join("call-release");
        let factory = catalog.load(native_fixture()).unwrap();
        let application = tokio::spawn({
            let root = runtime.root();
            let config = json!({
                "prefix": "native:",
                "create_entered_path": create_entered,
                "create_release_path": create_release,
                "call_entered_path": call_entered,
                "call_release_path": call_release
            });
            async move { root.apply(factory, config).await }
        });
        wait_for_file(&create_entered).await;
        let create_sentinel = tokio::time::timeout(
            Duration::from_millis(100),
            tokio::task::spawn_blocking(|| 17_u8),
        )
        .await
        .is_ok();
        std::fs::write(&create_release, b"release").unwrap();
        let native = application.await.unwrap().unwrap();
        wait_active(&native).await;

        let slot = Arc::new(Mutex::new(None));
        let consumer = runtime
            .root()
            .apply(
                Arc::new(CaptureFactory {
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                        "blocking-pool-client",
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
        let call = tokio::spawn(async move {
            service
                .open()
                .unwrap()
                .unary(ServiceFrame::new(b"call".to_vec()))
                .await
        });
        wait_for_file(&call_entered).await;
        let call_sentinel = tokio::time::timeout(
            Duration::from_millis(100),
            tokio::task::spawn_blocking(|| 23_u8),
        )
        .await
        .is_ok();
        std::fs::write(&call_release, b"release").unwrap();
        assert_eq!(
            call.await.unwrap().unwrap().as_bytes(),
            b"native:upstream:call"
        );
        assert!(create_sentinel, "native create occupied the blocking pool");
        assert!(call_sentinel, "native call occupied the blocking pool");
        assert!(runtime.shutdown().await.is_clean());
    });
}

#[tokio::test]
async fn loader_preflight_runs_distinct_native_artifacts_with_bounded_concurrency() {
    let (_cache, catalog) = catalog_with_timeout(Duration::from_secs(2));
    let runtime = Runtime::default();
    let artifacts = tempfile::tempdir().unwrap();
    let first_artifact = artifacts.path().join("first-native");
    let second_artifact = artifacts.path().join("second-native");
    let bytes = std::fs::read(native_fixture()).unwrap();
    std::fs::write(&first_artifact, &bytes).unwrap();
    let mut distinct_bytes = bytes;
    distinct_bytes.extend_from_slice(b"distinct-digest");
    std::fs::write(&second_artifact, distinct_bytes).unwrap();

    let markers = tempfile::tempdir().unwrap();
    let first_entered = markers.path().join("first-entered");
    let first_release = markers.path().join("first-release");
    let second_entered = markers.path().join("second-entered");
    let second_release = markers.path().join("second-release");
    let task_first_entered = first_entered.clone();
    let task_first_release = first_release.clone();
    let task_second_entered = second_entered.clone();
    let task_second_release = second_release.clone();
    let application = tokio::spawn({
        let root = runtime.root();
        async move {
            root.apply(
                Arc::new(LoaderFactory::new(catalog)),
                json!({
                    "entries": [
                        {
                            "id": "first",
                            "artifact": first_artifact,
                            "config": {
                                "prefix": "first:",
                                "validate_entered_path": task_first_entered,
                                "validate_release_path": task_first_release
                            }
                        },
                        {
                            "id": "second",
                            "artifact": second_artifact,
                            "config": {
                                "prefix": "second:",
                                "validate_entered_path": task_second_entered,
                                "validate_release_path": task_second_release
                            }
                        }
                    ]
                }),
            )
            .await
        }
    });
    let concurrent = tokio::time::timeout(Duration::from_secs(5), async {
        while !first_entered.exists() || !second_entered.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok();
    std::fs::write(&first_release, b"release").unwrap();
    std::fs::write(&second_release, b"release").unwrap();
    let loader = application.await.unwrap().unwrap();
    wait_active(&loader).await;
    assert!(concurrent, "loader preflight serialized distinct artifacts");
    assert!(runtime.shutdown().await.is_clean());
}
