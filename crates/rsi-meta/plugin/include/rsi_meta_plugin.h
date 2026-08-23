#ifndef RSI_META_PLUGIN_H
#define RSI_META_PLUGIN_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define RSI_META_ABI_MAJOR 1u
#define RSI_META_ABI_MINOR 0u
#define RSI_META_STATUS_OK 0u
#define RSI_META_STATUS_INVALID_ARGUMENT 1u
#define RSI_META_STATUS_FAILED 2u
#define RSI_META_STATUS_PANICKED 3u

typedef struct rsi_meta_buffer {
  uint8_t *ptr;
  size_t len;
  size_t capacity;
} rsi_meta_buffer;

/*
 * A zero-length buffer may be {NULL, 0, 0}. release_buffer must accept that
 * value. Every nonempty returned buffer remains owned by its allocator until
 * the matching release_buffer callback is invoked exactly once.
 *
 * Every pointer/length input and every rsi_meta_host_api pointer is borrowed
 * only for its synchronous callback. A plugin must not retain those pointers
 * or use them after the callback returns.
 */

typedef struct rsi_meta_host_api {
  uint32_t abi_major;
  uint32_t abi_minor;
  uint32_t struct_size;
  uint32_t reserved;
  void *host_handle;
  uint32_t (*call_service)(void *, const uint8_t *, size_t, const uint8_t *,
                           size_t, rsi_meta_buffer *);
  void (*release_buffer)(rsi_meta_buffer);
} rsi_meta_host_api;

typedef struct rsi_meta_plugin_api {
  uint32_t abi_major;
  uint32_t abi_minor;
  uint32_t struct_size;
  uint32_t reserved;
  void *factory_handle;
  uint32_t (*descriptor)(void *, rsi_meta_buffer *);
  uint32_t (*validate_config)(void *, const uint8_t *, size_t,
                              rsi_meta_buffer *);
  uint32_t (*create)(void *, const uint8_t *, size_t, void **,
                     rsi_meta_buffer *);
  uint32_t (*call)(void *, const rsi_meta_host_api *, const uint8_t *, size_t,
                   const uint8_t *, size_t, rsi_meta_buffer *);
  void (*destroy_instance)(void *);
  void (*destroy_factory)(void *);
  void (*release_buffer)(rsi_meta_buffer);
} rsi_meta_plugin_api;

/*
 * Major versions must match. A host may accept a plugin minor no newer than
 * its own; a plugin may accept a host minor at least as new as the minor it
 * requires. Tables grow only by appending fields. struct_size is the number of
 * initialized bytes and compatibility is checked against the minimum prefix
 * for the table's declared minor, not a newer reader's complete table size.
 *
 * Callbacks may run on arbitrary host threads. descriptor, validate_config,
 * and create are serialized for one factory. call is serialized for one
 * instance but may run concurrently for distinct instances. Destruction runs
 * exactly once after callbacks for that handle have returned. A non-null
 * instance returned by create is host-owned on every status. A plugin must not
 * synchronously form a service-call cycle that re-enters the same instance.
 */

/*
 * Before entry, the host must zero its entire output allocation and pass its
 * byte capacity. The plugin writes its known prefix and reports that prefix in
 * struct_size; untouched suffix bytes therefore remain zero/NULL.
 */
uint32_t rsi_meta_plugin_entry_v1(rsi_meta_plugin_api *output,
                                  size_t capacity);

#ifdef __cplusplus
}
#endif

#endif
