#ifndef RSI_META_PLUGIN_H
#define RSI_META_PLUGIN_H

#include <stdint.h>

#if UINTPTR_MAX != UINT64_MAX
#error "rsi-meta native ABI v3 requires 64-bit pointers"
#endif

#ifdef __cplusplus
extern "C" {
#endif

#define RSI_META_ABI_MAJOR 3u
#define RSI_META_ABI_MINOR 0u
#define RSI_META_MAX_DIAGNOSTIC_BYTES 65536u

#define RSI_META_STATUS_OK 0u
#define RSI_META_STATUS_INVALID_ARGUMENT 1u
#define RSI_META_STATUS_FAILED 2u
#define RSI_META_STATUS_PANICKED 3u
#define RSI_META_STATUS_PROTOCOL_ERROR 4u
#define RSI_META_STATUS_UNSUPPORTED 5u
#define RSI_META_STATUS_BUSY 6u
#define RSI_META_STATUS_REENTRANT 7u
#define RSI_META_STATUS_STALE_CAPABILITY 8u
#define RSI_META_STATUS_WRONG_CAPABILITY 9u
#define RSI_META_STATUS_LIMIT_EXCEEDED 10u
#define RSI_META_STATUS_CANCELLED 11u
#define RSI_META_STATUS_TERMINAL 12u
#define RSI_META_STATUS_BUFFER_TOO_SMALL 13u

/* Plugin-port opcodes: host calls the plugin table. */
#define RSI_META_PLUGIN_IDENTITY 1u
#define RSI_META_PLUGIN_PREPARE 2u
#define RSI_META_PLUGIN_CREATE 3u
#define RSI_META_PLUGIN_ACTIVATE 4u
#define RSI_META_PLUGIN_SERVE_PORT 5u
#define RSI_META_PLUGIN_RUN_CLEANUP 6u
#define RSI_META_PLUGIN_DESTROY_INSTANCE 7u
#define RSI_META_PLUGIN_DESTROY_FACTORY 8u
#define RSI_META_PLUGIN_CAP_RETAIN 9u
#define RSI_META_PLUGIN_CAP_RELEASE 10u
#define RSI_META_PLUGIN_RELEASE_OUTPUT 11u
#define RSI_META_PLUGIN_FINALIZE 12u

/* Host-port opcodes: plugin calls the host table. */
#define RSI_META_HOST_CAP_RETAIN 257u
#define RSI_META_HOST_CAP_RELEASE 258u
#define RSI_META_HOST_CAP_OPEN 259u
#define RSI_META_HOST_CHANNEL_RECV 260u
#define RSI_META_HOST_CHANNEL_SEND 261u
#define RSI_META_HOST_CHANNEL_FINISH_REQUESTS 262u
#define RSI_META_HOST_CHANNEL_TERMINAL 263u
#define RSI_META_HOST_CHANNEL_CANCELLED 264u
#define RSI_META_HOST_EFFECT_BEGIN 265u
#define RSI_META_HOST_EFFECT_DEFER 266u
#define RSI_META_HOST_EFFECT_COMMIT 267u
#define RSI_META_HOST_EFFECT_ABORT 268u
#define RSI_META_HOST_PROVIDE 269u
#define RSI_META_HOST_RELEASE_OUTPUT 270u

#define RSI_META_CAP_KIND_FACTORY 1u
#define RSI_META_CAP_KIND_PREPARED 2u
#define RSI_META_CAP_KIND_INSTANCE 3u
#define RSI_META_CAP_KIND_SERVICE 4u
#define RSI_META_CAP_KIND_CALL_CHANNEL 5u
#define RSI_META_CAP_KIND_PROVIDER_CHANNEL 6u
#define RSI_META_CAP_KIND_EFFECT_TXN 7u
#define RSI_META_CAP_KIND_CLEANUP 8u
#define RSI_META_CAP_KIND_ACTIVATION 9u

#define RSI_META_RIGHT_RETAIN (1u << 0)
#define RSI_META_RIGHT_OPEN (1u << 1)
#define RSI_META_RIGHT_RECEIVE (1u << 2)
#define RSI_META_RIGHT_SEND (1u << 3)
#define RSI_META_RIGHT_FINISH (1u << 4)
#define RSI_META_RIGHT_MUTATE (1u << 5)

typedef struct rsi_meta_table_header {
  uint32_t abi_major;
  uint32_t abi_minor;
  uint32_t struct_size;
  uint32_t flags;
} rsi_meta_table_header;

typedef struct rsi_meta_frame_header {
  uint32_t struct_size;
  uint32_t reserved;
} rsi_meta_frame_header;

typedef struct rsi_meta_cap_id {
  uint64_t issuer;
  uint64_t slot;
  uint64_t epoch;
  uint32_t kind;
  uint32_t rights;
} rsi_meta_cap_id;

typedef struct rsi_meta_release_id {
  uint64_t issuer;
  uint64_t slot;
  uint64_t epoch;
} rsi_meta_release_id;

typedef struct rsi_meta_bytes {
  const uint8_t *ptr;
  uint64_t len;
} rsi_meta_bytes;

typedef struct rsi_meta_message {
  rsi_meta_bytes bytes;
  const rsi_meta_cap_id *capabilities;
  uint64_t capability_count;
} rsi_meta_message;

typedef struct rsi_meta_requirement {
  rsi_meta_bytes key;
  rsi_meta_bytes contract;
  uint64_t version;
} rsi_meta_requirement;

typedef struct rsi_meta_injection {
  uint64_t requirement_index;
  rsi_meta_cap_id service;
} rsi_meta_injection;

/*
 * Every operation output starts with this prefix. A nonzero release token is
 * adopted before the receiver interprets status or any later field. The token
 * owns every pointer range published by that output. Owned-cap frames separately
 * specify which transferable initial capability leases the token also owns.
 */
typedef struct rsi_meta_output_prefix {
  uint32_t struct_size;
  uint32_t reserved;
  rsi_meta_release_id release;
  rsi_meta_bytes diagnostic;
} rsi_meta_output_prefix;

typedef struct rsi_meta_cap_input {
  rsi_meta_frame_header header;
  rsi_meta_cap_id capability;
} rsi_meta_cap_input;

typedef struct rsi_meta_open_input {
  rsi_meta_frame_header header;
  rsi_meta_cap_id scope;
  rsi_meta_cap_id service;
} rsi_meta_open_input;

typedef struct rsi_meta_empty_input {
  rsi_meta_frame_header header;
} rsi_meta_empty_input;

typedef struct rsi_meta_release_output_input {
  rsi_meta_frame_header header;
  rsi_meta_release_id release;
} rsi_meta_release_output_input;

typedef struct rsi_meta_bytes_input {
  rsi_meta_frame_header header;
  rsi_meta_cap_id receiver;
  rsi_meta_bytes bytes;
} rsi_meta_bytes_input;

typedef struct rsi_meta_message_input {
  rsi_meta_frame_header header;
  rsi_meta_cap_id channel;
  rsi_meta_message message;
} rsi_meta_message_input;

typedef struct rsi_meta_activate_input {
  rsi_meta_frame_header header;
  uint64_t callback_id;
  rsi_meta_cap_id instance;
  rsi_meta_cap_id activation;
  const rsi_meta_injection *injections;
  uint64_t injection_count;
} rsi_meta_activate_input;

typedef struct rsi_meta_serve_input {
  rsi_meta_frame_header header;
  uint64_t callback_id;
  rsi_meta_cap_id instance;
  rsi_meta_cap_id provider;
  rsi_meta_bytes port;
} rsi_meta_serve_input;

typedef struct rsi_meta_effect_defer_input {
  rsi_meta_frame_header header;
  rsi_meta_cap_id transaction;
  rsi_meta_cap_id cleanup;
  rsi_meta_bytes label;
} rsi_meta_effect_defer_input;

typedef struct rsi_meta_provide_input {
  rsi_meta_frame_header header;
  rsi_meta_cap_id transaction;
  rsi_meta_bytes port;
  rsi_meta_bytes key;
  rsi_meta_bytes contract;
  uint64_t version;
} rsi_meta_provide_input;

typedef struct rsi_meta_basic_output {
  rsi_meta_output_prefix prefix;
} rsi_meta_basic_output;

typedef struct rsi_meta_bytes_output {
  rsi_meta_output_prefix prefix;
  rsi_meta_bytes bytes;
} rsi_meta_bytes_output;

typedef struct rsi_meta_cap_output {
  rsi_meta_output_prefix prefix;
  rsi_meta_cap_id capability;
} rsi_meta_cap_output;

/*
 * A callback-frame-owned capability. Unlike CapOutput, the capability is not
 * owned by prefix.release and must not have RIGHT_RETAIN. The release token can
 * own diagnostic pointer ranges only. The callback seal or an explicit
 * one-shot state transition ends the capability's lifetime.
 */
typedef struct rsi_meta_borrowed_cap_output {
  rsi_meta_output_prefix prefix;
  rsi_meta_cap_id capability;
} rsi_meta_borrowed_cap_output;

typedef struct rsi_meta_message_output {
  rsi_meta_output_prefix prefix;
  uint32_t present;
  uint32_t reserved;
  rsi_meta_message message;
} rsi_meta_message_output;

typedef struct rsi_meta_bool_output {
  rsi_meta_output_prefix prefix;
  uint32_t value;
  uint32_t reserved;
} rsi_meta_bool_output;

typedef struct rsi_meta_prepare_output {
  rsi_meta_output_prefix prefix;
  rsi_meta_cap_id prepared;
  rsi_meta_bytes normalized_config;
  const rsi_meta_requirement *requirements;
  uint64_t requirement_count;
  uint64_t retained_bytes;
} rsi_meta_prepare_output;

/*
 * Exact operation frames. All named outputs are zero-initialized by the
 * caller before exchange. Operations documented as status-only pass a null
 * output and zero output_capacity.
 *
 * Plugin port:
 *   IDENTITY          CapInput(factory)       -> BytesOutput
 *   PREPARE           BytesInput(factory)     -> PrepareOutput
 *   CREATE            CapInput(prepared)      -> CapOutput(instance)
 *   ACTIVATE          ActivateInput            -> BasicOutput
 *   SERVE_PORT        ServeInput(provider channel) -> BasicOutput
 *   RUN_CLEANUP       CapInput(cleanup)        -> BasicOutput
 *   DESTROY_INSTANCE  CapInput(instance)       -> BasicOutput
 *   DESTROY_FACTORY   CapInput(factory)        -> BasicOutput
 *   CAP_RETAIN        CapInput                 -> BasicOutput
 *   CAP_RELEASE       CapInput                 -> BasicOutput
 *   RELEASE_OUTPUT    ReleaseOutputInput       -> status only
 *   FINALIZE          EmptyInput               -> BasicOutput
 *
 * Host port:
 *   CAP_RETAIN        CapInput                 -> BasicOutput
 *   CAP_RELEASE       CapInput                 -> BasicOutput
 *   CAP_OPEN          OpenInput(scope, service) -> BorrowedCapOutput(caller channel)
 *   CHANNEL_RECV      CapInput(caller/provider channel) -> MessageOutput
 *   CHANNEL_SEND      MessageInput(caller/provider channel) -> BasicOutput
 *   FINISH_REQUESTS   CapInput(caller channel) -> BasicOutput
 *   CHANNEL_TERMINAL  CapInput(caller channel) -> BasicOutput
 *   CHANNEL_CANCELLED CapInput(caller/provider channel) -> BoolOutput
 *   EFFECT_BEGIN      CapInput(activation)     -> BorrowedCapOutput(effect txn)
 *   EFFECT_DEFER      EffectDeferInput         -> BasicOutput
 *   EFFECT_COMMIT     CapInput(effect txn)     -> BasicOutput
 *   EFFECT_ABORT      CapInput(effect txn)     -> BasicOutput
 *   PROVIDE           ProvideInput             -> CapOutput(service)
 *   RELEASE_OUTPUT    ReleaseOutputInput       -> status only
 */

typedef uint32_t (*rsi_meta_exchange_fn)(void *state, uint32_t opcode,
                                         const void *input,
                                         uint32_t input_size, void *output,
                                         uint32_t output_capacity);

typedef struct rsi_meta_host_table {
  rsi_meta_table_header header;
  uint64_t issuer;
  void *state;
  rsi_meta_exchange_fn exchange;
} rsi_meta_host_table;

typedef struct rsi_meta_plugin_table {
  rsi_meta_table_header header;
  uint64_t issuer;
  void *state;
  rsi_meta_exchange_fn exchange;
  rsi_meta_cap_id factory;
} rsi_meta_plugin_table;

/*
 * All input/output structs use the exact declared struct_size prefix and zero
 * reserved fields. Tables extend only by appending fields. Major versions must
 * match. A host accepts a plugin minor no newer than its own; a plugin accepts
 * a host minor at least as new as the minor it requires.
 *
 * Every count and byte length is uint64_t. Before converting to an addressable
 * size, receivers must check conversion, count multiplication, the applicable
 * bounds, null-with-nonzero, and alignment. Message bytes are opaque; identity,
 * requirement, and diagnostic bytes must be UTF-8 and at most
 * RSI_META_MAX_DIAGNOSTIC_BYTES long. A structural check cannot prove that an
 * arbitrary non-null native pointer is readable: plugins remain trusted
 * in-process code.
 *
 * Capability metadata is issuer-authoritative. issuer and slot zero are
 * invalid. Reusing a slot increments its epoch; epoch exhaustion permanently
 * retires that slot. Plugin-table issuer allocation never wraps or reuses an
 * exhausted sequence; entry returns LIMIT_EXCEEDED with no table authority
 * instead. Imports validate their complete set and budget before retaining
 * anything, then roll back an accepted prefix if a later retain fails. kind
 * and rights must match the issuer table exactly and can never be elevated by
 * a receiver.
 *
 * A CapOutput capability has RIGHT_RETAIN. Its output release token owns the
 * published initial lease, so the receiver retains/imports it before releasing
 * output. BorrowedCapOutput is a distinct authority even though its layout is
 * identical: its capability has no RIGHT_RETAIN and belongs to the callback
 * frame, while its release token can own diagnostic pointer ranges only.
 * Every capability in MessageOutput is transferable, has RIGHT_RETAIN, and has
 * an initial lease owned by the output token. Message input rejects any
 * capability without the exact issuer-advertised RIGHT_RETAIN metadata.
 * PrepareOutput.retained_bytes is the plugin's explicit conservative charge for
 * state retained behind the PREPARED capability until that attempt is created
 * or released. It is not inferred from native memory and is not output-token
 * storage. The host validates and reserves it before adopting the prepared
 * attempt. Zero is valid only when the retained prepared state is truly empty.
 *
 * ACTIVATION is a host-issued callback-local capability with exactly
 * RIGHT_MUTATE and cannot be retained. ACTIVATE must exchange it exactly once
 * through EFFECT_BEGIN for the Runtime-owned setup record installed before
 * native entry. An empty activation still begins and requests acceptance of
 * that record. In
 * the safe Rust SDK, EffectTxn.commit records a local CommitRequested state and
 * closes further mutation; it does not call EFFECT_COMMIT from user code. The
 * adapter calls EFFECT_COMMIT exactly once only after user activation returns
 * success with that exact state. EFFECT_COMMIT is the host adapter's one-shot
 * acceptance of the native subprotocol state, not publication or irrevocable
 * commit of the core activation root. Only after the adapter itself returns
 * success may the Runtime commit its already-installed root, and a lifecycle
 * fence can still fail and roll it back. Error, panic, or drop aborts both Open and
 * CommitRequested states. Success with no request, a duplicate request, or
 * mutation after the request is a protocol error. If EFFECT_COMMIT fails, the
 * adapter uses any remaining effect authority to abort/join cleanup and must
 * report ACTIVATE failure. A raw plugin success is likewise rejected unless
 * the one Runtime record was begun and its adapter acceptance was recorded.
 * EFFECT_TXN, CALL_CHANNEL, and PROVIDER_CHANNEL are likewise callback-local
 * and non-retainable. SERVE_PORT receives exactly a PROVIDER_CHANNEL with
 * RIGHT_RECEIVE | RIGHT_SEND. CAP_OPEN returns exactly a CALL_CHANNEL with
 * RIGHT_RECEIVE | RIGHT_SEND | RIGHT_FINISH. These are different protocols,
 * not interchangeable views of one kind.
 *
 * Every CAP_OPEN also presents the exact live callback-local capability that
 * owns the returned borrowed channel. Activation uses its begun EFFECT_TXN;
 * provider code uses its PROVIDER_CHANNEL; nested caller code uses its current
 * CALL_CHANNEL. The host validates that scope and attaches the new channel to
 * the same callback frame. A global HostTable, TLS, or thread identity is not
 * callback-frame authority.
 *
 * A caller channel may SEND requests until one successful, one-shot
 * FINISH_REQUESTS transition, and may RECV responses independently. After
 * FINISH_REQUESTS, SEND and another FINISH_REQUESTS are protocol errors. When
 * the core call reaches any terminal outcome, RECV publishes present=0 and the
 * host caches the exact clean, error, cancellation, or deadline outcome.
 * CHANNEL_TERMINAL is a one-shot observation valid only after that EOF; it
 * returns the cached outcome without losing cancellation or deadline status.
 * Further RECV or TERMINAL operations are protocol errors.
 *
 * A provider channel may RECV requests until request EOF, SEND responses, and
 * query CHANNEL_CANCELLED. FINISH_REQUESTS and CHANNEL_TERMINAL are invalid for
 * PROVIDER_CHANNEL, and another RECV after request EOF is a protocol error.
 * Returning from SERVE_PORT owns the provider-side terminal result. Callback
 * seal closes either channel orientation.
 *
 * A CLEANUP capability has exactly RIGHT_MUTATE and cannot be retained.
 * EFFECT_DEFER atomically moves its exact initial lease to the host only on
 * STATUS_OK; on any failure the lease remains plugin-owned and the plugin must
 * release it. The host neither retains nor clones that lease. For every moved
 * cleanup, rollback, drop, or successful retirement calls RUN_CLEANUP exactly
 * once and then CAP_RELEASE exactly once. RUN_CLEANUP consumes only the cleanup
 * action, not its lease. CAP_RELEASE before RUN_CLEANUP completes, duplicate
 * moves, duplicate cleanup runs, or duplicate releases are protocol errors.
 *
 * Every ACTIVATE and SERVE_PORT callback_id is nonzero lineage supplied by the
 * host, not authority. A recursive callback chain preserves it unchanged.
 *
 * RELEASE_OUTPUT accepts only rsi_meta_release_id. The receiver never returns
 * pointer, length, capacity, or allocator metadata chosen by the receiver.
 * Release, cap retain/release, transaction commit/abort, and destruction are
 * one-shot state transitions; duplicates return a protocol error and never
 * invoke undefined behavior.
 * DESTROY_INSTANCE returns BUSY while that instance's callback gate is owned.
 * A successful destroy closes callback admission and terminalizes the instance
 * lifecycle before consuming the capability, so a callback that raced an
 * earlier capability lookup cannot enter plugin instance code afterward.
 *
 * DESTROY_FACTORY consumes only the factory capability. It deliberately does
 * not destroy the PluginTable control block. FINALIZE is the only transport
 * destruction authority: it succeeds only after factory destruction and after
 * every plugin-issued capability, release token, callback reference, and other
 * admitted exchange has returned. Before beginning FINALIZE, the host closes
 * its own admission in front of every raw table access: no new invocation may
 * load or call exchange, and every invocation begun earlier must have returned
 * or completed plugin admission. The plugin atomically closes its admission
 * only when FINALIZE is its sole admitted exchange. A failed FINALIZE leaves
 * the table valid. Ordinary host admission remains closed while the exclusive
 * finalizer lane invokes exactly RELEASE_OUTPUT if that failure published a
 * diagnostic release token; only then may ordinary admission reopen. A
 * successful FINALIZE publishes no release token; after it returns, the
 * PluginTable state and the opposite HostTable state are invalid: neither side
 * may invoke an exchange again. Before permitting successful FINALIZE, a
 * plugin must have joined plugin-owned threads and returned every copied host
 * table and owned SDK handle that could invoke it. An in-state gate cannot
 * protect a thread that retained either raw pointer without first obeying the
 * owning side's admission.
 *
 * No exchange lock may be held while calling the opposite exchange port or
 * user code. Every callback-local channel/effect capability is sealed when its
 * callback returns. Callback admission is checked and returns LIMIT_EXCEEDED
 * at counter saturation rather than wrapping into a false finalizable state. A
 * callback into the same instance with the same lineage returns REENTRANT,
 * regardless of port; unrelated contention returns BUSY without waiting.
 * Implementations must contain both a plugin panic and destruction of its panic
 * payload before returning through a C function pointer; neither may unwind
 * across this ABI.
 */

/*
 * The host zeroes output_capacity bytes before entry. The plugin writes only
 * its known prefix, sets header.struct_size to that prefix, and leaves the
 * suffix untouched. Only a structurally compatible PluginTable transfers its
 * factory and table cleanup authority, including on a non-OK status. If the
 * returned table prefix is incompatible, entry cleans up before returning and
 * leaves factory and state zero. The host validates the structural prefix
 * before adopting any authority from it.
 */
uint32_t rsi_meta_plugin_entry_v3(const rsi_meta_host_table *host,
                                  rsi_meta_plugin_table *plugin_out,
                                  uint32_t output_capacity);

#ifdef __cplusplus
}
#endif

#endif
