#include <stdint.h>
#include <string.h>
#include "rsi_meta_plugin.h"

#ifndef RSI_FIXTURE_FINALIZE_STATUS
#define RSI_FIXTURE_FINALIZE_STATUS RSI_META_STATUS_FAILED
#endif

#ifdef RSI_FIXTURE_IDENTITY_ENTERED
#include <sched.h>
#include <stdio.h>
#include <unistd.h>
#endif

#define ISSUER 7101u
static const uint8_t IDENTITY[] = "fixture.retained-finalizer";

static uint32_t exchange(void *state, uint32_t opcode, const void *input,
                         uint32_t input_size, void *output,
                         uint32_t output_capacity) {
  (void)state;
  (void)input;
  (void)input_size;
  if (opcode == RSI_META_PLUGIN_IDENTITY) {
#ifdef RSI_FIXTURE_IDENTITY_ENTERED
    FILE *entered = fopen(RSI_FIXTURE_IDENTITY_ENTERED, "ab");
    if (entered != NULL) {
      fputs("entered", entered);
      fclose(entered);
    }
    while (access(RSI_FIXTURE_IDENTITY_RELEASE, F_OK) != 0)
      sched_yield();
#endif
    if (output == NULL || output_capacity < sizeof(rsi_meta_bytes_output))
      return RSI_META_STATUS_BUFFER_TOO_SMALL;
    rsi_meta_bytes_output *value = output;
    memset(value, 0, sizeof(*value));
    value->prefix.struct_size = sizeof(*value);
    value->prefix.release = (rsi_meta_release_id){ISSUER, 2u, 1u};
    value->bytes = (rsi_meta_bytes){IDENTITY, sizeof(IDENTITY) - 1u};
    return RSI_META_STATUS_OK;
  }
  if (opcode == RSI_META_PLUGIN_RELEASE_OUTPUT)
    return RSI_META_STATUS_OK;
  if (opcode == RSI_META_PLUGIN_DESTROY_FACTORY ||
      opcode == RSI_META_PLUGIN_FINALIZE) {
    if (output == NULL || output_capacity < sizeof(rsi_meta_basic_output))
      return RSI_META_STATUS_BUFFER_TOO_SMALL;
    rsi_meta_basic_output *value = output;
    memset(value, 0, sizeof(*value));
    value->prefix.struct_size = sizeof(*value);
    return opcode == RSI_META_PLUGIN_FINALIZE ? RSI_FIXTURE_FINALIZE_STATUS
                                              : RSI_META_STATUS_OK;
  }
  return RSI_META_STATUS_UNSUPPORTED;
}

uint32_t rsi_meta_plugin_entry_v3(const rsi_meta_host_table *host,
                                  rsi_meta_plugin_table *output,
                                  uint32_t capacity) {
  (void)host;
  if (output == NULL || capacity < sizeof(*output))
    return RSI_META_STATUS_INVALID_ARGUMENT;
  memset(output, 0, sizeof(*output));
  output->header = (rsi_meta_table_header){RSI_META_ABI_MAJOR, RSI_META_ABI_MINOR,
                                         sizeof(*output), 0u};
  output->issuer = ISSUER;
  output->state = (void *)(uintptr_t)1;
  output->exchange = exchange;
  output->factory = (rsi_meta_cap_id){ISSUER, 1u, 1u, RSI_META_CAP_KIND_FACTORY,
                                    RSI_META_RIGHT_RETAIN | RSI_META_RIGHT_MUTATE};
  return RSI_META_STATUS_OK;
}
