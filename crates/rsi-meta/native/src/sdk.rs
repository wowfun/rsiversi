//! Safe Rust authoring interface for ABI v3 plugins.

mod author;
mod host;
mod runtime;

pub use author::{NativeInstance, NativePlugin, Prepared, ServiceRequirement};
pub use host::{
    Activation, CallChannel, Capability, EffectTxn, Host, Message, ProviderChannel, SdkError,
};
pub use runtime::plugin_entry;

/// Exports one ABI v3 plugin entry backed by the safe SDK.
#[macro_export]
macro_rules! export_plugin {
    ($plugin:ty) => {
        #[unsafe(no_mangle)]
        #[doc = "# Safety"]
        #[doc = ""]
        #[doc = "The host and output pointers must satisfy the ABI v3 entry contract in `rsi_meta_plugin.h`."]
        pub unsafe extern "C" fn rsi_meta_plugin_entry_v3(
            host: *const $crate::HostTable,
            plugin_out: *mut $crate::PluginTable,
            output_capacity: u32,
        ) -> u32 {
            // SAFETY: This trampoline forwards the exact raw entry contract.
            unsafe { $crate::plugin_entry::<$plugin>(host, plugin_out, output_capacity) }
        }
    };
}
