use super::*;

#[cfg(target_os = "linux")]
const UNLOAD_ORDER_SOURCE: &str = r#"
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "rsi_meta_plugin.h"

#define ISSUER 8117u
#define IDENTITY_RELEASE_SLOT 101u
#define PREPARE_RELEASE_SLOT 102u
#define CREATE_RELEASE_SLOT 103u
#define MARKER_PATH "@MARKER@"

typedef struct fixture_state {
  rsi_meta_host_table host;
  uint64_t prepared_refs;
  uint64_t instance_refs;
  uint64_t cleanup_refs;
  uint32_t factory_live;
  uint32_t prepared_consumed;
  uint32_t instance_active;
  uint32_t cleanup_moved;
  uint32_t cleanup_ran;
  uint32_t identity_output;
  uint32_t prepare_output;
  uint32_t create_output;
} fixture_state;

static const uint8_t IDENTITY[] = "fixture.unload-order";
static const uint8_t NORMALIZED[] = "{}";
static const uint8_t CLEANUP_LABEL[] = "ordered cleanup";

static rsi_meta_cap_id capability(uint64_t slot, uint32_t kind,
                                  uint32_t rights) {
  rsi_meta_cap_id value;
  value.issuer = ISSUER;
  value.slot = slot;
  value.epoch = 1u;
  value.kind = kind;
  value.rights = rights;
  return value;
}

static rsi_meta_release_id release_id(uint64_t slot) {
  rsi_meta_release_id value;
  value.issuer = ISSUER;
  value.slot = slot;
  value.epoch = 1u;
  return value;
}

static int same_cap(rsi_meta_cap_id left, rsi_meta_cap_id right) {
  return left.issuer == right.issuer && left.slot == right.slot &&
         left.epoch == right.epoch && left.kind == right.kind &&
         left.rights == right.rights;
}

static int same_release(rsi_meta_release_id left,
                        rsi_meta_release_id right) {
  return left.issuer == right.issuer && left.slot == right.slot &&
         left.epoch == right.epoch;
}

static int valid_frame(const void *input, uint32_t input_size,
                       uint32_t expected) {
  const rsi_meta_frame_header *header = input;
  return input != NULL && input_size == expected &&
         header->struct_size == expected && header->reserved == 0u;
}

static uint32_t write_basic(void *output, uint32_t output_capacity) {
  rsi_meta_basic_output *value = output;
  if (output == NULL || output_capacity < sizeof(*value))
    return RSI_META_STATUS_BUFFER_TOO_SMALL;
  memset(value, 0, sizeof(*value));
  value->prefix.struct_size = sizeof(*value);
  return RSI_META_STATUS_OK;
}

static int mark(char value) {
  FILE *file = fopen(MARKER_PATH, "ab");
  int wrote;
  int closed;
  if (file == NULL)
    return 0;
  wrote = fputc(value, file) != EOF;
  closed = fclose(file) == 0;
  return wrote && closed;
}

#if defined(__GNUC__)
__attribute__((destructor))
#endif
static void library_unloaded(void) {
  (void)mark('u');
}

static uint32_t release_host_output(fixture_state *state,
                                    rsi_meta_release_id release) {
  rsi_meta_release_output_input input;
  if (release.issuer == 0u && release.slot == 0u && release.epoch == 0u)
    return RSI_META_STATUS_OK;
  memset(&input, 0, sizeof(input));
  input.header.struct_size = sizeof(input);
  input.release = release;
  return state->host.exchange(state->host.state, RSI_META_HOST_RELEASE_OUTPUT,
                              &input, sizeof(input), NULL, 0u);
}

static uint32_t host_borrowed_cap(fixture_state *state, uint32_t opcode,
                                  const void *input, uint32_t input_size,
                                  uint32_t kind, uint32_t rights,
                                  rsi_meta_cap_id *capability_out) {
  rsi_meta_borrowed_cap_output output;
  uint32_t status;
  uint32_t release_status;
  memset(&output, 0, sizeof(output));
  status = state->host.exchange(state->host.state, opcode, input, input_size,
                                &output, sizeof(output));
  release_status = release_host_output(state, output.prefix.release);
  if (status != RSI_META_STATUS_OK)
    return status;
  if (release_status != RSI_META_STATUS_OK)
    return release_status;
  if (output.prefix.struct_size != sizeof(output) ||
      output.prefix.reserved != 0u ||
      output.capability.issuer != state->host.issuer ||
      output.capability.slot == 0u || output.capability.epoch == 0u ||
      output.capability.kind != kind || output.capability.rights != rights)
    return RSI_META_STATUS_PROTOCOL_ERROR;
  *capability_out = output.capability;
  return RSI_META_STATUS_OK;
}

