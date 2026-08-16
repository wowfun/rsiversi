use core::ffi::c_void;
use std::fs;
use std::process::Command;

use rsi_meta_plugin::{
    ABI_LAYOUT_DESCRIPTOR, ABI_LAYOUT_SHA256, ABI_MAJOR, ABI_MINOR, C_HEADER, HostApi, PluginApi,
};
use sha2::{Digest, Sha256};

#[test]
fn fixed_layout_matches_the_c_header() {
    let pointer = core::mem::size_of::<*mut c_void>();

    assert_eq!(core::mem::offset_of!(HostApi, abi_major), 0);
    assert_eq!(core::mem::offset_of!(HostApi, abi_minor), 4);
    assert_eq!(core::mem::offset_of!(HostApi, struct_size), 8);
    assert_eq!(core::mem::offset_of!(HostApi, reserved), 12);
    assert_eq!(core::mem::offset_of!(HostApi, host_handle), 16);
    assert_eq!(
        core::mem::offset_of!(HostApi, host_post_frame),
        16 + pointer
    );
    assert_eq!(core::mem::size_of::<HostApi>(), 16 + 2 * pointer);

    assert_eq!(core::mem::offset_of!(PluginApi, abi_major), 0);
    assert_eq!(core::mem::offset_of!(PluginApi, abi_minor), 4);
    assert_eq!(core::mem::offset_of!(PluginApi, struct_size), 8);
    assert_eq!(core::mem::offset_of!(PluginApi, reserved), 12);
    assert_eq!(core::mem::offset_of!(PluginApi, plugin_handle), 16);
    assert_eq!(core::mem::offset_of!(PluginApi, on_frame), 16 + pointer);
    assert_eq!(core::mem::offset_of!(PluginApi, shutdown), 16 + 2 * pointer);
    assert_eq!(core::mem::offset_of!(PluginApi, destroy), 16 + 3 * pointer);
    assert_eq!(core::mem::size_of::<PluginApi>(), 16 + 4 * pointer);

    assert_eq!(
        HostApi::STRUCT_SIZE as usize,
        core::mem::size_of::<HostApi>()
    );
    assert_eq!(
        PluginApi::STRUCT_SIZE as usize,
        core::mem::size_of::<PluginApi>()
    );
    assert!(C_HEADER.contains(&format!("RSI_META_ABI_MAJOR UINT32_C({ABI_MAJOR})")));
    assert!(C_HEADER.contains(&format!("RSI_META_ABI_MINOR UINT32_C({ABI_MINOR})")));
    assert!(C_HEADER.contains(ABI_LAYOUT_SHA256));
    assert!(C_HEADER.contains("rsi_meta_plugin_entry_v0"));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn maintained_header_compiles_with_c11_layout_assertions() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("abi_layout.c");
    let object = directory.path().join("abi_layout.o");
    fs::write(
        &source,
        r#"
#include <stddef.h>
#include "rsi_meta_plugin.h"

_Static_assert(RSI_META_ABI_MAJOR == 0, "ABI major drift");
_Static_assert(RSI_META_ABI_MINOR == 0, "ABI minor drift");
_Static_assert(offsetof(rsi_meta_host_api, abi_major) == 0, "host major offset");
_Static_assert(offsetof(rsi_meta_host_api, host_handle) == 16, "host handle offset");
_Static_assert(sizeof(rsi_meta_host_api) == 16 + 2 * sizeof(void *), "host size");
_Static_assert(offsetof(rsi_meta_plugin_api, plugin_handle) == 16, "plugin handle offset");
_Static_assert(offsetof(rsi_meta_plugin_api, on_frame) == 16 + sizeof(void *), "on_frame offset");
_Static_assert(offsetof(rsi_meta_plugin_api, shutdown) == 16 + 2 * sizeof(void *), "shutdown offset");
_Static_assert(offsetof(rsi_meta_plugin_api, destroy) == 16 + 3 * sizeof(void *), "destroy offset");
_Static_assert(sizeof(rsi_meta_plugin_api) == 16 + 4 * sizeof(void *), "plugin size");

static rsi_meta_plugin_entry_v0_fn typed_entry = rsi_meta_plugin_entry_v0;

int use_entry_type(void) {
    return typed_entry != NULL;
}
"#,
    )
    .unwrap();

    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let status = Command::new(compiler)
        .arg("-std=c11")
        .arg("-Werror")
        .arg("-c")
        .arg(&source)
        .arg("-I")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/include"))
        .arg("-o")
        .arg(object)
        .status()
        .expect("invoke the platform C compiler");
    assert!(status.success(), "maintained C header failed to compile");
}

#[test]
fn abi_descriptor_hash_is_a_golden() {
    let actual = hex::encode(Sha256::digest(ABI_LAYOUT_DESCRIPTOR.as_bytes()));
    assert_eq!(actual, ABI_LAYOUT_SHA256);
}
