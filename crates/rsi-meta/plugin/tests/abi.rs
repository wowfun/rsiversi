#![allow(unsafe_code)] // This test verifies the audited raw C ABI layout.

use core::ffi::c_void;
use rsi_meta_plugin::{
    ABI_MAJOR, ABI_MINOR, ActivateInput, BasicOutput, BoolOutput, BorrowedCapOutput, BytesInput,
    BytesOutput, CAP_KIND_ACTIVATION, CAP_KIND_CALL_CHANNEL, CAP_KIND_CLEANUP, CAP_KIND_EFFECT_TXN,
    CAP_KIND_FACTORY, CAP_KIND_PROVIDER_CHANNEL, CapId, CapInput, CapOutput, EffectDeferInput,
    EmptyInput, ExchangeFn, FrameHeader, HOST_CAP_OPEN, HOST_CAP_RELEASE, HOST_CAP_RETAIN,
    HOST_CHANNEL_CANCELLED, HOST_CHANNEL_FINISH_REQUESTS, HOST_CHANNEL_RECV, HOST_CHANNEL_SEND,
    HOST_CHANNEL_TERMINAL, HOST_EFFECT_ABORT, HOST_EFFECT_BEGIN, HOST_EFFECT_COMMIT,
    HOST_EFFECT_DEFER, HOST_PROVIDE, HOST_RELEASE_OUTPUT, HostTable, Injection,
    MAX_DIAGNOSTIC_BYTES, MessageInput, MessageOutput, OpenInput, OutputPrefix, PLUGIN_ACTIVATE,
    PLUGIN_CAP_RELEASE, PLUGIN_CAP_RETAIN, PLUGIN_CREATE, PLUGIN_DESTROY_FACTORY,
    PLUGIN_DESTROY_INSTANCE, PLUGIN_ENTRY_SYMBOL, PLUGIN_FINALIZE, PLUGIN_IDENTITY, PLUGIN_PREPARE,
    PLUGIN_RELEASE_OUTPUT, PLUGIN_RUN_CLEANUP, PLUGIN_SERVE_PORT, PluginTable, PrepareOutput,
    ProvideInput, RIGHT_FINISH, RIGHT_MUTATE, RIGHT_RECEIVE, RIGHT_RETAIN, RIGHT_SEND, RawBytes,
    RawMessage, ReleaseId, ReleaseOutputInput, STATUS_BUFFER_TOO_SMALL, STATUS_BUSY,
    STATUS_CANCELLED, STATUS_FAILED, STATUS_INVALID_ARGUMENT, STATUS_LIMIT_EXCEEDED, STATUS_OK,
    STATUS_PANICKED, STATUS_PROTOCOL_ERROR, STATUS_REENTRANT, STATUS_STALE_CAPABILITY,
    STATUS_TERMINAL, STATUS_UNSUPPORTED, STATUS_WRONG_CAPABILITY, ServeInput, TableHeader,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::fs;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::process::Command;

unsafe extern "C" fn exchange(
    _: *mut c_void,
    _: u32,
    _: *const c_void,
    _: u32,
    _: *mut c_void,
    _: u32,
) -> u32 {
    STATUS_OK
}

fn host(minor: u32) -> HostTable {
    HostTable {
        header: TableHeader::new(minor, HostTable::STRUCT_SIZE),
        issuer: 11,
        state: core::ptr::dangling_mut::<c_void>(),
        exchange: Some(exchange),
    }
}

fn plugin(minor: u32) -> PluginTable {
    PluginTable {
        header: TableHeader::new(minor, PluginTable::STRUCT_SIZE),
        issuer: 22,
        state: core::ptr::dangling_mut::<c_void>(),
        exchange: Some(exchange),
        factory: CapId {
            issuer: 22,
            slot: 1,
            epoch: 1,
            kind: CAP_KIND_FACTORY,
            rights: RIGHT_RETAIN | RIGHT_MUTATE,
        },
    }
}

#[test]
fn v2_operation_frames_are_exact_and_finalization_is_separate() {
    assert_eq!(PLUGIN_FINALIZE, 12);
    assert_eq!(CAP_KIND_CALL_CHANNEL, 5);
    assert_eq!(CAP_KIND_PROVIDER_CHANNEL, 6);
    assert_eq!(CAP_KIND_EFFECT_TXN, 7);
    assert_eq!(CAP_KIND_CLEANUP, 8);
    assert_eq!(CAP_KIND_ACTIVATION, 9);
    assert_eq!(size_of::<Injection>(), 40);
    assert_eq!(size_of::<EmptyInput>(), 8);
    assert_eq!(size_of::<CapInput>(), 40);
    assert_eq!(size_of::<OpenInput>(), 72);
    assert_eq!(size_of::<BytesInput>(), 56);
    assert_eq!(size_of::<ReleaseOutputInput>(), 32);
    assert_eq!(size_of::<MessageInput>(), 72);
    assert_eq!(size_of::<ActivateInput>(), 96);
    assert_eq!(size_of::<ServeInput>(), 96);
    assert_eq!(size_of::<EffectDeferInput>(), 88);
    assert_eq!(size_of::<ProvideInput>(), 96);
    assert_eq!(size_of::<BasicOutput>(), 48);
    assert_eq!(size_of::<BytesOutput>(), 64);
    assert_eq!(size_of::<BoolOutput>(), 56);
    assert_eq!(size_of::<CapOutput>(), 80);
    assert_eq!(size_of::<BorrowedCapOutput>(), 80);
    assert_eq!(size_of::<MessageOutput>(), 88);
    assert_eq!(size_of::<PrepareOutput>(), 120);

    assert_eq!(core::mem::offset_of!(ActivateInput, callback_id), 8);
    assert_eq!(core::mem::offset_of!(ActivateInput, instance), 16);
    assert_eq!(core::mem::offset_of!(ActivateInput, activation), 48);
    assert_eq!(core::mem::offset_of!(ActivateInput, injections), 80);
    assert_eq!(core::mem::offset_of!(ServeInput, provider), 48);
    assert_eq!(core::mem::offset_of!(OpenInput, scope), 8);
    assert_eq!(core::mem::offset_of!(OpenInput, service), 40);
    assert_eq!(core::mem::offset_of!(EffectDeferInput, cleanup), 40);
    assert_eq!(core::mem::offset_of!(ProvideInput, version), 88);
    assert_eq!(core::mem::offset_of!(PrepareOutput, retained_bytes), 112);
}

#[test]
fn protocol_status_and_opcode_numbers_are_pinned() {
    assert_eq!(
        [
            STATUS_OK,
            STATUS_INVALID_ARGUMENT,
            STATUS_FAILED,
            STATUS_PANICKED,
            STATUS_PROTOCOL_ERROR,
            STATUS_UNSUPPORTED,
            STATUS_BUSY,
            STATUS_REENTRANT,
            STATUS_STALE_CAPABILITY,
            STATUS_WRONG_CAPABILITY,
            STATUS_LIMIT_EXCEEDED,
            STATUS_CANCELLED,
            STATUS_TERMINAL,
            STATUS_BUFFER_TOO_SMALL,
        ],
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13]
    );
    assert_eq!(
        [
            PLUGIN_IDENTITY,
            PLUGIN_PREPARE,
            PLUGIN_CREATE,
            PLUGIN_ACTIVATE,
            PLUGIN_SERVE_PORT,
            PLUGIN_RUN_CLEANUP,
            PLUGIN_DESTROY_INSTANCE,
            PLUGIN_DESTROY_FACTORY,
            PLUGIN_CAP_RETAIN,
            PLUGIN_CAP_RELEASE,
            PLUGIN_RELEASE_OUTPUT,
            PLUGIN_FINALIZE,
        ],
        [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]
    );
    assert_eq!(
        [
            HOST_CAP_RETAIN,
            HOST_CAP_RELEASE,
            HOST_CAP_OPEN,
            HOST_CHANNEL_RECV,
            HOST_CHANNEL_SEND,
            HOST_CHANNEL_FINISH_REQUESTS,
            HOST_CHANNEL_TERMINAL,
            HOST_CHANNEL_CANCELLED,
            HOST_EFFECT_BEGIN,
            HOST_EFFECT_DEFER,
            HOST_EFFECT_COMMIT,
            HOST_EFFECT_ABORT,
            HOST_PROVIDE,
            HOST_RELEASE_OUTPUT,
        ],
        [
            257, 258, 259, 260, 261, 262, 263, 264, 265, 266, 267, 268, 269, 270
        ]
    );
}