static uint32_t host_basic(fixture_state *state, uint32_t opcode,
                           const void *input, uint32_t input_size) {
  rsi_meta_basic_output output;
  uint32_t status;
  uint32_t release_status;
  memset(&output, 0, sizeof(output));
  status = state->host.exchange(state->host.state, opcode, input, input_size,
                                &output, sizeof(output));
  release_status = release_host_output(state, output.prefix.release);
  if (status != RSI_META_STATUS_OK)
    return status;
  if (release_status != RSI_META_STATUS_OK)
    return release_status;
  if (output.prefix.struct_size != sizeof(output) ||
      output.prefix.reserved != 0u)
    return RSI_META_STATUS_PROTOCOL_ERROR;
  return RSI_META_STATUS_OK;
}

static uint32_t identity(fixture_state *state, const void *input,
                         uint32_t input_size, void *output,
                         uint32_t output_capacity) {
  const rsi_meta_cap_input *frame = input;
  rsi_meta_bytes_output *value = output;
  const rsi_meta_cap_id factory = capability(
      1u, RSI_META_CAP_KIND_FACTORY,
      RSI_META_RIGHT_RETAIN | RSI_META_RIGHT_MUTATE);
  if (!valid_frame(input, input_size, sizeof(*frame)) ||
      !same_cap(frame->capability, factory) || !state->factory_live)
    return RSI_META_STATUS_WRONG_CAPABILITY;
  if (output == NULL || output_capacity < sizeof(*value) ||
      state->identity_output)
    return RSI_META_STATUS_PROTOCOL_ERROR;
  memset(value, 0, sizeof(*value));
  value->prefix.struct_size = sizeof(*value);
  value->prefix.release = release_id(IDENTITY_RELEASE_SLOT);
  value->bytes.ptr = IDENTITY;
  value->bytes.len = sizeof(IDENTITY) - 1u;
  state->identity_output = 1u;
  return RSI_META_STATUS_OK;
}

static uint32_t prepare(fixture_state *state, const void *input,
                        uint32_t input_size, void *output,
                        uint32_t output_capacity) {
  const rsi_meta_bytes_input *frame = input;
  rsi_meta_prepare_output *value = output;
  const rsi_meta_cap_id factory = capability(
      1u, RSI_META_CAP_KIND_FACTORY,
      RSI_META_RIGHT_RETAIN | RSI_META_RIGHT_MUTATE);
  if (!valid_frame(input, input_size, sizeof(*frame)) ||
      !same_cap(frame->receiver, factory) || !state->factory_live)
    return RSI_META_STATUS_WRONG_CAPABILITY;
  if (output == NULL || output_capacity < sizeof(*value) ||
      state->prepared_refs != 0u || state->prepare_output)
    return RSI_META_STATUS_PROTOCOL_ERROR;
  memset(value, 0, sizeof(*value));
  value->prefix.struct_size = sizeof(*value);
  value->prefix.release = release_id(PREPARE_RELEASE_SLOT);
  value->prepared = capability(
      2u, RSI_META_CAP_KIND_PREPARED,
      RSI_META_RIGHT_RETAIN | RSI_META_RIGHT_MUTATE);
  value->normalized_config.ptr = NORMALIZED;
  value->normalized_config.len = sizeof(NORMALIZED) - 1u;
  value->requirements = NULL;
  value->requirement_count = 0u;
  value->retained_bytes = 0u;
  state->prepared_refs = 1u;
  state->prepare_output = 1u;
  return RSI_META_STATUS_OK;
}

