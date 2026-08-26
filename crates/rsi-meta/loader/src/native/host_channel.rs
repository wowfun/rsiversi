use super::host::CallbackFrame;
use rsi_meta::{CancellationObserver, CapabilityCall, Message, MetaError, ProviderChannel};
use std::sync::{Arc, Mutex};
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot};

pub(super) enum ProviderCommand {
    Receive(oneshot::Sender<Option<Message>>),
    Send(Message, oneshot::Sender<Result<(), MetaError>>),
    Cancelled(oneshot::Sender<bool>),
}

pub(super) struct ProviderBridge {
    frame: Arc<CallbackFrame>,
    commands: mpsc::Sender<ProviderCommand>,
    state: Mutex<ProviderState>,
    cancellation: CancellationObserver,
}

#[derive(Default)]
struct ProviderState {
    receiving: bool,
    eof: bool,
}

impl ProviderBridge {
    pub(super) fn new(
        frame: Arc<CallbackFrame>,
        cancellation: CancellationObserver,
    ) -> (Arc<Self>, mpsc::Receiver<ProviderCommand>) {
        let (commands, receiver) = mpsc::channel(8);
        (
            Arc::new(Self {
                frame,
                commands,
                state: Mutex::new(ProviderState::default()),
                cancellation,
            }),
            receiver,
        )
    }

    pub(super) fn frame(&self) -> &Arc<CallbackFrame> {
        &self.frame
    }

    pub(super) fn receive(&self, runtime: &Handle) -> Result<Option<Message>, ChannelError> {
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.receiving || state.eof {
                return Err(ChannelError::Protocol(
                    "provider receive is already in flight or at EOF".to_owned(),
                ));
            }
            state.receiving = true;
        }
        let (sender, receiver) = oneshot::channel();
        let result = runtime.block_on(async {
            self.commands
                .send(ProviderCommand::Receive(sender))
                .await
                .map_err(|_| ChannelError::Stale)?;
            receiver.await.map_err(|_| ChannelError::Stale)
        });
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.receiving = false;
        if matches!(result, Ok(None)) {
            state.eof = true;
        }
        result
    }

    pub(super) fn send(&self, runtime: &Handle, message: Message) -> Result<(), ChannelError> {
        let (sender, receiver) = oneshot::channel();
        runtime.block_on(async {
            self.commands
                .send(ProviderCommand::Send(message, sender))
                .await
                .map_err(|_| ChannelError::Stale)?;
            receiver
                .await
                .map_err(|_| ChannelError::Stale)?
                .map_err(ChannelError::Core)
        })
    }

    pub(super) fn cancelled(&self, runtime: &Handle) -> Result<bool, ChannelError> {
        if self.cancellation.is_cancelled() {
            return Ok(true);
        }
        let (sender, receiver) = oneshot::channel();
        runtime.block_on(async {
            self.commands
                .send(ProviderCommand::Cancelled(sender))
                .await
                .map_err(|_| ChannelError::Stale)?;
            receiver.await.map_err(|_| ChannelError::Stale)
        })
    }
}

pub(super) async fn pump_provider(
    channel: &mut ProviderChannel<'_>,
    commands: &mut mpsc::Receiver<ProviderCommand>,
    mut callback: oneshot::Receiver<Result<(), crate::LoaderError>>,
) -> Result<(), MetaError> {
    loop {
        tokio::select! {
            biased;
            result = &mut callback => {
                return result
                    .map_err(|error| MetaError::Service(format!("native callback worker disconnected: {error}")))?
                    .map_err(loader_service_error);
            }
            command = commands.recv() => match command {
                Some(ProviderCommand::Receive(reply)) => {
                    let _ = reply.send(channel.recv().await);
                }
                Some(ProviderCommand::Send(message, reply)) => {
                    let _ = reply.send(channel.send(message).await);
                }
                Some(ProviderCommand::Cancelled(reply)) => {
                    let _ = reply.send(channel.cancellation().is_cancelled());
                }
                None => {
                    return callback.await
                        .map_err(|error| MetaError::Service(format!("native callback worker disconnected: {error}")))?
                        .map_err(loader_service_error);
                }
            }
        }
    }
}

pub(super) struct CallerChannel {
    frame: Arc<CallbackFrame>,
    runtime: Handle,
    cancellation: CancellationObserver,
    state: Mutex<CallerState>,
}

#[allow(clippy::struct_excessive_bools)] // Independent wire facts cannot be collapsed without losing exact diagnostics.
struct CallerState {
    call: Option<CapabilityCall>,
    in_flight: bool,
    requests_finished: bool,
    recv_eof: bool,
    terminal: Option<Terminal>,
    terminal_observed: bool,
}

#[derive(Clone)]
pub(super) struct Terminal {
    pub(super) status: u32,
    pub(super) diagnostic: String,
}