#[test]
fn input_header_declares_the_complete_frame_without_trailing_bytes() {
    let known = u32::try_from(size_of::<CapInput>()).unwrap();
    let current = FrameHeader::new(known);
    assert!(current.is_compatible(known, known));

    let extended = FrameHeader::new(known + 8);
    assert!(extended.is_compatible(known, known + 8));
    assert!(!current.is_compatible(known, known + 8));
    assert!(!extended.is_compatible(known, known + 16));

    let mut reserved = current;
    reserved.reserved = 1;
    assert!(!reserved.is_compatible(known, known));
}

#[test]
fn owned_and_borrowed_cap_outputs_have_distinct_rights_contracts() {
    let prefix = OutputPrefix::empty(u32::try_from(size_of::<CapOutput>()).unwrap());
    let owned = CapOutput {
        prefix,
        capability: CapId {
            issuer: 7,
            slot: 9,
            epoch: 11,
            kind: CAP_KIND_FACTORY,
            rights: RIGHT_RETAIN | RIGHT_MUTATE,
        },
    };
    assert!(owned.validate_capability_shape().is_ok());

    let borrowed = BorrowedCapOutput {
        prefix,
        capability: CapId {
            issuer: 7,
            slot: 9,
            epoch: 11,
            kind: CAP_KIND_CALL_CHANNEL,
            rights: RIGHT_RECEIVE | RIGHT_SEND | RIGHT_FINISH,
        },
    };
    assert!(borrowed.validate_capability_shape().is_ok());

    let wrongly_borrowed = BorrowedCapOutput {
        capability: owned.capability,
        ..borrowed
    };
    assert!(wrongly_borrowed.validate_capability_shape().is_err());
    let wrongly_owned = CapOutput {
        capability: borrowed.capability,
        ..owned
    };
    assert!(wrongly_owned.validate_capability_shape().is_err());
}