static uint32_t create(fixture_state *state, const void *input,
                       uint32_t input_size, void *output,
                       uint32_t output_capacity) {
  const rsi_meta_cap_input *frame = input;
  rsi_meta_cap_output *value = output;
  const rsi_meta_cap_id prepared = capability(
      2u, RSI_META_CAP_KIND_PREPARED,
      RSI_META_RIGHT_RETAIN | RSI_META_RIGHT_MUTATE);
  if (!valid_frame(input, input_size, sizeof(*frame)) ||
      !same_cap(frame->capability, prepared) ||
      state->prepared_refs != 1u || state->prepared_consumed)
    return RSI_META_STATUS_PROTOCOL_ERROR;
  if (output == NULL || output_capacity < sizeof(*value) ||
      state->instance_refs != 0u || state->create_output)
    return RSI_META_STATUS_PROTOCOL_ERROR;
  memset(value, 0, sizeof(*value));
  value->prefix.struct_size = sizeof(*value);
  value->prefix.release = release_id(CREATE_RELEASE_SLOT);
  value->capability = capability(
      3u, RSI_META_CAP_KIND_INSTANCE,
      RSI_META_RIGHT_RETAIN | RSI_META_RIGHT_MUTATE);
  state->prepared_consumed = 1u;
  state->instance_refs = 1u;
  state->create_output = 1u;
  return RSI_META_STATUS_OK;
}

static uint32_t retain(fixture_state *state, const void *input,
                       uint32_t input_size, void *output,
                       uint32_t output_capacity) {
  const rsi_meta_cap_input *frame = input;
  const rsi_meta_cap_id prepared = capability(
      2u, RSI_META_CAP_KIND_PREPARED,
      RSI_META_RIGHT_RETAIN | RSI_META_RIGHT_MUTATE);
  const rsi_meta_cap_id instance = capability(
      3u, RSI_META_CAP_KIND_INSTANCE,
      RSI_META_RIGHT_RETAIN | RSI_META_RIGHT_MUTATE);
  uint32_t status = write_basic(output, output_capacity);
  if (status != RSI_META_STATUS_OK ||
      !valid_frame(input, input_size, sizeof(*frame)))
    return status != RSI_META_STATUS_OK ? status : RSI_META_STATUS_INVALID_ARGUMENT;
  if (same_cap(frame->capability, prepared) && state->prepared_refs == 1u) {
    state->prepared_refs = 2u;
    return RSI_META_STATUS_OK;
  }
  if (same_cap(frame->capability, instance) && state->instance_refs == 1u) {
    state->instance_refs = 2u;
    return RSI_META_STATUS_OK;
  }
  return RSI_META_STATUS_PROTOCOL_ERROR;
}

static uint32_t release_cap(fixture_state *state, const void *input,
                            uint32_t input_size, void *output,
                            uint32_t output_capacity) {
  const rsi_meta_cap_input *frame = input;
  const rsi_meta_cap_id prepared = capability(
      2u, RSI_META_CAP_KIND_PREPARED,
      RSI_META_RIGHT_RETAIN | RSI_META_RIGHT_MUTATE);
  const rsi_meta_cap_id cleanup = capability(
      4u, RSI_META_CAP_KIND_CLEANUP, RSI_META_RIGHT_MUTATE);
  uint32_t status = write_basic(output, output_capacity);
  if (status != RSI_META_STATUS_OK ||
      !valid_frame(input, input_size, sizeof(*frame)))
    return status != RSI_META_STATUS_OK ? status : RSI_META_STATUS_INVALID_ARGUMENT;
  if (same_cap(frame->capability, prepared) && state->prepared_refs == 1u) {
    state->prepared_refs = 0u;
    return RSI_META_STATUS_OK;
  }
  if (same_cap(frame->capability, cleanup) && state->cleanup_refs == 1u &&
      state->cleanup_moved && state->cleanup_ran) {
    if (!mark('r'))
      return RSI_META_STATUS_FAILED;
    state->cleanup_refs = 0u;
    return RSI_META_STATUS_OK;
  }
  return RSI_META_STATUS_PROTOCOL_ERROR;
}

