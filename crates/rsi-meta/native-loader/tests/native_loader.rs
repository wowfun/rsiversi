use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, Capability, ConfigValue, ContractVersion, DeadlineLimits, FactoryIdentity,
    FiberState, Message, MetaError, PluginFactory, PreparedActivation, ProviderChannel,
    Requirement, Result, Runtime, RuntimeLimits, ServiceEndpoint,
};
use rsi_meta_native_loader::{CatalogOptions, LoaderError, NativeCatalog, NativeCatalogLimits};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

fn resolved<T: PluginFactory>(factory: Arc<T>) -> rsi_meta::ResolvedFactory {
    rsi_meta::ResolvedFactory::linked("test", "1", rsi_meta::UpdateMode::Replayable, factory)
}

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
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while catalog.snapshot().staging_bytes != 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(catalog.snapshot().staging_bytes, 0);
}

fn wait_for_callback_quiescence(catalog: &NativeCatalog) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while catalog.snapshot().active_callbacks != 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(catalog.snapshot().active_callbacks, 0);
}

async fn wait_for_staging_release_async(catalog: &NativeCatalog) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while catalog.snapshot().staging_bytes != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("native staging admission did not drain");
}

async fn wait_for_callback_quiescence_async(catalog: &NativeCatalog) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while catalog.snapshot().active_callbacks != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("native callback admission did not drain");
}

fn wait_for_catalog_ownership_release(cache: &Path) -> NativeCatalog {
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
fn compile_c_fixture(stem: &str, code: &str) -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join(format!("{stem}.c"));
    let library = directory.path().join(format!("lib{stem}.so"));
    std::fs::write(&source, code).unwrap();
    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let status = std::process::Command::new(compiler)
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-pedantic"])
        .args(["-shared", "-fPIC"])
        .arg(&source)
        .arg("-I")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../native/include"))
        .arg("-o")
        .arg(&library)
        .status()
        .unwrap_or_else(|error| panic!("compile {stem} native fixture: {error}"));
    assert!(status.success(), "{stem} native fixture failed to build");
    (directory, library)
}

#[cfg(target_os = "linux")]
fn v2_only_fixture() -> (tempfile::TempDir, PathBuf) {
    compile_c_fixture(
        "v2_only",
        r"
#include <stdint.h>
uint32_t rsi_meta_plugin_entry_v2(void *output, uint32_t capacity) {
  (void)output; (void)capacity;
  return 0;
}
",
    )
}

#[cfg(target_os = "linux")]
fn failed_entry_fixture(status: u32, exchange_log: &Path) -> (tempfile::TempDir, PathBuf) {
    compile_c_fixture(
        "failed_entry",
        &format!(
            r#"
#include <stdio.h>
#include <string.h>
#include "rsi_meta_plugin.h"

#define ISSUER 7000u

static void mark(char value) {{
  FILE *log = fopen("{}", "ab");
  if (log != NULL) {{ fputc(value, log); fclose(log); }}
}}

static uint32_t exchange(void *state, uint32_t opcode, const void *input,
                         uint32_t input_size, void *output,
                         uint32_t output_capacity) {{
  (void)state; (void)input; (void)input_size;
  if (opcode == RSI_META_PLUGIN_IDENTITY) {{
    mark('i');
    return RSI_META_STATUS_PROTOCOL_ERROR;
  }}
  if (opcode == RSI_META_PLUGIN_DESTROY_FACTORY ||
      opcode == RSI_META_PLUGIN_FINALIZE) {{
    if (output == NULL || output_capacity < sizeof(rsi_meta_basic_output))
      return RSI_META_STATUS_BUFFER_TOO_SMALL;
    rsi_meta_basic_output *value = output;
    memset(value, 0, sizeof(*value));
    value->prefix.struct_size = sizeof(*value);
    mark(opcode == RSI_META_PLUGIN_DESTROY_FACTORY ? 'd' : 'f');
    return RSI_META_STATUS_OK;
  }}
  return RSI_META_STATUS_UNSUPPORTED;
}}

uint32_t rsi_meta_plugin_entry_v3(const rsi_meta_host_table *host,
                                  rsi_meta_plugin_table *output,
                                  uint32_t capacity) {{
  (void)host;
  if (output == NULL || capacity < sizeof(*output))
    return RSI_META_STATUS_INVALID_ARGUMENT;
  memset(output, 0, sizeof(*output));
  output->header = (rsi_meta_table_header){{RSI_META_ABI_MAJOR, RSI_META_ABI_MINOR,
                                            sizeof(*output), 0u}};
  output->issuer = ISSUER;
  output->state = (void *)(uintptr_t)1;
  output->exchange = exchange;
  output->factory = (rsi_meta_cap_id){{ISSUER, 1u, 1u,
      RSI_META_CAP_KIND_FACTORY, RSI_META_RIGHT_RETAIN | RSI_META_RIGHT_MUTATE}};
  return {status};
}}
"#,
            exchange_log.display()
        ),
    )
}