#[test]
fn v2_layout_uses_fixed_width_ids_and_one_exchange_port() {
    assert_eq!(ABI_MAJOR, 2);
    assert_eq!(ABI_MINOR, 0);
    assert_eq!(MAX_DIAGNOSTIC_BYTES, 65_536);
    assert_eq!(PLUGIN_ENTRY_SYMBOL, b"rsi_meta_plugin_entry_v2\0");

    assert_eq!(core::mem::size_of::<TableHeader>(), 16);
    assert_eq!(core::mem::size_of::<FrameHeader>(), 8);
    assert_eq!(core::mem::size_of::<CapId>(), 32);
    assert_eq!(core::mem::size_of::<ReleaseId>(), 24);
    assert_eq!(core::mem::offset_of!(CapId, issuer), 0);
    assert_eq!(core::mem::offset_of!(CapId, slot), 8);
    assert_eq!(core::mem::offset_of!(CapId, epoch), 16);
    assert_eq!(core::mem::offset_of!(CapId, kind), 24);
    assert_eq!(core::mem::offset_of!(CapId, rights), 28);

    assert_eq!(core::mem::offset_of!(HostTable, header), 0);
    assert_eq!(core::mem::offset_of!(HostTable, issuer), 16);
    assert_eq!(core::mem::offset_of!(HostTable, state), 24);
    assert_eq!(
        core::mem::offset_of!(HostTable, exchange),
        24 + core::mem::size_of::<*mut c_void>()
    );
    assert_eq!(HostTable::STRUCT_SIZE as usize, size_of::<HostTable>());

    assert_eq!(core::mem::offset_of!(PluginTable, header), 0);
    assert_eq!(core::mem::offset_of!(PluginTable, issuer), 16);
    assert_eq!(core::mem::offset_of!(PluginTable, state), 24);
    assert_eq!(
        core::mem::offset_of!(PluginTable, exchange),
        24 + core::mem::size_of::<*mut c_void>()
    );
    assert_eq!(
        core::mem::offset_of!(PluginTable, factory),
        24 + core::mem::size_of::<*mut c_void>() + core::mem::size_of::<ExchangeFn>()
    );
    assert_eq!(PluginTable::STRUCT_SIZE as usize, size_of::<PluginTable>());
}