static uint32_t release_output(fixture_state *state, const void *input,
                               uint32_t input_size) {
  const rsi_meta_release_output_input *frame = input;
  if (!valid_frame(input, input_size, sizeof(*frame)))
    return RSI_META_STATUS_INVALID_ARGUMENT;
  if (same_release(frame->release, release_id(IDENTITY_RELEASE_SLOT)) &&
      state->identity_output) {
    state->identity_output = 0u;
    return RSI_META_STATUS_OK;
  }
  if (same_release(frame->release, release_id(PREPARE_RELEASE_SLOT)) &&
      state->prepare_output && state->prepared_refs == 2u) {
    state->prepare_output = 0u;
    state->prepared_refs = 1u;
    return RSI_META_STATUS_OK;
  }
  if (same_release(frame->release, release_id(CREATE_RELEASE_SLOT)) &&
      state->create_output && state->instance_refs == 2u) {
    state->create_output = 0u;
    state->instance_refs = 1u;
    return RSI_META_STATUS_OK;
  }
  return RSI_META_STATUS_PROTOCOL_ERROR;
}

static uint32_t activate(fixture_state *state, const void *input,
                         uint32_t input_size, void *output,
                         uint32_t output_capacity) {
  const rsi_meta_activate_input *frame = input;
  const rsi_meta_cap_id instance = capability(
      3u, RSI_META_CAP_KIND_INSTANCE,
      RSI_META_RIGHT_RETAIN | RSI_META_RIGHT_MUTATE);
  rsi_meta_cap_id transaction;
  rsi_meta_cap_input begin;
  rsi_meta_effect_defer_input defer;
  rsi_meta_cap_input commit;
  uint32_t status;
  if (!valid_frame(input, input_size, sizeof(*frame)) ||
      !same_cap(frame->instance, instance) || state->instance_refs != 1u ||
      state->instance_active || frame->callback_id == 0u ||
      frame->injection_count != 0u)
    return RSI_META_STATUS_PROTOCOL_ERROR;
  status = write_basic(output, output_capacity);
  if (status != RSI_META_STATUS_OK)
    return status;

  memset(&begin, 0, sizeof(begin));
  begin.header.struct_size = sizeof(begin);
  begin.capability = frame->activation;
  status = host_borrowed_cap(
      state, RSI_META_HOST_EFFECT_BEGIN, &begin, sizeof(begin),
      RSI_META_CAP_KIND_EFFECT_TXN, RSI_META_RIGHT_MUTATE, &transaction);
  if (status != RSI_META_STATUS_OK)
    return status;

  state->cleanup_refs = 1u;
  memset(&defer, 0, sizeof(defer));
  defer.header.struct_size = sizeof(defer);
  defer.transaction = transaction;
  defer.cleanup = capability(
      4u, RSI_META_CAP_KIND_CLEANUP, RSI_META_RIGHT_MUTATE);
  defer.label.ptr = CLEANUP_LABEL;
  defer.label.len = sizeof(CLEANUP_LABEL) - 1u;
  status = host_basic(state, RSI_META_HOST_EFFECT_DEFER, &defer, sizeof(defer));
  if (status != RSI_META_STATUS_OK) {
    state->cleanup_refs = 0u;
    return status;
  }
  state->cleanup_moved = 1u;

  memset(&commit, 0, sizeof(commit));
  commit.header.struct_size = sizeof(commit);
  commit.capability = transaction;
  status = host_basic(state, RSI_META_HOST_EFFECT_COMMIT, &commit,
                      sizeof(commit));
  if (status != RSI_META_STATUS_OK)
    return status;
  state->instance_active = 1u;
  return RSI_META_STATUS_OK;
}

static uint32_t run_cleanup(fixture_state *state, const void *input,
                            uint32_t input_size, void *output,
                            uint32_t output_capacity) {
  const rsi_meta_cap_input *frame = input;
  const rsi_meta_cap_id cleanup = capability(
      4u, RSI_META_CAP_KIND_CLEANUP, RSI_META_RIGHT_MUTATE);
  if (!valid_frame(input, input_size, sizeof(*frame)) ||
      !same_cap(frame->capability, cleanup) || state->cleanup_refs != 1u ||
      !state->cleanup_moved || state->cleanup_ran)
    return RSI_META_STATUS_PROTOCOL_ERROR;
  if (write_basic(output, output_capacity) != RSI_META_STATUS_OK)
    return RSI_META_STATUS_BUFFER_TOO_SMALL;
  if (!mark('c'))
    return RSI_META_STATUS_FAILED;
  state->cleanup_ran = 1u;
  return RSI_META_STATUS_OK;
}

