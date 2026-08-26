use super::host::HostLease;
use super::transport::PluginTransport;
use crate::catalog::{CatalogInner, StagedArtifact};
use crate::catalog_resources::HostResourceLedger;
use crate::panic_containment::contain_result;
use crate::worker::{DestructionReservation, NativeExecutor};
use libloading::Library;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

pub(super) type TeardownJob = Box<dyn FnOnce() + Send + 'static>;

/// Complete foreign mapping bundle retained until one proven finalization.
pub(super) struct FinalResources {
    transport: Option<Arc<PluginTransport>>,
    host: Option<HostLease>,
    library: Option<Library>,
    artifact: Option<StagedArtifact>,
    catalog: Option<Arc<CatalogInner>>,
    host_resources: Option<Arc<HostResourceLedger>>,
    released: bool,
}

// SAFETY: PluginTransport admission and HostLease finalization govern the raw
// tables; Library is retained and only dropped on the destruction lane.
unsafe impl Send for FinalResources {}

impl FinalResources {
    pub(super) fn new(
        transport: Arc<PluginTransport>,
        host: HostLease,
        library: Library,
        artifact: Option<StagedArtifact>,
        catalog: Option<Arc<CatalogInner>>,
    ) -> Self {
        let host_resources = catalog
            .as_ref()
            .map(|catalog| Arc::clone(&catalog.host_resources));
        Self {
            transport: Some(transport),
            host: Some(host),
            library: Some(library),
            artifact,
            catalog,
            host_resources,
            released: false,
        }
    }

    /// Performs the sole ordered factory/finalization attempt for this bundle.
    pub(super) fn finalize(self) {
        let resources = self;
        let destroyed = resources.transport().destroy_factory().is_ok();
        let finalized = resources
            .host()
            .finalize_plugin(resources.transport())
            .is_ok();
        if destroyed && finalized && resources.can_release() {
            resources.release();
        }
        // Default Drop is deliberately non-unmapping. A FINALIZE refusal,
        // malformed successful output, or panic above pins the complete
        // table/mapping bundle and records the retained module.
    }

    fn transport(&self) -> &Arc<PluginTransport> {
        self.transport
            .as_ref()
            .expect("final resources retain transport")
    }

    fn host(&self) -> &HostLease {
        self.host
            .as_ref()
            .expect("final resources retain host table")
    }

    fn can_release(&self) -> bool {
        self.transport().is_finalized() && self.host().is_finalized()
    }

    /// Drops the mapping only after both raw tables are permanently closed.
    /// The library is intentionally last: if any earlier destructor unwinds,
    /// `Drop` leaks every still-owned field and keeps foreign code mapped.
    fn release(mut self) {
        assert!(
            self.can_release(),
            "unfinalized native mapping cannot be released"
        );
        drop(self.transport.take());
        drop(self.host.take());
        drop(self.artifact.take());
        drop(self.catalog.take());
        drop(self.host_resources.take());
        drop(self.library.take());
        self.released = true;
    }
}

impl Drop for FinalResources {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        if let Some(catalog) = &self.catalog {
            catalog.retain_failed_finalization();
        } else if let Some(resources) = &self.host_resources {
            resources.retain_failed_finalization();
        }
        // No individual field may run a destructor on the fail-closed path:
        // HostState can own foreign cleanup capabilities and Library unmapping
        // would invalidate every copied table function pointer.
        if let Some(value) = self.transport.take() {
            std::mem::forget(value);
        }
        if let Some(value) = self.host.take() {
            std::mem::forget(value);
        }
        if let Some(value) = self.library.take() {
            std::mem::forget(value);
        }
        if let Some(value) = self.artifact.take() {
            std::mem::forget(value);
        }
        if let Some(value) = self.catalog.take() {
            std::mem::forget(value);
        }
        if let Some(value) = self.host_resources.take() {
            std::mem::forget(value);
        }
    }
}

/// One FIFO lane for all foreign teardown owned by a mapped module.
pub(super) struct ModuleTeardownQueue {
    executor: NativeExecutor,
    reservation: DestructionReservation,
    state: Mutex<TeardownState>,
}

#[derive(Default)]
struct TeardownState {
    scheduled: bool,
    jobs: VecDeque<TeardownJob>,
}

impl ModuleTeardownQueue {
    pub(super) fn new(executor: NativeExecutor, reservation: DestructionReservation) -> Arc<Self> {
        Arc::new(Self {
            executor,
            reservation,
            state: Mutex::new(TeardownState::default()),
        })
    }

