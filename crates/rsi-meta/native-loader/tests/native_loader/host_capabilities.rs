use super::*;

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_lines)] // Keep the hostile C translation unit intact at the real-dylib seam.
fn hostile_host_capability_fixture() -> (tempfile::TempDir, PathBuf) {
    compile_c_fixture(
        "host_capability_boundary",
        r#"
#include <stdint.h>
#include <string.h>
#include "rsi_meta_plugin.h"

#define PLUGIN_ISSUER 8101u
#define FACTORY_SLOT 1u
#define PREPARED_SLOT 2u
#define INSTANCE_SLOT 3u
#define RELEASE_IDENTITY 1u
#define RELEASE_PREPARE 2u
#define RELEASE_CREATE 3u
#define SERVICE_RIGHTS (RSI_META_RIGHT_RETAIN | RSI_META_RIGHT_OPEN)
#define MUTABLE_RIGHTS (RSI_META_RIGHT_RETAIN | RSI_META_RIGHT_MUTATE)
#define CALLER_RIGHTS \
  (RSI_META_RIGHT_RECEIVE | RSI_META_RIGHT_SEND | RSI_META_RIGHT_FINISH)

static rsi_meta_host_table host_table;
static uint64_t factory_references;
static uint64_t prepared_references;
static uint64_t instance_references;
static uint64_t release_epochs[4];
static uint32_t release_active[4];

static const uint8_t identity_bytes[] = "fixture.host-capability-boundary";
static const uint8_t normalized_config[] = "null";
static const uint8_t upstream_key[] = "upstream";
static const uint8_t upstream_contract[] = "fixture.upstream";
static const uint8_t rejected_payload[] = "rejected";
static const uint8_t accepted_payload[] = "accepted";
static const uint8_t expected_response[] = "upstream:accepted";

static const rsi_meta_requirement upstream_requirement = {
  {upstream_key, sizeof(upstream_key) - 1u},
  {upstream_contract, sizeof(upstream_contract) - 1u},
  1u
};

static rsi_meta_frame_header frame_header(uint32_t size) {
  rsi_meta_frame_header header = {size, 0u};
  return header;
}

static rsi_meta_output_prefix empty_prefix(uint32_t size) {
  rsi_meta_output_prefix prefix;
  memset(&prefix, 0, sizeof(prefix));
  prefix.struct_size = size;
  return prefix;
}

static int same_cap(rsi_meta_cap_id left, rsi_meta_cap_id right) {
  return left.issuer == right.issuer && left.slot == right.slot &&
         left.epoch == right.epoch && left.kind == right.kind &&
         left.rights == right.rights;
}

static rsi_meta_cap_id plugin_cap(uint64_t slot, uint32_t kind) {
  rsi_meta_cap_id capability = {
    PLUGIN_ISSUER, slot, 1u, kind, MUTABLE_RIGHTS
  };
  return capability;
}

static int frame_is_exact(const void *input, uint32_t input_size,
                          uint32_t expected) {
  const rsi_meta_frame_header *header = input;
  return input != NULL && input_size == expected &&
         header->struct_size == expected && header->reserved == 0u;
}

static uint32_t write_basic(void *output, uint32_t output_capacity,
                            uint32_t status) {
  rsi_meta_basic_output *value = output;
  if (output == NULL || output_capacity < sizeof(*value))
    return RSI_META_STATUS_BUFFER_TOO_SMALL;
  memset(value, 0, sizeof(*value));
  value->prefix = empty_prefix(sizeof(*value));
  return status;
}

static rsi_meta_release_id issue_release(uint64_t slot) {
  rsi_meta_release_id release = {0u, 0u, 0u};
  if (slot == 0u || slot > RELEASE_CREATE || release_active[slot] != 0u)
    return release;
  release_epochs[slot] += 1u;
  release_active[slot] = 1u;
  release.issuer = PLUGIN_ISSUER;
  release.slot = 100u + slot;
  release.epoch = release_epochs[slot];
  return release;
}

static int release_matches(rsi_meta_release_id release, uint64_t slot) {
  return release.issuer == PLUGIN_ISSUER && release.slot == 100u + slot &&
         release.epoch == release_epochs[slot] && release_active[slot] != 0u;
}

static int host_prefix_is_exact(rsi_meta_output_prefix prefix,
                                uint32_t expected) {
  return prefix.struct_size == expected && prefix.reserved == 0u;
}

static uint32_t release_host_output_status(rsi_meta_release_id release) {
  rsi_meta_release_output_input input;
  input.header = frame_header(sizeof(input));
  input.release = release;
  return host_table.exchange(host_table.state, RSI_META_HOST_RELEASE_OUTPUT,
                             &input, sizeof(input), NULL, 0u);
}

static int release_host_output(rsi_meta_release_id release) {
  if (release.issuer == 0u && release.slot == 0u && release.epoch == 0u)
    return 1;
  return release_host_output_status(release) == RSI_META_STATUS_OK;
}

static int host_output_double_release_is_protocol(rsi_meta_cap_id service) {
  rsi_meta_cap_input input;
  rsi_meta_basic_output output;
  uint32_t status;
  uint32_t first_release;
  uint32_t duplicate_release;
  service.kind = RSI_META_CAP_KIND_CALL_CHANNEL;
  memset(&output, 0, sizeof(output));
  input.header = frame_header(sizeof(input));
  input.capability = service;
  status = host_table.exchange(host_table.state, RSI_META_HOST_CAP_RETAIN,
                               &input, sizeof(input), &output, sizeof(output));
  if (status != RSI_META_STATUS_WRONG_CAPABILITY ||
      !host_prefix_is_exact(output.prefix, sizeof(output)) ||
      output.prefix.release.issuer == 0u)
    return 0;
  first_release = release_host_output_status(output.prefix.release);
  duplicate_release = release_host_output_status(output.prefix.release);
  return first_release == RSI_META_STATUS_OK &&
         duplicate_release == RSI_META_STATUS_PROTOCOL_ERROR;
}

static uint32_t host_cap_operation(uint32_t opcode,
                                   rsi_meta_cap_id capability) {
  rsi_meta_cap_input input;
  rsi_meta_basic_output output;
  uint32_t status;
  memset(&output, 0, sizeof(output));
  input.header = frame_header(sizeof(input));
  input.capability = capability;
  status = host_table.exchange(host_table.state, opcode, &input, sizeof(input),
                               &output, sizeof(output));
  if (!host_prefix_is_exact(output.prefix, sizeof(output)) ||
      !release_host_output(output.prefix.release))
    return UINT32_MAX;
  return status;
}

static uint32_t host_send(rsi_meta_cap_id channel, const uint8_t *bytes,
                          uint64_t byte_count,
                          const rsi_meta_cap_id *capabilities,
                          uint64_t capability_count) {
  rsi_meta_message_input input;
  rsi_meta_basic_output output;
  uint32_t status;
  memset(&input, 0, sizeof(input));
  memset(&output, 0, sizeof(output));
  input.header = frame_header(sizeof(input));
  input.channel = channel;
  input.message.bytes.ptr = bytes;
  input.message.bytes.len = byte_count;
  input.message.capabilities = capabilities;
  input.message.capability_count = capability_count;
  status = host_table.exchange(host_table.state, RSI_META_HOST_CHANNEL_SEND,
                               &input, sizeof(input), &output, sizeof(output));
  if (!host_prefix_is_exact(output.prefix, sizeof(output)) ||
      !release_host_output(output.prefix.release))
    return UINT32_MAX;
  return status;
}

static int host_effect_begin(rsi_meta_cap_id activation,
                             rsi_meta_cap_id *transaction) {
  rsi_meta_cap_input input;
  rsi_meta_borrowed_cap_output output;
  uint32_t status;
  memset(&output, 0, sizeof(output));
  input.header = frame_header(sizeof(input));
  input.capability = activation;
  status = host_table.exchange(host_table.state, RSI_META_HOST_EFFECT_BEGIN,
                               &input, sizeof(input), &output, sizeof(output));
  if (status != RSI_META_STATUS_OK ||
      !host_prefix_is_exact(output.prefix, sizeof(output)) ||
      output.capability.issuer != host_table.issuer ||
      output.capability.kind != RSI_META_CAP_KIND_EFFECT_TXN ||
      output.capability.rights != RSI_META_RIGHT_MUTATE ||
      !release_host_output(output.prefix.release))
    return 0;
  *transaction = output.capability;
  return 1;
}

static int host_open(rsi_meta_cap_id scope, rsi_meta_cap_id service,
                     rsi_meta_cap_id *channel) {
  rsi_meta_open_input input;
  rsi_meta_borrowed_cap_output output;
  uint32_t status;
  memset(&output, 0, sizeof(output));
  input.header = frame_header(sizeof(input));
  input.scope = scope;
  input.service = service;
  status = host_table.exchange(host_table.state, RSI_META_HOST_CAP_OPEN,
                               &input, sizeof(input), &output, sizeof(output));
  if (status != RSI_META_STATUS_OK ||
      !host_prefix_is_exact(output.prefix, sizeof(output)) ||
      output.capability.issuer != host_table.issuer ||
      output.capability.kind != RSI_META_CAP_KIND_CALL_CHANNEL ||
      output.capability.rights != CALLER_RIGHTS ||
      !release_host_output(output.prefix.release))
    return 0;
  *channel = output.capability;
  return 1;
}

static int receive_only_accepted_response(rsi_meta_cap_id channel) {
  rsi_meta_cap_input input;
  rsi_meta_message_output output;
  rsi_meta_cap_id returned_capability;
  uint32_t status;
  memset(&output, 0, sizeof(output));
  input.header = frame_header(sizeof(input));
  input.capability = channel;
  status = host_table.exchange(host_table.state, RSI_META_HOST_CHANNEL_RECV,
                               &input, sizeof(input), &output, sizeof(output));
  if (status != RSI_META_STATUS_OK ||
      !host_prefix_is_exact(output.prefix, sizeof(output)) ||
      output.present != 1u || output.reserved != 0u ||
      output.message.bytes.len != sizeof(expected_response) - 1u ||
      output.message.bytes.ptr == NULL ||
      memcmp(output.message.bytes.ptr, expected_response,
             sizeof(expected_response) - 1u) != 0 ||
      output.message.capability_count != 1u ||
      output.message.capabilities == NULL)
    return 0;
  returned_capability = output.message.capabilities[0];
  if (returned_capability.issuer != host_table.issuer ||
      returned_capability.kind != RSI_META_CAP_KIND_SERVICE ||
      returned_capability.rights != SERVICE_RIGHTS ||
      host_cap_operation(RSI_META_HOST_CAP_RETAIN, returned_capability) !=
        RSI_META_STATUS_OK ||
      !release_host_output(output.prefix.release) ||
      host_cap_operation(RSI_META_HOST_CAP_RELEASE, returned_capability) !=
        RSI_META_STATUS_OK)
    return 0;

  memset(&output, 0, sizeof(output));
  status = host_table.exchange(host_table.state, RSI_META_HOST_CHANNEL_RECV,
                               &input, sizeof(input), &output, sizeof(output));
  return status == RSI_META_STATUS_OK &&
         host_prefix_is_exact(output.prefix, sizeof(output)) &&
         output.present == 0u && output.reserved == 0u &&
         output.message.bytes.len == 0u &&
         output.message.capability_count == 0u &&
         release_host_output(output.prefix.release);
}

static int run_host_capability_checks(const rsi_meta_activate_input *input) {
  rsi_meta_cap_id service;
  rsi_meta_cap_id altered;
  rsi_meta_cap_id transaction;
  rsi_meta_cap_id channel;
  rsi_meta_cap_id bad_message_caps[2];
  rsi_meta_cap_id good_message_caps[1];

  if (input->callback_id == 0u || input->injection_count != 1u ||
      input->injections == NULL ||
      input->injections[0].requirement_index != 0u)
    return 0;
  service = input->injections[0].service;
  if (service.issuer != host_table.issuer || service.slot == 0u ||
      service.epoch == 0u || service.kind != RSI_META_CAP_KIND_SERVICE ||
      service.rights != SERVICE_RIGHTS)
    return 0;
  if (!host_output_double_release_is_protocol(service))
    return 0;

  altered = service;
  altered.issuer = 0u;
  if (host_cap_operation(RSI_META_HOST_CAP_RETAIN, altered) !=
      RSI_META_STATUS_INVALID_ARGUMENT)
    return 0;
  altered = service;
  altered.issuer = service.issuer == UINT64_MAX ? service.issuer - 1u
                                                : service.issuer + 1u;
  if (host_cap_operation(RSI_META_HOST_CAP_RETAIN, altered) !=
      RSI_META_STATUS_STALE_CAPABILITY)
    return 0;
  altered = service;
  altered.slot = 0u;
  if (host_cap_operation(RSI_META_HOST_CAP_RETAIN, altered) !=
      RSI_META_STATUS_INVALID_ARGUMENT)
    return 0;
  altered = service;
  altered.slot = UINT64_MAX;
  if (host_cap_operation(RSI_META_HOST_CAP_RETAIN, altered) !=
      RSI_META_STATUS_STALE_CAPABILITY)
    return 0;
  altered = service;
  altered.epoch = 0u;
  if (host_cap_operation(RSI_META_HOST_CAP_RETAIN, altered) !=
      RSI_META_STATUS_INVALID_ARGUMENT)
    return 0;
  altered = service;
  altered.epoch += 1u;
  if (host_cap_operation(RSI_META_HOST_CAP_RETAIN, altered) !=
      RSI_META_STATUS_STALE_CAPABILITY)
    return 0;
  altered = service;
  altered.kind = RSI_META_CAP_KIND_CALL_CHANNEL;
  if (host_cap_operation(RSI_META_HOST_CAP_RETAIN, altered) !=
      RSI_META_STATUS_WRONG_CAPABILITY)
    return 0;
  altered = service;
  altered.rights = RSI_META_RIGHT_RETAIN;
  if (host_cap_operation(RSI_META_HOST_CAP_RETAIN, altered) !=
      RSI_META_STATUS_WRONG_CAPABILITY)
    return 0;

  if (host_cap_operation(RSI_META_HOST_CAP_RETAIN, service) !=
        RSI_META_STATUS_OK ||
      host_cap_operation(RSI_META_HOST_CAP_RELEASE, service) !=
        RSI_META_STATUS_OK ||
      host_cap_operation(RSI_META_HOST_CAP_RETAIN, input->activation) !=
        RSI_META_STATUS_WRONG_CAPABILITY ||
      !host_effect_begin(input->activation, &transaction) ||
      host_cap_operation(RSI_META_HOST_CAP_RETAIN, transaction) !=
        RSI_META_STATUS_WRONG_CAPABILITY ||
      !host_open(transaction, service, &channel) ||
      host_cap_operation(RSI_META_HOST_CAP_RETAIN, channel) !=
        RSI_META_STATUS_WRONG_CAPABILITY)
    return 0;

  bad_message_caps[0] = service;
  bad_message_caps[1] = service;
  bad_message_caps[1].epoch += 1u;
  if (host_send(channel, rejected_payload, sizeof(rejected_payload) - 1u,
                bad_message_caps, 2u) != RSI_META_STATUS_STALE_CAPABILITY)
    return 0;

  good_message_caps[0] = service;
  if (host_send(channel, accepted_payload, sizeof(accepted_payload) - 1u,
                good_message_caps, 1u) != RSI_META_STATUS_OK ||
      host_cap_operation(RSI_META_HOST_CHANNEL_FINISH_REQUESTS, channel) !=
        RSI_META_STATUS_OK ||
      !receive_only_accepted_response(channel) ||
      host_cap_operation(RSI_META_HOST_CHANNEL_TERMINAL, channel) !=
        RSI_META_STATUS_OK ||
      host_cap_operation(RSI_META_HOST_EFFECT_COMMIT, transaction) !=
        RSI_META_STATUS_OK)
    return 0;
  return 1;
}

static uint64_t *plugin_references(rsi_meta_cap_id capability) {
  if (capability.issuer != PLUGIN_ISSUER || capability.epoch != 1u ||
      capability.rights != MUTABLE_RIGHTS)
    return NULL;
  if (same_cap(capability,
               plugin_cap(FACTORY_SLOT, RSI_META_CAP_KIND_FACTORY)))
    return &factory_references;
  if (same_cap(capability,
               plugin_cap(PREPARED_SLOT, RSI_META_CAP_KIND_PREPARED)))
    return &prepared_references;
  if (same_cap(capability,
               plugin_cap(INSTANCE_SLOT, RSI_META_CAP_KIND_INSTANCE)))
    return &instance_references;
  return NULL;
}

static uint32_t plugin_exchange(void *state, uint32_t opcode,
                                const void *input, uint32_t input_size,
                                void *output, uint32_t output_capacity) {
  (void)state;
  if (opcode == RSI_META_PLUGIN_IDENTITY) {
    const rsi_meta_cap_input *value = input;
    rsi_meta_bytes_output *reply = output;
    rsi_meta_release_id release;
    if (!frame_is_exact(input, input_size, sizeof(*value)) ||
        !same_cap(value->capability,
                  plugin_cap(FACTORY_SLOT, RSI_META_CAP_KIND_FACTORY)) ||
        factory_references == 0u || output == NULL ||
        output_capacity < sizeof(*reply))
      return RSI_META_STATUS_INVALID_ARGUMENT;
    release = issue_release(RELEASE_IDENTITY);
    if (release.issuer == 0u)
      return RSI_META_STATUS_PROTOCOL_ERROR;
    memset(reply, 0, sizeof(*reply));
    reply->prefix = empty_prefix(sizeof(*reply));
    reply->prefix.release = release;
    reply->bytes.ptr = identity_bytes;
    reply->bytes.len = sizeof(identity_bytes) - 1u;
    return RSI_META_STATUS_OK;
  }
  if (opcode == RSI_META_PLUGIN_PREPARE) {
    const rsi_meta_bytes_input *value = input;
    rsi_meta_prepare_output *reply = output;
    rsi_meta_release_id release;
    if (!frame_is_exact(input, input_size, sizeof(*value)) ||
        !same_cap(value->receiver,
                  plugin_cap(FACTORY_SLOT, RSI_META_CAP_KIND_FACTORY)) ||
        factory_references == 0u || prepared_references != 0u ||
        output == NULL || output_capacity < sizeof(*reply))
      return RSI_META_STATUS_INVALID_ARGUMENT;
    release = issue_release(RELEASE_PREPARE);
    if (release.issuer == 0u)
      return RSI_META_STATUS_PROTOCOL_ERROR;
    prepared_references = 1u;
    memset(reply, 0, sizeof(*reply));
    reply->prefix = empty_prefix(sizeof(*reply));
    reply->prefix.release = release;
    reply->prepared = plugin_cap(PREPARED_SLOT, RSI_META_CAP_KIND_PREPARED);
    reply->normalized_config.ptr = normalized_config;
    reply->normalized_config.len = sizeof(normalized_config) - 1u;
    reply->requirements = &upstream_requirement;
    reply->requirement_count = 1u;
    reply->retained_bytes = 0u;
    return RSI_META_STATUS_OK;
  }
  if (opcode == RSI_META_PLUGIN_CREATE) {
    const rsi_meta_cap_input *value = input;
    rsi_meta_cap_output *reply = output;
    rsi_meta_release_id release;
    if (!frame_is_exact(input, input_size, sizeof(*value)) ||
        !same_cap(value->capability,
                  plugin_cap(PREPARED_SLOT, RSI_META_CAP_KIND_PREPARED)) ||
        prepared_references == 0u || instance_references != 0u ||
        output == NULL || output_capacity < sizeof(*reply))
      return RSI_META_STATUS_INVALID_ARGUMENT;
    release = issue_release(RELEASE_CREATE);
    if (release.issuer == 0u)
      return RSI_META_STATUS_PROTOCOL_ERROR;
    instance_references = 1u;
    memset(reply, 0, sizeof(*reply));
    reply->prefix = empty_prefix(sizeof(*reply));
    reply->prefix.release = release;
    reply->capability = plugin_cap(INSTANCE_SLOT, RSI_META_CAP_KIND_INSTANCE);
    return RSI_META_STATUS_OK;
  }
  if (opcode == RSI_META_PLUGIN_ACTIVATE) {
    const rsi_meta_activate_input *value = input;
    uint32_t status;
    if (!frame_is_exact(input, input_size, sizeof(*value)) ||
        !same_cap(value->instance,
                  plugin_cap(INSTANCE_SLOT, RSI_META_CAP_KIND_INSTANCE)) ||
        instance_references == 0u)
      return write_basic(output, output_capacity,
                         RSI_META_STATUS_INVALID_ARGUMENT);
    status = run_host_capability_checks(value) ? RSI_META_STATUS_OK
                                               : RSI_META_STATUS_FAILED;
    return write_basic(output, output_capacity, status);
  }
  if (opcode == RSI_META_PLUGIN_CAP_RETAIN ||
      opcode == RSI_META_PLUGIN_CAP_RELEASE) {
    const rsi_meta_cap_input *value = input;
    uint64_t *references;
    if (!frame_is_exact(input, input_size, sizeof(*value)))
      return write_basic(output, output_capacity,
                         RSI_META_STATUS_INVALID_ARGUMENT);
    references = plugin_references(value->capability);
    if (references == NULL || *references == 0u)
      return write_basic(output, output_capacity,
                         RSI_META_STATUS_STALE_CAPABILITY);
    if (opcode == RSI_META_PLUGIN_CAP_RETAIN) {
      if (*references == UINT64_MAX)
        return write_basic(output, output_capacity,
                           RSI_META_STATUS_LIMIT_EXCEEDED);
      *references += 1u;
    } else {
      *references -= 1u;
    }
    return write_basic(output, output_capacity, RSI_META_STATUS_OK);
  }
  if (opcode == RSI_META_PLUGIN_RELEASE_OUTPUT) {
    const rsi_meta_release_output_input *value = input;
    uint64_t slot;
    if (!frame_is_exact(input, input_size, sizeof(*value)) || output != NULL ||
        output_capacity != 0u || value->release.slot <= 100u)
      return RSI_META_STATUS_INVALID_ARGUMENT;
    slot = value->release.slot - 100u;
    if (slot > RELEASE_CREATE ||
        !release_matches(value->release, slot))
      return RSI_META_STATUS_STALE_CAPABILITY;
    release_active[slot] = 0u;
    if (slot == RELEASE_PREPARE) {
      if (prepared_references == 0u)
        return RSI_META_STATUS_PROTOCOL_ERROR;
      prepared_references -= 1u;
    } else if (slot == RELEASE_CREATE) {
      if (instance_references == 0u)
        return RSI_META_STATUS_PROTOCOL_ERROR;
      instance_references -= 1u;
    }
    return RSI_META_STATUS_OK;
  }
  if (opcode == RSI_META_PLUGIN_DESTROY_INSTANCE) {
    const rsi_meta_cap_input *value = input;
    if (!frame_is_exact(input, input_size, sizeof(*value)) ||
        !same_cap(value->capability,
                  plugin_cap(INSTANCE_SLOT, RSI_META_CAP_KIND_INSTANCE)) ||
        instance_references != 1u)
      return write_basic(output, output_capacity,
                         RSI_META_STATUS_PROTOCOL_ERROR);
    instance_references = 0u;
    return write_basic(output, output_capacity, RSI_META_STATUS_OK);
  }
  if (opcode == RSI_META_PLUGIN_DESTROY_FACTORY) {
    const rsi_meta_cap_input *value = input;
    if (!frame_is_exact(input, input_size, sizeof(*value)) ||
        !same_cap(value->capability,
                  plugin_cap(FACTORY_SLOT, RSI_META_CAP_KIND_FACTORY)) ||
        factory_references != 1u)
      return write_basic(output, output_capacity,
                         RSI_META_STATUS_PROTOCOL_ERROR);
    factory_references = 0u;
    return write_basic(output, output_capacity, RSI_META_STATUS_OK);
  }
  if (opcode == RSI_META_PLUGIN_FINALIZE) {
    const rsi_meta_empty_input *value = input;
    uint64_t index;
    if (!frame_is_exact(input, input_size, sizeof(*value)))
      return write_basic(output, output_capacity,
                         RSI_META_STATUS_INVALID_ARGUMENT);
    for (index = RELEASE_IDENTITY; index <= RELEASE_CREATE; index += 1u) {
      if (release_active[index] != 0u)
        return write_basic(output, output_capacity,
                           RSI_META_STATUS_PROTOCOL_ERROR);
    }
    if (factory_references != 0u || prepared_references != 0u ||
        instance_references != 0u)
      return write_basic(output, output_capacity,
                         RSI_META_STATUS_PROTOCOL_ERROR);
    return write_basic(output, output_capacity, RSI_META_STATUS_OK);
  }
  return write_basic(output, output_capacity, RSI_META_STATUS_UNSUPPORTED);
}

uint32_t rsi_meta_plugin_entry_v3(const rsi_meta_host_table *host,
                                  rsi_meta_plugin_table *output,
                                  uint32_t output_capacity) {
  if (host == NULL || output == NULL || output_capacity < sizeof(*output) ||
      host->header.abi_major != RSI_META_ABI_MAJOR ||
      host->header.struct_size < sizeof(*host) || host->header.flags != 0u ||
      host->issuer == 0u || host->state == NULL || host->exchange == NULL)
    return RSI_META_STATUS_INVALID_ARGUMENT;
  memset(release_epochs, 0, sizeof(release_epochs));
  memset(release_active, 0, sizeof(release_active));
  host_table = *host;
  factory_references = 1u;
  prepared_references = 0u;
  instance_references = 0u;
  memset(output, 0, sizeof(*output));
  output->header.abi_major = RSI_META_ABI_MAJOR;
  output->header.abi_minor = RSI_META_ABI_MINOR;
  output->header.struct_size = sizeof(*output);
  output->header.flags = 0u;
  output->issuer = PLUGIN_ISSUER;
  output->state = &host_table;
  output->exchange = plugin_exchange;
  output->factory = plugin_cap(FACTORY_SLOT, RSI_META_CAP_KIND_FACTORY);
  return RSI_META_STATUS_OK;
}
"#,
    )
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn real_dylib_host_capabilities_reject_forgery_and_import_messages_atomically() {
    let (_fixture, artifact) = hostile_host_capability_fixture();
    let (_cache, catalog) = catalog_with_timeout(Duration::from_secs(2));
    let runtime = Runtime::default();
    let upstream = runtime
        .root()
        .apply(upstream_factory(), Value::Null)
        .await
        .unwrap();
    wait_active(&upstream).await;
    let baseline = runtime.resource_snapshot();

    let native = runtime
        .root()
        .apply(catalog.load(artifact).unwrap(), Value::Null)
        .await
        .unwrap();
    wait_active(&native).await;

    let after_callback = catalog.snapshot();
    assert_eq!(after_callback.active_instances, 1, "{after_callback:?}");
    assert_eq!(after_callback.host_capabilities, 0, "{after_callback:?}");
    assert_eq!(after_callback.host_outputs, 0, "{after_callback:?}");
    assert_eq!(after_callback.host_output_bytes, 0, "{after_callback:?}");
    assert!(
        after_callback.peak_host_capabilities >= 5,
        "the real callback did not exercise activation, effect, caller, injection, and returned service authorities: {after_callback:?}"
    );
    assert!(
        after_callback.peak_host_outputs >= 1,
        "host rejection diagnostics did not cross an owned output seam: {after_callback:?}"
    );
    let after_resources = runtime.resource_snapshot();
    assert_eq!(
        after_resources.capability_entries.current,
        baseline.capability_entries.current
    );
    assert_eq!(after_resources.queued_capability_references.current, 0);

    assert_clean_shutdown(&runtime).await;
    let released = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = catalog.snapshot();
            if snapshot.staging_bytes == 0
                && snapshot.active_callbacks == 0
                && snapshot.active_instances == 0
                && snapshot.pending_instance_destructions == 0
                && snapshot.active_destructions == 0
                && snapshot.queued_destructions == 0
                && snapshot.host_capabilities == 0
                && snapshot.host_outputs == 0
                && snapshot.host_output_bytes == 0
            {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("native ownership did not retire: {:?}", catalog.snapshot()));
    assert_eq!(released.retained_failed_finalizations, 0, "{released:?}");
}