impl CallerChannel {
    pub(super) fn new(
        frame: Arc<CallbackFrame>,
        runtime: Handle,
        call: CapabilityCall,
    ) -> Arc<Self> {
        let cancellation = call.cancellation_observer();
        Arc::new(Self {
            frame,
            runtime,
            cancellation,
            state: Mutex::new(CallerState {
                call: Some(call),
                in_flight: false,
                requests_finished: false,
                recv_eof: false,
                terminal: None,
                terminal_observed: false,
            }),
        })
    }

    pub(super) fn frame(&self) -> &Arc<CallbackFrame> {
        &self.frame
    }

    pub(super) fn send(&self, message: Message) -> Result<(), ChannelError> {
        let call = self.take_call()?;
        let result = self.runtime.block_on(call.send(message));
        self.restore_call(call);
        result.map_err(ChannelError::Core)
    }

    pub(super) fn finish(&self) -> Result<(), ChannelError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.in_flight || state.requests_finished {
            return Err(ChannelError::Protocol(
                "caller request stream is busy or already finished".to_owned(),
            ));
        }
        let call = state.call.as_mut().ok_or(ChannelError::Stale)?;
        call.finish();
        state.requests_finished = true;
        Ok(())
    }

    pub(super) fn receive(&self) -> Result<Option<Message>, ChannelError> {
        {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.recv_eof {
                return Err(ChannelError::Protocol(
                    "caller receive repeated after EOF".to_owned(),
                ));
            }
        }
        let mut call = self.take_call()?;
        let result = self.runtime.block_on(call.recv());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.in_flight = false;
        state.call = Some(call);
        match result {
            Ok(Some(message)) => Ok(Some(message)),
            Ok(None) => {
                state.recv_eof = true;
                state.terminal = Some(Terminal {
                    status: rsi_meta_plugin::STATUS_OK,
                    diagnostic: String::new(),
                });
                Ok(None)
            }
            Err(error) => {
                state.recv_eof = true;
                state.terminal = Some(terminal_from_error(&error));
                Ok(None)
            }
        }
    }

    pub(super) fn terminal(&self) -> Result<Terminal, ChannelError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.recv_eof || state.terminal_observed {
            return Err(ChannelError::Protocol(
                "caller terminal is unavailable or already observed".to_owned(),
            ));
        }
        state.terminal_observed = true;
        state
            .terminal
            .clone()
            .ok_or_else(|| ChannelError::Protocol("caller EOF has no cached terminal".to_owned()))
    }

    pub(super) fn cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    fn take_call(&self) -> Result<CapabilityCall, ChannelError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.in_flight {
            return Err(ChannelError::Busy);
        }
        let call = state.call.take().ok_or(ChannelError::Stale)?;
        state.in_flight = true;
        Ok(call)
    }

    fn restore_call(&self, call: CapabilityCall) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        debug_assert!(state.in_flight && state.call.is_none());
        state.call = Some(call);
        state.in_flight = false;
    }
}

pub(super) enum ChannelError {
    Stale,
    Busy,
    Protocol(String),
    Core(MetaError),
}

fn terminal_from_error(error: &MetaError) -> Terminal {
    let status = match error {
        MetaError::Cancelled => rsi_meta_plugin::STATUS_CANCELLED,
        MetaError::Timeout(_) => rsi_meta_plugin::STATUS_TERMINAL,
        MetaError::Busy { .. } => rsi_meta_plugin::STATUS_BUSY,
        MetaError::Reentrant { .. } => rsi_meta_plugin::STATUS_REENTRANT,
        MetaError::StaleCapability
        | MetaError::StaleService { .. }
        | MetaError::StaleContext { .. }
        | MetaError::FiberDisposed { .. } => rsi_meta_plugin::STATUS_STALE_CAPABILITY,
        _ => rsi_meta_plugin::STATUS_FAILED,
    };
    Terminal {
        status,
        diagnostic: error.to_string(),
    }
}