    pub(super) fn enqueue(self: &Arc<Self>, job: TeardownJob) {
        let schedule = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.jobs.push_back(job);
            if state.scheduled {
                false
            } else {
                state.scheduled = true;
                true
            }
        };
        if schedule {
            let queue = Arc::clone(self);
            self.executor.submit_reserved_destruction(
                self.reservation.clone(),
                move |_reservation| {
                    queue.drain();
                },
            );
        }
    }

    fn drain(self: Arc<Self>) {
        loop {
            let job = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(job) = state.jobs.pop_front() {
                    job
                } else {
                    state.scheduled = false;
                    return;
                }
            };
            let _ = contain_result(std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog_resources::NativeCatalogLimits;
    use crate::native::host::HostLease;
    use core::ffi::c_void;
    use rsi_meta_plugin::{
        ABI_MINOR, BasicOutput, CAP_KIND_FACTORY, CapId, PLUGIN_DESTROY_FACTORY, PLUGIN_FINALIZE,
        PluginTable, RIGHT_MUTATE, RIGHT_RETAIN, STATUS_OK, TableHeader,
    };
    use std::time::Duration;

    unsafe extern "C" fn malformed_finalize_exchange(
        _state: *mut c_void,
        opcode: u32,
        _input: *const c_void,
        _input_size: u32,
        output: *mut c_void,
        output_capacity: u32,
    ) -> u32 {
        if output.is_null()
            || output_capacity
                != u32::try_from(std::mem::size_of::<BasicOutput>()).expect("frame fits u32")
        {
            return rsi_meta_plugin::STATUS_PROTOCOL_ERROR;
        }
        let mut prefix = rsi_meta_plugin::OutputPrefix::empty(output_capacity);
        if opcode == PLUGIN_FINALIZE {
            prefix.struct_size = 0;
        } else if opcode != PLUGIN_DESTROY_FACTORY {
            return rsi_meta_plugin::STATUS_PROTOCOL_ERROR;
        }
        // SAFETY: The transport supplied an aligned writable BasicOutput with
        // the exact checked capacity for this test exchange.
        unsafe { output.cast::<BasicOutput>().write(BasicOutput { prefix }) };
        STATUS_OK
    }

    #[test]
    fn module_queue_retains_pending_instance_destruction_until_job_finishes() {
        let executor = NativeExecutor::new(1, 1, 1, 1).unwrap();
        let finalizer = executor.reserve_factory_destruction().unwrap();
        let queue = ModuleTeardownQueue::new(executor.clone(), finalizer);
        let (head_entered_sender, head_entered_receiver) = std::sync::mpsc::sync_channel(1);
        let (head_release_sender, head_release_receiver) = std::sync::mpsc::sync_channel(1);
        let (entered_sender, entered_receiver) = std::sync::mpsc::sync_channel(1);
        let (release_sender, release_receiver) = std::sync::mpsc::sync_channel(1);
        let (finished_sender, finished_receiver) = std::sync::mpsc::sync_channel(1);

        queue.enqueue(Box::new(move || {
            head_entered_sender.send(()).unwrap();
            head_release_receiver.recv().unwrap();
        }));
        head_entered_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("head teardown did not start");

        let pending = executor.begin_instance_destruction();
        queue.enqueue(Box::new(move || {
            let _pending = pending;
            entered_sender.send(()).unwrap();
            release_receiver.recv().unwrap();
        }));
        assert_eq!(executor.snapshot().pending_instance_destructions, 1);

        head_release_sender.send(()).unwrap();
        entered_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("instance destruction did not start");
        assert_eq!(executor.snapshot().pending_instance_destructions, 1);

        release_sender.send(()).unwrap();
        queue.enqueue(Box::new(move || {
            finished_sender.send(()).unwrap();
        }));
        finished_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("instance destruction did not finish");
        assert_eq!(executor.snapshot().pending_instance_destructions, 0);
    }

    #[test]
    fn final_resources_fail_closed_drop_records_and_pins_resources() {
        let resources = HostResourceLedger::new(&NativeCatalogLimits::default());
        let final_resources = FinalResources {
            transport: None,
            host: None,
            library: None,
            artifact: None,
            catalog: None,
            host_resources: Some(Arc::clone(&resources)),
            released: false,
        };
        assert_eq!(Arc::strong_count(&resources), 2);

        drop(final_resources);

        assert_eq!(resources.snapshot().retained_failed_finalizations, 1);
        assert_eq!(
            Arc::strong_count(&resources),
            2,
            "fail-closed drop must retain its resource bundle"
        );
    }

    #[test]
    fn malformed_successful_finalize_is_observable_and_pins_the_bundle() {
        let resources = HostResourceLedger::new(&NativeCatalogLimits::default());
        let host = HostLease::new(1, 1, Arc::clone(&resources)).unwrap();
        let mut plugin_state = Box::new(0_u8);
        let table = PluginTable {
            header: TableHeader::new(ABI_MINOR, PluginTable::STRUCT_SIZE),
            issuer: 77,
            state: (&raw mut *plugin_state).cast(),
            exchange: Some(malformed_finalize_exchange),
            factory: CapId {
                issuer: 77,
                slot: 1,
                epoch: 1,
                kind: CAP_KIND_FACTORY,
                rights: RIGHT_RETAIN | RIGHT_MUTATE,
            },
        };
        let final_resources = FinalResources {
            transport: Some(Arc::new(PluginTransport::new(table))),
            host: Some(host),
            library: None,
            artifact: None,
            catalog: None,
            host_resources: Some(Arc::clone(&resources)),
            released: false,
        };

        final_resources.finalize();

        assert_eq!(
            resources.snapshot().retained_failed_finalizations,
            1,
            "malformed success was indistinguishable from clean unload"
        );
    }
}
