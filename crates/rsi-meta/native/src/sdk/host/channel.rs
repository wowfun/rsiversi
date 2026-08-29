use super::{CallbackScope, Host, HostPort, Message, SdkError};
use crate::{CapId, STATUS_PROTOCOL_ERROR};
use std::marker::PhantomData;

/// Callback-local caller orientation returned by [`Host::open`].
pub struct CallChannel<'callback> {
    host: Host<'callback>,
    channel: CapId,
    requests_finished: bool,
    responses_eof: bool,
    terminal_observed: bool,
}

impl CallChannel<'_> {
    pub(super) fn new(port: HostPort, scope: CallbackScope, channel: CapId) -> Self {
        Self {
            host: Host {
                port,
                scope,
                authority: channel,
                lifetime: PhantomData,
            },
            channel,
            requests_finished: false,
            responses_eof: false,
            terminal_observed: false,
        }
    }

    pub fn host(&self) -> Host<'_> {
        self.host.clone()
    }

    pub fn receive(&mut self) -> Result<Option<Message>, SdkError> {
        self.host.scope.ensure_open()?;
        if self.responses_eof {
            return Err(protocol("caller response stream already reached EOF"));
        }
        let message = self.host.port.receive(self.channel)?;
        if message.is_none() {
            self.responses_eof = true;
        }
        Ok(message)
    }

    pub fn send(&mut self, message: &Message) -> Result<(), SdkError> {
        self.host.scope.ensure_open()?;
        if self.requests_finished || self.responses_eof {
            return Err(protocol("caller request stream is closed"));
        }
        self.host.port.send(self.channel, message)
    }

    pub fn finish_requests(&mut self) -> Result<(), SdkError> {
        self.host.scope.ensure_open()?;
        if self.requests_finished || self.responses_eof {
            return Err(protocol("caller request stream is already closed"));
        }
        self.host.port.finish(self.channel)?;
        self.requests_finished = true;
        Ok(())
    }

    pub fn terminal(&mut self) -> Result<(), SdkError> {
        self.host.scope.ensure_open()?;
        if !self.responses_eof {
            return Err(protocol("caller terminal outcome requested before EOF"));
        }
        if self.terminal_observed {
            return Err(protocol("caller terminal outcome already observed"));
        }
        self.terminal_observed = true;
        self.host.port.terminal(self.channel)
    }

    pub fn cancelled(&self) -> Result<bool, SdkError> {
        self.host.scope.ensure_open()?;
        self.host.port.cancelled(self.channel)
    }
}

impl std::fmt::Debug for CallChannel<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CallChannel")
            .field("requests_finished", &self.requests_finished)
            .field("responses_eof", &self.responses_eof)
            .field("terminal_observed", &self.terminal_observed)
            .finish_non_exhaustive()
    }
}

/// Callback-local provider orientation passed to [`crate::NativeInstance::serve`].
///
/// Provider code cannot finish caller requests or observe caller terminal state:
///
/// ```compile_fail,E0599
/// fn invalid(channel: &mut rsi_meta_native::ProviderChannel<'_>) {
///     channel.finish_requests().unwrap();
///     channel.terminal().unwrap();
/// }
/// ```
pub struct ProviderChannel<'callback> {
    host: Host<'callback>,
    channel: CapId,
    requests_eof: bool,
}

impl ProviderChannel<'_> {
    pub fn host(&self) -> Host<'_> {
        self.host.clone()
    }

    pub fn receive(&mut self) -> Result<Option<Message>, SdkError> {
        self.host.scope.ensure_open()?;
        if self.requests_eof {
            return Err(protocol("provider request stream already reached EOF"));
        }
        let message = self.host.port.receive(self.channel)?;
        if message.is_none() {
            self.requests_eof = true;
        }
        Ok(message)
    }

    pub fn send(&mut self, message: &Message) -> Result<(), SdkError> {
        self.host.scope.ensure_open()?;
        self.host.port.send(self.channel, message)
    }

    pub fn cancelled(&self) -> Result<bool, SdkError> {
        self.host.scope.ensure_open()?;
        self.host.port.cancelled(self.channel)
    }
}

impl std::fmt::Debug for ProviderChannel<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderChannel")
            .field("requests_eof", &self.requests_eof)
            .finish_non_exhaustive()
    }
}

pub(crate) fn provider_channel(
    port: HostPort,
    scope: &CallbackScope,
    channel: CapId,
) -> ProviderChannel<'_> {
    ProviderChannel {
        host: Host {
            port,
            scope: scope.clone(),
            authority: channel,
            lifetime: PhantomData,
        },
        channel,
        requests_eof: false,
    }
}

fn protocol(message: &'static str) -> SdkError {
    SdkError::new(STATUS_PROTOCOL_ERROR, message)
}
