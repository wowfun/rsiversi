#ifndef RSI_META_PLUGIN_H
#define RSI_META_PLUGIN_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define RSI_META_ABI_MAJOR UINT32_C(0)
#define RSI_META_ABI_MINOR UINT32_C(0)
#define RSI_META_ABI_LAYOUT_SHA256 "87fff0f82faef21695613380f4aab233bfb9a273a3c255ab5c439a9dff1589f5"

#define RSI_META_LANE_CONTROL UINT32_C(0)
#define RSI_META_LANE_DATA UINT32_C(1)

#define RSI_META_POST_FRAME_ACCEPTED UINT32_C(0)
/* Queue saturation may be retried after progress. An oversized frame must be
 * re-encoded within the host maximum before retrying. */
#define RSI_META_POST_FRAME_WOULD_BLOCK UINT32_C(1)
#define RSI_META_POST_FRAME_CLOSED UINT32_C(2)

#define RSI_META_CALL_OK UINT32_C(0)
#define RSI_META_CALL_INVALID_ARGUMENT UINT32_C(1)
#define RSI_META_CALL_CLOSED UINT32_C(2)
#define RSI_META_CALL_FAILED UINT32_C(3)
#define RSI_META_CALL_PANICKED UINT32_C(4)

#define RSI_META_INIT_OK UINT32_C(0)
#define RSI_META_INIT_INVALID_HOST_API UINT32_C(1)
#define RSI_META_INIT_REJECTED UINT32_C(2)
#define RSI_META_INIT_PANICKED UINT32_C(3)

typedef uint32_t (*rsi_meta_host_post_frame_fn)(
    void *host_handle,
    uint32_t lane,
    const uint8_t *data_ptr,
    size_t data_len);

typedef struct rsi_meta_host_api {
    uint32_t abi_major;
    uint32_t abi_minor;
    uint32_t struct_size;
    uint32_t reserved;
    void *host_handle;
    rsi_meta_host_post_frame_fn host_post_frame;
} rsi_meta_host_api;

/* The host serializes callbacks for one handle, but successive callbacks may
 * run on different host threads. Plugin handles must not be thread affine. */
typedef uint32_t (*rsi_meta_plugin_on_frame_fn)(
    void *plugin_handle,
    uint32_t lane,
    const uint8_t *data_ptr,
    size_t data_len);
typedef uint32_t (*rsi_meta_plugin_shutdown_fn)(void *plugin_handle);
typedef uint32_t (*rsi_meta_plugin_destroy_fn)(void *plugin_handle);

typedef struct rsi_meta_plugin_api {
    uint32_t abi_major;
    uint32_t abi_minor;
    uint32_t struct_size;
    uint32_t reserved;
    void *plugin_handle;
    rsi_meta_plugin_on_frame_fn on_frame;
    rsi_meta_plugin_shutdown_fn shutdown;
    rsi_meta_plugin_destroy_fn destroy;
} rsi_meta_plugin_api;

typedef uint32_t (*rsi_meta_plugin_entry_v0_fn)(
    const rsi_meta_host_api *host_api,
    rsi_meta_plugin_api *plugin_api_out,
    size_t plugin_api_capacity);

uint32_t rsi_meta_plugin_entry_v0(
    const rsi_meta_host_api *host_api,
    rsi_meta_plugin_api *plugin_api_out,
    size_t plugin_api_capacity);

#ifdef __cplusplus
}
#endif

#endif /* RSI_META_PLUGIN_H */
