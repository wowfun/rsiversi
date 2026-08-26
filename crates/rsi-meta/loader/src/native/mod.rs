mod admission;
mod callback_gate;
mod cap_table;
mod host;
mod host_channel;
mod lifecycle;
mod module_teardown;
mod output_table;
mod slot_allocator;
mod transport;

pub(crate) use lifecycle::ModuleControl as NativeModule;
pub use lifecycle::NativeFactory;