static uint32_t destroy_instance(fixture_state *state, const void *input,
                                 uint32_t input_size, void *output,
                                 uint32_t output_capacity) {
  const rsi_meta_cap_input *frame = input;
  const rsi_meta_cap_id instance = capability(
      3u, RSI_META_CAP_KIND_INSTANCE,
      RSI_META_RIGHT_RETAIN | RSI_META_RIGHT_MUTATE);
  if (!valid_frame(input, input_size, sizeof(*frame)) ||
      !same_cap(frame->capability, instance) || state->instance_refs != 1u ||
      state->cleanup_refs != 0u || !state->instance_active)
    return RSI_META_STATUS_PROTOCOL_ERROR;
  if (write_basic(output, output_capacity) != RSI_META_STATUS_OK)
    return RSI_META_STATUS_BUFFER_TOO_SMALL;
  if (!mark('i'))
    return RSI_META_STATUS_FAILED;
  state->instance_refs = 0u;
  state->instance_active = 0u;
  return RSI_META_STATUS_OK;
}

static uint32_t destroy_factory(fixture_state *state, const void *input,
                                uint32_t input_size, void *output,
                                uint32_t output_capacity) {
  const rsi_meta_cap_input *frame = input;
  const rsi_meta_cap_id factory = capability(
      1u, RSI_META_CAP_KIND_FACTORY,
      RSI_META_RIGHT_RETAIN | RSI_META_RIGHT_MUTATE);
  if (!valid_frame(input, input_size, sizeof(*frame)) ||
      !same_cap(frame->capability, factory) || !state->factory_live ||
      state->prepared_refs != 0u || state->instance_refs != 0u ||
      state->cleanup_refs != 0u || state->identity_output ||
      state->prepare_output || state->create_output)
    return RSI_META_STATUS_PROTOCOL_ERROR;
  if (write_basic(output, output_capacity) != RSI_META_STATUS_OK)
    return RSI_META_STATUS_BUFFER_TOO_SMALL;
  if (!mark('d'))
    return RSI_META_STATUS_FAILED;
  state->factory_live = 0u;
  return RSI_META_STATUS_OK;
}

static uint32_t finalize(fixture_state *state, const void *input,
                         uint32_t input_size, void *output,
                         uint32_t output_capacity) {
  const rsi_meta_empty_input *frame = input;
  uint32_t status;
  if (!valid_frame(input, input_size, sizeof(*frame)) ||
      state->factory_live || state->prepared_refs != 0u ||
      state->instance_refs != 0u || state->cleanup_refs != 0u ||
      state->identity_output || state->prepare_output || state->create_output)
    return RSI_META_STATUS_PROTOCOL_ERROR;
  status = write_basic(output, output_capacity);
  if (status != RSI_META_STATUS_OK)
    return status;
  if (!mark('f'))
    return RSI_META_STATUS_FAILED;
  free(state);
  return RSI_META_STATUS_OK;
}

static uint32_t exchange(void *opaque, uint32_t opcode, const void *input,
                         uint32_t input_size, void *output,
                         uint32_t output_capacity) {
  fixture_state *state = opaque;
  if (state == NULL)
    return RSI_META_STATUS_INVALID_ARGUMENT;
  switch (opcode) {
    case RSI_META_PLUGIN_IDENTITY:
      return identity(state, input, input_size, output, output_capacity);
    case RSI_META_PLUGIN_PREPARE:
      return prepare(state, input, input_size, output, output_capacity);
    case RSI_META_PLUGIN_CREATE:
      return create(state, input, input_size, output, output_capacity);
    case RSI_META_PLUGIN_ACTIVATE:
      return activate(state, input, input_size, output, output_capacity);
    case RSI_META_PLUGIN_RUN_CLEANUP:
      return run_cleanup(state, input, input_size, output, output_capacity);
    case RSI_META_PLUGIN_CAP_RETAIN:
      return retain(state, input, input_size, output, output_capacity);
    case RSI_META_PLUGIN_CAP_RELEASE:
      return release_cap(state, input, input_size, output, output_capacity);
    case RSI_META_PLUGIN_RELEASE_OUTPUT:
      return release_output(state, input, input_size);
    case RSI_META_PLUGIN_DESTROY_INSTANCE:
      return destroy_instance(state, input, input_size, output,
                              output_capacity);
    case RSI_META_PLUGIN_DESTROY_FACTORY:
      return destroy_factory(state, input, input_size, output,
                             output_capacity);
    case RSI_META_PLUGIN_FINALIZE:
      return finalize(state, input, input_size, output, output_capacity);
    default:
      return RSI_META_STATUS_UNSUPPORTED;
  }
}