#[test]
fn table_minor_compatibility_has_one_direction_per_receiver() {
    assert!(host(ABI_MINOR).is_compatible());
    assert!(host(ABI_MINOR + 1).is_compatible());
    assert!(!host(0).is_compatible_for_plugin(1));
    assert!(host(1).is_compatible_for_plugin(1));

    let current = plugin(ABI_MINOR);
    assert!(current.is_compatible_for_host(ABI_MINOR));
    assert!(current.is_compatible_for_host(ABI_MINOR + 1));
    assert!(!plugin(ABI_MINOR + 1).is_compatible_for_host(ABI_MINOR));

    let mut invalid = host(ABI_MINOR);
    invalid.header.abi_major += 1;
    assert!(!invalid.is_compatible());
    invalid = host(ABI_MINOR);
    invalid.header.flags = 1;
    assert!(!invalid.is_compatible());
    invalid = host(ABI_MINOR);
    invalid.issuer = 0;
    assert!(!invalid.is_compatible());
    invalid = host(ABI_MINOR);
    invalid.exchange = None;
    assert!(!invalid.is_compatible());

    let mut wrong_factory = plugin(ABI_MINOR);
    wrong_factory.factory.issuer += 1;
    assert!(!wrong_factory.is_compatible_for_host(ABI_MINOR));
    wrong_factory = plugin(ABI_MINOR);
    wrong_factory.factory.epoch = 0;
    assert!(!wrong_factory.is_compatible_for_host(ABI_MINOR));
}

#[test]
fn raw_pointer_shapes_reject_null_nonempty_and_integer_overflow() {
    assert_eq!(RawBytes::EMPTY.checked_len(10), Ok(0));
    assert!(
        RawBytes {
            ptr: core::ptr::null(),
            len: 1
        }
        .checked_len(10)
        .is_err()
    );
    assert!(
        RawBytes {
            ptr: core::ptr::dangling(),
            len: 11
        }
        .checked_len(10)
        .is_err()
    );
    assert!(
        RawBytes {
            ptr: core::ptr::dangling(),
            len: u64::MAX
        }
        .checked_len(usize::MAX)
        .is_err()
    );

    let message = RawMessage {
        bytes: RawBytes::EMPTY,
        capabilities: core::ptr::null(),
        capability_count: 1,
    };
    assert!(message.validate_shape(32, 4).is_err());

    let message = RawMessage {
        bytes: RawBytes::EMPTY,
        capabilities: core::ptr::dangling(),
        capability_count: u64::MAX,
    };
    assert!(message.validate_shape(32, usize::MAX).is_err());
}