#[cfg(target_os = "linux")]
fn malformed_output_fixture(
    release_marker: &Path,
    finalize_marker: &Path,
) -> (tempfile::TempDir, PathBuf) {
    compile_c_fixture(
        "malformed_output",
        &format!(
            r#"
#include <stdio.h>
#include <string.h>
#include "rsi_meta_plugin.h"

#define ISSUER 7001u
static uint32_t exchange(void *state, uint32_t opcode, const void *input,
                         uint32_t input_size, void *output,
                         uint32_t output_capacity) {{
  (void)state; (void)input; (void)input_size;
  if (opcode == RSI_META_PLUGIN_IDENTITY) {{
    if (output == NULL || output_capacity < sizeof(rsi_meta_bytes_output))
      return RSI_META_STATUS_BUFFER_TOO_SMALL;
    rsi_meta_bytes_output *value = output;
    memset(value, 0, sizeof(*value));
    value->prefix.struct_size = sizeof(*value);
    value->prefix.release = (rsi_meta_release_id){{ISSUER, 2u, 1u}};
    value->bytes.ptr = NULL;
    value->bytes.len = 1u;
    return RSI_META_STATUS_OK;
  }}
  if (opcode == RSI_META_PLUGIN_RELEASE_OUTPUT) {{
    FILE *marker = fopen("{}", "ab");
    if (marker != NULL) {{ fputc('x', marker); fclose(marker); }}
    return RSI_META_STATUS_OK;
  }}
  if (opcode == RSI_META_PLUGIN_DESTROY_FACTORY ||
      opcode == RSI_META_PLUGIN_FINALIZE) {{
    if (output == NULL || output_capacity < sizeof(rsi_meta_basic_output))
      return RSI_META_STATUS_BUFFER_TOO_SMALL;
    rsi_meta_basic_output *value = output;
    memset(value, 0, sizeof(*value));
    value->prefix.struct_size = sizeof(*value);
    if (opcode == RSI_META_PLUGIN_FINALIZE) {{
      FILE *marker = fopen("{}", "wb");
      if (marker != NULL) {{ fputs("finalized", marker); fclose(marker); }}
    }}
    return RSI_META_STATUS_OK;
  }}
  return RSI_META_STATUS_UNSUPPORTED;
}}

uint32_t rsi_meta_plugin_entry_v3(const rsi_meta_host_table *host,
                                  rsi_meta_plugin_table *output,
                                  uint32_t capacity) {{
  (void)host;
  if (output == NULL || capacity < sizeof(*output))
    return RSI_META_STATUS_INVALID_ARGUMENT;
  memset(output, 0, sizeof(*output));
  output->header = (rsi_meta_table_header){{RSI_META_ABI_MAJOR, RSI_META_ABI_MINOR,
                                            sizeof(*output), 0u}};
  output->issuer = ISSUER;
  output->state = (void *)(uintptr_t)1;
  output->exchange = exchange;
  output->factory = (rsi_meta_cap_id){{ISSUER, 1u, 1u,
      RSI_META_CAP_KIND_FACTORY, RSI_META_RIGHT_RETAIN | RSI_META_RIGHT_MUTATE}};
  return RSI_META_STATUS_OK;
}}
"#,
            release_marker.display(),
            finalize_marker.display()
        ),
    )
}

