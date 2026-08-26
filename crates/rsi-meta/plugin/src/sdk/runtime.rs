use super::NativePlugin;
use super::host::HostPort;
use super::host::{Cleanup, CleanupRegistry};
use crate::{
    CAP_KIND_FACTORY, CAP_KIND_INSTANCE, RIGHT_MUTATE, RIGHT_RETAIN, STATUS_PANICKED,
    STATUS_WRONG_CAPABILITY,
};
use std::sync::{Mutex, MutexGuard};

mod callbacks;
mod caps;
mod entry;
mod exchanges;
mod gates;
mod operations;
mod output_records;
mod outputs;
mod panics;
mod values;
mod wire_io;

use callbacks::{CallbackAdmission, CallbackGate};
use caps::CapTable;
use exchanges::ExchangeGate;
use gates::InstanceGate;
use outputs::OutputTable;
use values::{CapValue, CleanupCell, FactoryCell};

pub use entry::plugin_entry;

pub(super) struct PluginRuntime<P: NativePlugin> {
    host: HostPort,
    caps: Mutex<CapTable<P>>,
    outputs: Mutex<OutputTable>,
    exchanges: ExchangeGate,
    callback_refs: CallbackGate,
}

impl<P: NativePlugin> PluginRuntime<P> {
    fn new(host: HostPort, plugin: P, issuer: u64) -> Result<(Self, crate::CapId), u32> {
        let mut caps = CapTable::new(issuer);
        let factory = match caps.insert(
            CAP_KIND_FACTORY,
            RIGHT_RETAIN | RIGHT_MUTATE,
            CapValue::Factory(std::sync::Arc::new(FactoryCell {
                plugin,
                gate: std::sync::atomic::AtomicBool::new(false),
            })),
        ) {
            Ok(factory) => factory,
            Err((status, value)) => {
                return Err(drop_status(value).unwrap_or(status));
            }
        };
        Ok((
            Self {
                host,
                caps: Mutex::new(caps),
                outputs: Mutex::new(OutputTable::new(issuer)),
                exchanges: ExchangeGate::default(),
                callback_refs: CallbackGate::default(),
            },
            factory,
        ))
    }

    fn caps(&self) -> MutexGuard<'_, CapTable<P>> {
        lock(&self.caps)
    }

    fn outputs(&self) -> MutexGuard<'_, OutputTable> {
        lock(&self.outputs)
    }

    pub(super) const fn host(&self) -> HostPort {
        self.host
    }

    pub(in crate::sdk::runtime) fn callback(&self) -> Result<CallbackAdmission<'_>, u32> {
        self.callback_refs.enter()
    }

    pub(in crate::sdk::runtime) fn insert_cap(
        &self,
        kind: u32,
        rights: u32,
        value: CapValue<P>,
    ) -> Result<crate::CapId, u32> {
        let result = self.caps().insert(kind, rights, value);
        match result {
            Ok(capability) => Ok(capability),
            Err((status, rejected)) => Err(drop_status(rejected).unwrap_or(status)),
        }
    }

    pub(super) fn release_external_cap(&self, capability: crate::CapId) -> Result<(), u32> {
        let retired = {
            let mut caps = self.caps();
            caps.release_external(capability)?
        };
        drop_status(retired).map_or(Ok(()), Err)
    }

    pub(super) fn release_owned_cap(&self, capability: crate::CapId) -> Result<(), u32> {
        let retired = { self.caps().release_owned(capability) };
        drop_status(retired).map_or(Ok(()), Err)
    }

    pub(super) fn destroy_cap(&self, capability: crate::CapId, kind: u32) -> Result<(), u32> {
        let retired = {
            let mut caps = self.caps();
            caps.destroy(capability, kind)?
        };
        drop_status(retired).map_or(Ok(()), Err)
    }

    pub(super) fn destroy_instance_cap(&self, capability: crate::CapId) -> Result<(), u32> {
        let instance = {
            let caps = self.caps();
            match caps.get(capability, CAP_KIND_INSTANCE, RIGHT_MUTATE)? {
                CapValue::Instance(instance) => instance,
                _ => return Err(STATUS_WRONG_CAPABILITY),
            }
        };
        InstanceGate::begin_destruction(&instance)?;
        instance.mark_terminal();
        self.destroy_cap(capability, CAP_KIND_INSTANCE)
    }
}

impl<P: NativePlugin> CleanupRegistry for PluginRuntime<P> {
    fn insert_cleanup(&self, cleanup: Cleanup) -> Result<crate::CapId, u32> {
        self.insert_cap(
            crate::CAP_KIND_CLEANUP,
            RIGHT_MUTATE,
            CapValue::Cleanup(std::sync::Arc::new(CleanupCell::new(cleanup))),
        )
    }

    fn discard_cleanup(&self, capability: crate::CapId) {
        let _ = self.release_owned_cap(capability);
    }
}

fn drop_status<T>(value: T) -> Option<u32> {
    panics::drop_contained(value).err().map(|_| STATUS_PANICKED)
}

pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