uint32_t rsi_meta_plugin_entry_v3(const rsi_meta_host_table *host,
                                  rsi_meta_plugin_table *output,
                                  uint32_t output_capacity) {
  fixture_state *state;
  if (host == NULL || output == NULL ||
      output_capacity < sizeof(*output) ||
      host->header.abi_major != RSI_META_ABI_MAJOR ||
      host->header.struct_size < sizeof(*host) || host->header.flags != 0u ||
      host->issuer == 0u || host->exchange == NULL)
    return RSI_META_STATUS_INVALID_ARGUMENT;
  state = calloc(1u, sizeof(*state));
  if (state == NULL)
    return RSI_META_STATUS_FAILED;
  state->host = *host;
  state->factory_live = 1u;
  memset(output, 0, sizeof(*output));
  output->header.abi_major = RSI_META_ABI_MAJOR;
  output->header.abi_minor = RSI_META_ABI_MINOR;
  output->header.struct_size = sizeof(*output);
  output->issuer = ISSUER;
  output->state = state;
  output->exchange = exchange;
  output->factory = capability(
      1u, RSI_META_CAP_KIND_FACTORY,
      RSI_META_RIGHT_RETAIN | RSI_META_RIGHT_MUTATE);
  return RSI_META_STATUS_OK;
}
"#;

#[cfg(target_os = "linux")]
fn unload_order_fixture(marker: &Path) -> (tempfile::TempDir, PathBuf) {
    let marker = marker
        .to_str()
        .expect("temporary marker path is UTF-8")
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let code = UNLOAD_ORDER_SOURCE.replace("@MARKER@", &marker);
    compile_c_fixture("unload_order", &code)
}

#[cfg(target_os = "linux")]
async fn wait_for_exact_sequence(path: &Path, expected: &[u8], catalog: &NativeCatalog) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let observed = std::fs::read(path).unwrap_or_default();
            assert!(
                expected.starts_with(&observed),
                "native unload departed from its required prefix: expected={expected:?}, observed={observed:?}, catalog={:?}",
                catalog.snapshot()
            );
            if observed == expected {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "native unload did not reach the exact sequence: expected={expected:?}, observed={:?}, catalog={:?}",
            std::fs::read(path).unwrap_or_default(),
            catalog.snapshot()
        )
    });
}

#[cfg(target_os = "linux")]
async fn wait_for_teardown_quiescence(catalog: &NativeCatalog) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = catalog.snapshot();
            if snapshot.staging_bytes == 0
                && snapshot.active_loads == 0
                && snapshot.active_callbacks == 0
                && snapshot.active_instances == 0
                && snapshot.pending_instance_destructions == 0
                && snapshot.active_destructions == 0
                && snapshot.queued_destructions == 0
                && snapshot.host_capabilities == 0
                && snapshot.host_outputs == 0
                && snapshot.host_output_bytes == 0
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "native teardown retained resources: {:?}",
            catalog.snapshot()
        )
    });
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn real_dylib_unload_orders_cleanup_release_instance_factory_finalize_and_unmap() {
    let markers = tempfile::tempdir().unwrap();
    let sequence = markers.path().join("unload-sequence");
    let (_fixture, artifact) = unload_order_fixture(&sequence);
    let (_cache, catalog) = catalog();
    let runtime = Runtime::default();

    let native = runtime
        .root()
        .apply(catalog.load(artifact).unwrap(), json!({}))
        .await
        .unwrap();
    wait_active(&native).await;

    let report = native.dispose().await;
    assert!(
        report.is_clean(),
        "native disposal was not clean: {report:?}"
    );
    drop(native);
    assert_clean_shutdown(&runtime).await;
    drop(runtime);

    wait_for_exact_sequence(&sequence, b"cridfu", &catalog).await;
    wait_for_teardown_quiescence(&catalog).await;
    let snapshot = catalog.snapshot();
    assert_eq!(snapshot.retained_failed_finalizations, 0, "{snapshot:?}");
    assert_eq!(std::fs::read(sequence).unwrap(), b"cridfu");
}