fn loader_service_error(error: crate::LoaderError) -> MetaError {
    match error {
        crate::LoaderError::Timeout(operation) => MetaError::Timeout(operation),
        crate::LoaderError::Busy { operation } => MetaError::Busy { operation },
        crate::LoaderError::Reentrant { operation } => MetaError::Reentrant { operation },
        error => MetaError::Service(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NativeCatalogLimits;
    use crate::catalog_resources::HostResourceLedger;
    use crate::native::host::HostLease;
    use async_trait::async_trait;
    use rsi_meta::{
        ActivationPlan, CancellationObserver, Capability, ConfigValue, ContractVersion,
        DeadlineLimits, FactoryIdentity, InvocationContext, PluginFactory, PreparedActivation,
        Requirement, Result, Runtime, RuntimeLimits, ServiceEndpoint,
    };
    use std::future::{Future as _, poll_fn};
    use std::time::Duration;

    const SERVICE: &str = "caller-cancellation-probe";
    const CONTRACT: &str = "test.caller-cancellation-probe";
    const V1: ContractVersion = ContractVersion(1);

    #[derive(Debug)]
    struct WaitingEndpoint {
        cancellation: Mutex<Option<oneshot::Sender<CancellationObserver>>>,
    }

    #[async_trait]
    impl ServiceEndpoint for WaitingEndpoint {
        async fn serve(
            &self,
            _invocation: InvocationContext,
            channel: ProviderChannel<'_>,
        ) -> Result<()> {
            let cancellation = channel.cancellation();
            if let Some(sender) = self
                .cancellation
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                let _ = sender.send(cancellation.clone());
            }
            cancellation.cancelled().await;
            Ok(())
        }
    }

    #[derive(Debug)]
    struct ProviderFactory {
        cancellation: Mutex<Option<oneshot::Sender<CancellationObserver>>>,
    }

    #[async_trait]
    impl PluginFactory for ProviderFactory {
        fn identity(&self) -> FactoryIdentity {
            FactoryIdentity::builtin("caller-cancellation-provider", "1")
        }

        fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
            Ok(PreparedActivation::new(desired.clone()))
        }

        async fn activate(&self, plan: ActivationPlan) -> Result<()> {
            let cancellation = self
                .cancellation
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            plan.context().provide(
                SERVICE,
                CONTRACT,
                V1,
                Arc::new(WaitingEndpoint {
                    cancellation: Mutex::new(cancellation),
                }),
            )?;
            Ok(())
        }
    }

    #[derive(Debug)]
    struct ConsumerFactory {
        capability: Arc<Mutex<Option<Capability>>>,
    }

    #[async_trait]
    impl PluginFactory for ConsumerFactory {
        fn identity(&self) -> FactoryIdentity {
            FactoryIdentity::builtin("caller-cancellation-consumer", "1")
        }

        fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
            Ok(PreparedActivation::new(desired.clone())
                .requiring(Requirement::new(SERVICE, CONTRACT, V1)))
        }

        async fn activate(&self, plan: ActivationPlan) -> Result<()> {
            *self
                .capability
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = plan.inject(SERVICE).cloned();
            Ok(())
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancellation_is_live_while_receive_owns_the_caller_half() {
        let runtime = Runtime::new(RuntimeLimits {
            deadlines: DeadlineLimits {
                service_call: Duration::from_millis(100),
                ..DeadlineLimits::default()
            },
            ..RuntimeLimits::default()
        })
        .unwrap();
        let (cancellation_sender, cancellation_receiver) = oneshot::channel();
        let provider = runtime
            .root()
            .apply(
                Arc::new(ProviderFactory {
                    cancellation: Mutex::new(Some(cancellation_sender)),
                }),
                ConfigValue::Null,
            )
            .await
            .expect("provider activates");
        let captured = Arc::new(Mutex::new(None));
        let consumer = runtime
            .root()
            .apply(
                Arc::new(ConsumerFactory {
                    capability: Arc::clone(&captured),
                }),
                ConfigValue::Null,
            )
            .await
            .expect("consumer activates");
        let capability = captured
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .expect("consumer receives capability");
        let call = capability.open().expect("caller opens service");
        drop(capability);

        let limits = NativeCatalogLimits::default();
        let host = HostLease::new(
            limits.maximum_host_capabilities,
            limits.maximum_host_outputs,
            HostResourceLedger::new(&limits),
        )
        .expect("host table initializes");
        let frame = host.state().callback_frame(Handle::current());
        let caller = CallerChannel::new(Arc::clone(&frame), Handle::current(), call);
        let Ok(mut call) = caller.take_call() else {
            panic!("caller half is available");
        };
        let (pending_sender, pending_receiver) = oneshot::channel();
        let receive = tokio::spawn(async move {
            let result = {
                let mut receive = Box::pin(call.recv());
                let mut pending_sender = Some(pending_sender);
                poll_fn(|context| {
                    let poll = receive.as_mut().poll(context);
                    if poll.is_pending()
                        && let Some(sender) = pending_sender.take()
                    {
                        let _ = sender.send(());
                    }
                    poll
                })
                .await
            };
            (call, result)
        });
        pending_receiver
            .await
            .expect("caller receive reaches a pending poll");
        assert!(matches!(caller.take_call(), Err(ChannelError::Busy)));

        let cancellation = cancellation_receiver
            .await
            .expect("provider exposes the exact call cancellation");
        tokio::time::timeout(Duration::from_secs(1), cancellation.cancelled())
            .await
            .expect("the call deadline cancels while its caller half is elsewhere");
        assert!(caller.cancelled());
        let (call, terminal) = receive.await.expect("receive task joins");
        let observed_terminal = matches!(
            terminal,
            Ok(None) | Err(MetaError::Cancelled | MetaError::Timeout("service call"))
        );
        caller.restore_call(call);

        drop(caller);
        frame.seal();
        drop(frame);
        host.retire_without_plugin();
        drop(host);
        assert!(consumer.dispose().await.is_clean());
        assert!(provider.dispose().await.is_clean());
        assert!(runtime.shutdown().await.is_clean());
        assert!(observed_terminal);
    }
}