#[test]
fn output_prefix_requires_a_whole_release_token_and_bounded_diagnostic() {
    let prefix_size = u32::try_from(size_of::<OutputPrefix>()).unwrap();
    let mut prefix = OutputPrefix::empty(prefix_size);
    assert!(prefix.validate(prefix_size, prefix_size, 128,).is_ok());

    prefix.release = ReleaseId {
        issuer: 1,
        slot: 0,
        epoch: 1,
    };
    assert!(prefix.validate(prefix_size, prefix_size, 128,).is_err());
    prefix.release = ReleaseId {
        issuer: 1,
        slot: 2,
        epoch: 3,
    };
    assert!(prefix.validate(prefix_size, prefix_size, 128,).is_ok());
    prefix.reserved = 1;
    assert!(prefix.validate(prefix_size, prefix_size, 128,).is_err());

    prefix = OutputPrefix::empty(prefix_size + 1);
    assert!(prefix.validate(prefix_size, prefix_size, 128,).is_err());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
#[allow(clippy::too_many_lines)] // One compiled source pins the complete public C/C++ contract.
fn maintained_header_compiles_with_c11_and_cpp17_layout_assertions() {
    let directory = tempfile::tempdir().unwrap();
    let assertions = r#"
#include <stddef.h>
#include "rsi_meta_plugin.h"

#if defined(__cplusplus)
#define RSI_META_ASSERT static_assert
#else
#define RSI_META_ASSERT _Static_assert
#endif

RSI_META_ASSERT(RSI_META_ABI_MAJOR == 2u, "ABI major drift");
RSI_META_ASSERT(RSI_META_ABI_MINOR == 0u, "ABI minor drift");
RSI_META_ASSERT(RSI_META_MAX_DIAGNOSTIC_BYTES == 65536u,
                "diagnostic limit drift");
RSI_META_ASSERT(sizeof(void *) == 8u, "pointer width drift");
RSI_META_ASSERT(RSI_META_STATUS_OK == 0u, "OK status drift");
RSI_META_ASSERT(RSI_META_STATUS_INVALID_ARGUMENT == 1u, "invalid status drift");
RSI_META_ASSERT(RSI_META_STATUS_FAILED == 2u, "failed status drift");
RSI_META_ASSERT(RSI_META_STATUS_PANICKED == 3u, "panicked status drift");
RSI_META_ASSERT(RSI_META_STATUS_PROTOCOL_ERROR == 4u, "protocol status drift");
RSI_META_ASSERT(RSI_META_STATUS_UNSUPPORTED == 5u, "unsupported status drift");
RSI_META_ASSERT(RSI_META_STATUS_BUSY == 6u, "busy status drift");
RSI_META_ASSERT(RSI_META_STATUS_REENTRANT == 7u, "reentrant status drift");
RSI_META_ASSERT(RSI_META_STATUS_STALE_CAPABILITY == 8u, "stale status drift");
RSI_META_ASSERT(RSI_META_STATUS_WRONG_CAPABILITY == 9u, "wrong status drift");
RSI_META_ASSERT(RSI_META_STATUS_LIMIT_EXCEEDED == 10u, "limit status drift");
RSI_META_ASSERT(RSI_META_STATUS_CANCELLED == 11u, "cancelled status drift");
RSI_META_ASSERT(RSI_META_STATUS_TERMINAL == 12u, "terminal status drift");
RSI_META_ASSERT(RSI_META_STATUS_BUFFER_TOO_SMALL == 13u, "buffer status drift");
RSI_META_ASSERT(RSI_META_PLUGIN_IDENTITY == 1u, "identity opcode drift");
RSI_META_ASSERT(RSI_META_PLUGIN_PREPARE == 2u, "prepare opcode drift");
RSI_META_ASSERT(RSI_META_PLUGIN_CREATE == 3u, "create opcode drift");
RSI_META_ASSERT(RSI_META_PLUGIN_ACTIVATE == 4u, "activate opcode drift");
RSI_META_ASSERT(RSI_META_PLUGIN_SERVE_PORT == 5u, "serve opcode drift");
RSI_META_ASSERT(RSI_META_PLUGIN_RUN_CLEANUP == 6u, "cleanup opcode drift");
RSI_META_ASSERT(RSI_META_PLUGIN_DESTROY_INSTANCE == 7u, "instance destroy opcode drift");
RSI_META_ASSERT(RSI_META_PLUGIN_DESTROY_FACTORY == 8u, "factory destroy opcode drift");
RSI_META_ASSERT(RSI_META_PLUGIN_CAP_RETAIN == 9u, "retain opcode drift");
RSI_META_ASSERT(RSI_META_PLUGIN_CAP_RELEASE == 10u, "release opcode drift");
RSI_META_ASSERT(RSI_META_PLUGIN_RELEASE_OUTPUT == 11u, "output opcode drift");
RSI_META_ASSERT(RSI_META_PLUGIN_FINALIZE == 12u, "finalize opcode drift");
RSI_META_ASSERT(RSI_META_HOST_CAP_RETAIN == 257u, "host retain opcode drift");
RSI_META_ASSERT(RSI_META_HOST_CAP_RELEASE == 258u, "host release opcode drift");
RSI_META_ASSERT(RSI_META_HOST_CAP_OPEN == 259u, "host open opcode drift");
RSI_META_ASSERT(RSI_META_HOST_CHANNEL_RECV == 260u, "host recv opcode drift");
RSI_META_ASSERT(RSI_META_HOST_CHANNEL_SEND == 261u, "host send opcode drift");
RSI_META_ASSERT(RSI_META_HOST_CHANNEL_FINISH_REQUESTS == 262u, "host finish opcode drift");
RSI_META_ASSERT(RSI_META_HOST_CHANNEL_TERMINAL == 263u, "host terminal opcode drift");
RSI_META_ASSERT(RSI_META_HOST_CHANNEL_CANCELLED == 264u, "host cancel opcode drift");
RSI_META_ASSERT(RSI_META_HOST_EFFECT_BEGIN == 265u, "host effect begin drift");
RSI_META_ASSERT(RSI_META_HOST_EFFECT_DEFER == 266u, "host effect defer drift");
RSI_META_ASSERT(RSI_META_HOST_EFFECT_COMMIT == 267u, "host effect commit drift");
RSI_META_ASSERT(RSI_META_HOST_EFFECT_ABORT == 268u, "host effect abort drift");
RSI_META_ASSERT(RSI_META_HOST_PROVIDE == 269u, "host provide drift");
RSI_META_ASSERT(RSI_META_HOST_RELEASE_OUTPUT == 270u, "host output drift");
RSI_META_ASSERT(RSI_META_CAP_KIND_CALL_CHANNEL == 5u, "caller channel kind");
RSI_META_ASSERT(RSI_META_CAP_KIND_PROVIDER_CHANNEL == 6u,
                "provider channel kind");
RSI_META_ASSERT(RSI_META_CAP_KIND_EFFECT_TXN == 7u, "effect transaction kind");
RSI_META_ASSERT(RSI_META_CAP_KIND_CLEANUP == 8u, "cleanup kind");
RSI_META_ASSERT(RSI_META_CAP_KIND_ACTIVATION == 9u, "activation kind");
RSI_META_ASSERT(sizeof(rsi_meta_table_header) == 16u, "table header size");
RSI_META_ASSERT(sizeof(rsi_meta_frame_header) == 8u, "frame header size");
RSI_META_ASSERT(sizeof(rsi_meta_cap_id) == 32u, "cap id size");
RSI_META_ASSERT(offsetof(rsi_meta_cap_id, epoch) == 16u, "cap epoch offset");
RSI_META_ASSERT(offsetof(rsi_meta_cap_id, rights) == 28u, "cap rights offset");
RSI_META_ASSERT(sizeof(rsi_meta_release_id) == 24u, "release id size");
RSI_META_ASSERT(sizeof(rsi_meta_bytes) == 16u, "bytes size");
RSI_META_ASSERT(sizeof(rsi_meta_message) == 32u, "message size");
RSI_META_ASSERT(sizeof(rsi_meta_injection) == 40u, "injection size");
RSI_META_ASSERT(sizeof(rsi_meta_empty_input) == 8u, "empty input size");
RSI_META_ASSERT(sizeof(rsi_meta_cap_input) == 40u, "cap input size");
RSI_META_ASSERT(sizeof(rsi_meta_open_input) == 72u, "open input size");
RSI_META_ASSERT(offsetof(rsi_meta_open_input, scope) == 8u, "open scope offset");
RSI_META_ASSERT(offsetof(rsi_meta_open_input, service) == 40u,
                "open service offset");
RSI_META_ASSERT(sizeof(rsi_meta_bytes_input) == 56u, "bytes input size");
RSI_META_ASSERT(sizeof(rsi_meta_release_output_input) == 32u,
                "release output input size");
RSI_META_ASSERT(sizeof(rsi_meta_message_input) == 72u, "message input size");
RSI_META_ASSERT(sizeof(rsi_meta_activate_input) == 96u, "activate input size");
RSI_META_ASSERT(sizeof(rsi_meta_serve_input) == 96u, "serve input size");
RSI_META_ASSERT(offsetof(rsi_meta_serve_input, provider) == 48u,
                "serve provider offset");
RSI_META_ASSERT(sizeof(rsi_meta_effect_defer_input) == 88u, "effect defer size");
RSI_META_ASSERT(sizeof(rsi_meta_provide_input) == 96u, "provide input size");
RSI_META_ASSERT(sizeof(rsi_meta_output_prefix) == 48u, "output prefix size");
RSI_META_ASSERT(sizeof(rsi_meta_basic_output) == 48u, "basic output size");
RSI_META_ASSERT(sizeof(rsi_meta_bytes_output) == 64u, "bytes output size");
RSI_META_ASSERT(sizeof(rsi_meta_bool_output) == 56u, "bool output size");
RSI_META_ASSERT(sizeof(rsi_meta_cap_output) == 80u, "owned cap output size");
RSI_META_ASSERT(sizeof(rsi_meta_borrowed_cap_output) == 80u,
                "borrowed cap output size");
RSI_META_ASSERT(sizeof(rsi_meta_message_output) == 88u, "message output size");
RSI_META_ASSERT(sizeof(rsi_meta_prepare_output) == 120u, "prepare output size");
RSI_META_ASSERT(offsetof(rsi_meta_prepare_output, retained_bytes) == 112u,
                "prepared retained bytes offset");
RSI_META_ASSERT(sizeof(rsi_meta_host_table) == 40u, "host table size");
RSI_META_ASSERT(sizeof(rsi_meta_plugin_table) == 72u, "plugin table size");
RSI_META_ASSERT(offsetof(rsi_meta_host_table, issuer) == 16u, "host issuer offset");
RSI_META_ASSERT(offsetof(rsi_meta_plugin_table, issuer) == 16u, "plugin issuer offset");
RSI_META_ASSERT(offsetof(rsi_meta_plugin_table, factory) >
                offsetof(rsi_meta_plugin_table, exchange), "factory follows exchange");

typedef uint32_t (*entry_fn)(const rsi_meta_host_table *, rsi_meta_plugin_table *,
                             uint32_t);
static entry_fn typed_entry = rsi_meta_plugin_entry_v2;
int use_entry_type(void) { return typed_entry == NULL; }
"#;
    for (extension, compiler_variable, fallback, standard) in
        [("c", "CC", "cc", "c11"), ("cc", "CXX", "c++", "c++17")]
    {
        let source = directory.path().join(format!("abi_layout.{extension}"));
        let object = directory.path().join(format!("abi_layout.{extension}.o"));
        fs::write(&source, assertions).unwrap();
        let compiler = std::env::var_os(compiler_variable).unwrap_or_else(|| fallback.into());
        let status = Command::new(compiler)
            .arg(format!("-std={standard}"))
            .args(["-Wall", "-Wextra", "-Werror", "-pedantic", "-c"])
            .arg(&source)
            .arg("-I")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/include"))
            .arg("-o")
            .arg(object)
            .status()
            .expect("invoke host C-family compiler");
        assert!(
            status.success(),
            "{standard} rejected the maintained header"
        );
    }
}