#[cfg(target_os = "linux")]
fn blocking_entry_fixture(entry_log: &Path, entry_release: &Path) -> (tempfile::TempDir, PathBuf) {
    compile_c_fixture(
        "blocking_entry",
        &format!(
            r#"
#include <sched.h>
#include <stdio.h>
#include <unistd.h>
#include "rsi_meta_plugin.h"
uint32_t rsi_meta_plugin_entry_v3(const rsi_meta_host_table *host,
                                  rsi_meta_plugin_table *output,
                                  uint32_t capacity) {{
  (void)host; (void)output; (void)capacity;
  FILE *entered = fopen("{}", "ab");
  if (entered != NULL) {{ fputc('x', entered); fclose(entered); }}
  while (access("{}", F_OK) != 0) sched_yield();
  return RSI_META_STATUS_FAILED;
}}
"#,
            entry_log.display(),
            entry_release.display()
        ),
    )
}

#[cfg(target_os = "linux")]
fn blocking_identity_fixture(
    identity_entered: &Path,
    identity_release: &Path,
) -> (tempfile::TempDir, PathBuf) {
    compile_c_fixture(
        "blocking_identity",
        &format!(
            r#"
#include <sched.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include "rsi_meta_plugin.h"

#define ISSUER 7002u
static const uint8_t IDENTITY[] = "fixture.blocking-identity";

static uint32_t exchange(void *state, uint32_t opcode, const void *input,
                         uint32_t input_size, void *output,
                         uint32_t output_capacity) {{
  (void)state; (void)input; (void)input_size;
  if (opcode == RSI_META_PLUGIN_IDENTITY) {{
    FILE *entered = fopen("{}", "wb");
    if (entered != NULL) {{ fputs("entered", entered); fclose(entered); }}
    while (access("{}", F_OK) != 0) sched_yield();
    if (output == NULL || output_capacity < sizeof(rsi_meta_bytes_output))
      return RSI_META_STATUS_BUFFER_TOO_SMALL;
    rsi_meta_bytes_output *value = output;
    memset(value, 0, sizeof(*value));
    value->prefix.struct_size = sizeof(*value);
    value->prefix.release = (rsi_meta_release_id){{ISSUER, 2u, 1u}};
    value->bytes = (rsi_meta_bytes){{IDENTITY, sizeof(IDENTITY) - 1u}};
    return RSI_META_STATUS_OK;
  }}
  if (opcode == RSI_META_PLUGIN_RELEASE_OUTPUT)
    return RSI_META_STATUS_OK;
  if (opcode == RSI_META_PLUGIN_DESTROY_FACTORY ||
      opcode == RSI_META_PLUGIN_FINALIZE) {{
    if (output == NULL || output_capacity < sizeof(rsi_meta_basic_output))
      return RSI_META_STATUS_BUFFER_TOO_SMALL;
    rsi_meta_basic_output *value = output;
    memset(value, 0, sizeof(*value));
    value->prefix.struct_size = sizeof(*value);
    return RSI_META_STATUS_OK;
  }}
  return RSI_META_STATUS_UNSUPPORTED;
}}

uint32_t rsi_meta_plugin_entry_v3(const rsi_meta_host_table *host,
                                  rsi_meta_plugin_table *output,
                                  uint32_t capacity) {{
  (void)host;
  if (output == NULL || capacity < sizeof(*output))
    return RSI_META_STATUS_INVALID_ARGUMENT;
  memset(output, 0, sizeof(*output));
  output->header = (rsi_meta_table_header){{RSI_META_ABI_MAJOR, RSI_META_ABI_MINOR,
                                            sizeof(*output), 0u}};
  output->issuer = ISSUER;
  output->state = (void *)(uintptr_t)1;
  output->exchange = exchange;
  output->factory = (rsi_meta_cap_id){{ISSUER, 1u, 1u,
      RSI_META_CAP_KIND_FACTORY, RSI_META_RIGHT_RETAIN | RSI_META_RIGHT_MUTATE}};
  return RSI_META_STATUS_OK;
}}
"#,
            identity_entered.display(),
            identity_release.display()
        ),
    )
}

#[derive(Debug)]
struct UpstreamFactory;

