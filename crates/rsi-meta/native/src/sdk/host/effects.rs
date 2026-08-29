use super::port::frame;
use super::{CallbackScope, Capability, HostPort, SdkError};
use crate::{
    CAP_KIND_SERVICE, CapId, EffectDeferInput, HOST_EFFECT_ABORT, HOST_EFFECT_COMMIT,
    HOST_EFFECT_DEFER, HOST_PROVIDE, ProvideInput, RIGHT_OPEN, RawBytes, STATUS_PROTOCOL_ERROR,
};
use std::marker::PhantomData;

pub(crate) type Cleanup = Box<dyn FnOnce() -> Result<(), String> + Send + 'static>;

pub(crate) trait CleanupRegistry {
    fn insert_cleanup(&self, cleanup: Cleanup) -> Result<CapId, u32>;
    fn discard_cleanup(&self, capability: CapId);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Open,
    CommitRequested,
    Committed,
    Aborted,
}

/// Callback-local activation effect transaction.
pub struct EffectTxn<'callback> {
    port: HostPort,
    scope: CallbackScope,
    transaction: CapId,
    registry: &'callback dyn CleanupRegistry,
    state: State,
    lifetime: PhantomData<&'callback mut ()>,
}

impl<'callback> EffectTxn<'callback> {
    pub(super) fn new(
        port: HostPort,
        scope: &'callback CallbackScope,
        transaction: CapId,
        registry: &'callback dyn CleanupRegistry,
    ) -> Self {
        Self {
            port,
            scope: scope.clone(),
            transaction,
            registry,
            state: State::Open,
            lifetime: PhantomData,
        }
    }

    pub fn defer(
        &mut self,
        label: &str,
        cleanup: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) -> Result<(), SdkError> {
        self.ensure_open()?;
        let cleanup = self
            .registry
            .insert_cleanup(Box::new(cleanup))
            .map_err(|status| SdkError::new(status, "plugin cleanup table rejected effect"))?;
        let input = EffectDeferInput {
            header: frame::<EffectDeferInput>(),
            transaction: self.transaction,
            cleanup,
            label: raw_bytes(label.as_bytes()),
        };
        if let Err(error) = self.port.call_basic(HOST_EFFECT_DEFER, &input) {
            self.registry.discard_cleanup(cleanup);
            return Err(error);
        }
        Ok(())
    }

    pub fn provide(
        &mut self,
        key: &str,
        contract: &str,
        version: u64,
        port: &[u8],
    ) -> Result<Capability, SdkError> {
        self.ensure_open()?;
        let input = ProvideInput {
            header: frame::<ProvideInput>(),
            transaction: self.transaction,
            port: raw_bytes(port),
            key: raw_bytes(key.as_bytes()),
            contract: raw_bytes(contract.as_bytes()),
            version,
        };
        let capability = self
            .port
            .owned_cap(HOST_PROVIDE, &input, CAP_KIND_SERVICE, RIGHT_OPEN)?;
        Ok(Capability::new(self.port, capability))
    }

    /// Requests commit and closes further transaction mutation.
    ///
    /// The adapter asks the host to accept the native subprotocol state only
    /// after the activation callback returns success. The outer Runtime remains
    /// responsible for committing or rolling back its activation root.
    /// Returning an error, panicking, or dropping the activation first aborts
    /// this requested transaction.
    pub fn commit(&mut self) -> Result<(), SdkError> {
        self.ensure_open()?;
        self.state = State::CommitRequested;
        Ok(())
    }

    pub(super) fn finish_commit(&mut self) -> Result<(), SdkError> {
        self.scope.ensure_open()?;
        if self.state != State::CommitRequested {
            return Err(SdkError::new(
                STATUS_PROTOCOL_ERROR,
                "effect transaction has no commit request",
            ));
        }
        self.port.call_basic(
            HOST_EFFECT_COMMIT,
            &crate::CapInput {
                header: frame::<crate::CapInput>(),
                capability: self.transaction,
            },
        )?;
        self.state = State::Committed;
        Ok(())
    }

    pub fn abort(&mut self) -> Result<(), SdkError> {
        self.scope.ensure_open()?;
        if !matches!(self.state, State::Open | State::CommitRequested) {
            return Err(SdkError::new(
                STATUS_PROTOCOL_ERROR,
                "effect transaction is closed",
            ));
        }
        self.abort_open()
    }

    fn abort_open(&mut self) -> Result<(), SdkError> {
        let result = self.port.call_basic(
            HOST_EFFECT_ABORT,
            &crate::CapInput {
                header: frame::<crate::CapInput>(),
                capability: self.transaction,
            },
        );
        self.state = State::Aborted;
        result
    }

    fn ensure_open(&self) -> Result<(), SdkError> {
        self.scope.ensure_open()?;
        if self.state == State::Open {
            Ok(())
        } else {
            Err(SdkError::new(
                STATUS_PROTOCOL_ERROR,
                "effect transaction is closed",
            ))
        }
    }

    pub(super) const fn committed(&self) -> bool {
        matches!(self.state, State::Committed)
    }

    pub(super) const fn commit_requested(&self) -> bool {
        matches!(self.state, State::CommitRequested)
    }
}

impl Drop for EffectTxn<'_> {
    fn drop(&mut self) {
        if matches!(self.state, State::Open | State::CommitRequested) {
            let _ = self.abort_open();
        }
    }
}

impl std::fmt::Debug for EffectTxn<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EffectTxn")
            .field("state", &self.state)
            .finish()
    }
}

fn raw_bytes(bytes: &[u8]) -> RawBytes {
    RawBytes {
        ptr: if bytes.is_empty() {
            core::ptr::null()
        } else {
            bytes.as_ptr()
        },
        len: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    }
}
