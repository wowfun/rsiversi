use crate::sdk::host::Cleanup;
use crate::{NativeInstance, NativePlugin, ServiceRequirement};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub(super) struct FactoryCell<P> {
    pub(super) plugin: P,
    pub(super) gate: AtomicBool,
}

pub(super) struct PreparedCell<P: NativePlugin> {
    pub(super) factory: Arc<FactoryCell<P>>,
    pub(super) state: Mutex<Option<P::Prepared>>,
    pub(super) requirements: Vec<ServiceRequirement>,
}

pub(super) struct InstanceCell<I: NativeInstance> {
    pub(super) instance: Mutex<Option<I>>,
    pub(super) requirements: Vec<ServiceRequirement>,
    pub(super) owner_lineage: AtomicU64,
    pub(super) closing: AtomicBool,
    lifecycle: AtomicU8,
}

pub(super) struct CleanupCell {
    action: Mutex<Option<Cleanup>>,
    state: AtomicU8,
}

impl CleanupCell {
    const PENDING: u8 = 0;
    const RUNNING: u8 = 1;
    const FINISHED: u8 = 2;

    pub(super) fn new(action: Cleanup) -> Self {
        Self {
            action: Mutex::new(Some(action)),
            state: AtomicU8::new(Self::PENDING),
        }
    }

    pub(super) fn begin(&self) -> Option<CleanupRun<'_>> {
        self.state
            .compare_exchange(
                Self::PENDING,
                Self::RUNNING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .ok()
            .map(|_| CleanupRun { cell: self })
    }

    pub(super) fn take_action(&self) -> Option<Cleanup> {
        self.action
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    pub(super) fn is_finished(&self) -> bool {
        self.state.load(Ordering::Acquire) == Self::FINISHED
    }
}

pub(super) struct CleanupRun<'a> {
    cell: &'a CleanupCell,
}

impl Drop for CleanupRun<'_> {
    fn drop(&mut self) {
        self.cell
            .state
            .store(CleanupCell::FINISHED, Ordering::Release);
    }
}

impl<I: NativeInstance> InstanceCell<I> {
    const CREATED: u8 = 0;
    const ACTIVATING: u8 = 1;
    const ACTIVE: u8 = 2;
    const TERMINAL: u8 = 3;

    pub(super) fn new(instance: I, requirements: Vec<ServiceRequirement>) -> Self {
        Self {
            instance: Mutex::new(Some(instance)),
            requirements,
            owner_lineage: AtomicU64::new(0),
            closing: AtomicBool::new(false),
            lifecycle: AtomicU8::new(Self::CREATED),
        }
    }

    pub(super) fn begin_activation(&self) -> bool {
        self.lifecycle
            .compare_exchange(
                Self::CREATED,
                Self::ACTIVATING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(super) fn mark_active(&self) {
        self.lifecycle.store(Self::ACTIVE, Ordering::Release);
    }

    pub(super) fn mark_terminal(&self) {
        self.lifecycle.store(Self::TERMINAL, Ordering::Release);
    }

    pub(super) fn is_active(&self) -> bool {
        self.lifecycle.load(Ordering::Acquire) == Self::ACTIVE
    }
}

pub(super) enum CapValue<P: NativePlugin> {
    Factory(Arc<FactoryCell<P>>),
    Prepared(Arc<PreparedCell<P>>),
    Instance(Arc<InstanceCell<P::Instance>>),
    Cleanup(Arc<CleanupCell>),
}

impl<P: NativePlugin> Clone for CapValue<P> {
    fn clone(&self) -> Self {
        match self {
            Self::Factory(value) => Self::Factory(Arc::clone(value)),
            Self::Prepared(value) => Self::Prepared(Arc::clone(value)),
            Self::Instance(value) => Self::Instance(Arc::clone(value)),
            Self::Cleanup(value) => Self::Cleanup(Arc::clone(value)),
        }
    }
}