#[derive(Debug)]
struct EchoCollisionFactory;

#[derive(Debug)]
struct Upstream;

#[async_trait]
impl ServiceEndpoint for Upstream {
    async fn serve(
        &self,
        _: rsi_meta::InvocationContext,
        mut channel: ProviderChannel<'_>,
    ) -> Result<()> {
        while let Some(message) = channel.recv().await {
            let (payload, capabilities) = message.into_parts();
            let mut bytes = b"upstream:".to_vec();
            bytes.extend(payload);
            channel
                .send(Message::from_parts(bytes, capabilities))
                .await?;
        }
        Ok(())
    }
}

#[async_trait]
impl PluginFactory for UpstreamFactory {
    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        plan.context()
            .provide("upstream", "fixture.upstream", V1, Arc::new(Upstream))?;
        Ok(())
    }
}

#[async_trait]
impl PluginFactory for EchoCollisionFactory {
    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        plan.context()
            .provide("echo", "fixture.echo", V1, Arc::new(Upstream))?;
        Ok(())
    }
}

fn upstream_factory() -> rsi_meta::ResolvedFactory {
    rsi_meta::ResolvedFactory::linked(
        "upstream",
        "1",
        rsi_meta::UpdateMode::Replayable,
        Arc::new(UpstreamFactory),
    )
}

#[derive(Debug)]
struct CaptureFactory {
    _identity: FactoryIdentity,
    requirement: Requirement,
    slot: Arc<Mutex<Option<Capability>>>,
}

impl CaptureFactory {
    fn new(
        identity: impl Into<rsi_meta::PluginId>,
        requirement: Requirement,
        slot: Arc<Mutex<Option<Capability>>>,
    ) -> Self {
        Self {
            _identity: FactoryIdentity::linked(identity, "1"),
            requirement,
            slot,
        }
    }
}

#[async_trait]
impl PluginFactory for CaptureFactory {
    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()).requiring(self.requirement.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let service = plan
            .inject(&self.requirement.key)
            .ok_or_else(|| MetaError::ServiceUnavailable {
                service: self.requirement.key.clone(),
            })?
            .clone();
        *self.slot.lock().expect("capture poisoned") = Some(service);
        Ok(())
    }
}

async fn wait_active(handle: &rsi_meta::FiberHandle) {
    tokio::time::timeout(
        Duration::from_secs(5),
        handle.wait_active(&CancellationToken::new()),
    )
    .await
    .expect("activation timeout")
    .expect("fiber should activate");
}

async fn assert_clean_shutdown(runtime: &Runtime) {
    let outcome = runtime.shutdown().await;
    assert!(
        outcome.is_clean(),
        "runtime shutdown did not reach clean quiescence: outcome={outcome:?}; resources={:?}; snapshot={:?}",
        runtime.resource_snapshot(),
        runtime.snapshot()
    );
}

async fn wait_for_file(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("native fixture did not publish its callback marker");
}

async fn wait_for_catalog_marker(path: &Path, catalog: &NativeCatalog) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "native fixture did not publish marker {}: catalog={:?}",
            path.display(),
            catalog.snapshot()
        )
    });
}

async fn apply_delayed_native(
    runtime: &Runtime,
    catalog: &NativeCatalog,
    config: Value,
) -> (rsi_meta::FiberHandle, Capability) {
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
            crate::resolved(Arc::new(CaptureFactory::new(
                "delayed-native-client",
                Requirement::new("echo", "fixture.echo", V1),
                Arc::clone(&slot),
            ))),
            Value::Null,
        )
        .await
        .unwrap();
    wait_active(&consumer).await;
    let service = slot.lock().unwrap().take().unwrap();
    (native, service)
}

#[path = "native_loader/abi_e2e.rs"]
mod abi_e2e;
#[path = "native_loader/catalog_cache.rs"]
mod catalog_cache;
#[path = "native_loader/executor_lifecycle.rs"]
mod executor_lifecycle;
#[cfg(target_os = "linux")]
#[path = "native_loader/host_capabilities.rs"]
mod host_capabilities;
#[cfg(target_os = "linux")]
#[path = "native_loader/unload_order.rs"]
mod unload_order;
