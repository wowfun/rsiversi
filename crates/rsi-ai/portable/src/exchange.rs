use crate::{WIRE_BUDGET, error};
use rsi_ai_protocol::{
    AiError, DispatchStatus, ErrorKind, ErrorPhase, MediaDescriptor,
    portable::{self, ControlRequest, ControlResponse, Decoder, Kind, Packet},
};
use rsi_ai_provider::{AbortSignal, PrepareContext};
use rsi_meta::{Capability, CapabilityCall, Message, MetaError};

pub(crate) struct Exchange {
    call: CapabilityCall,
    abort: AbortSignal,
    decoder: Option<Decoder>,
    phase: ErrorPhase,
    dispatch: DispatchStatus,
    dependencies: usize,
    dependency_bytes: u64,
}
impl Exchange {
    pub(crate) fn open(
        capability: &Capability,
        abort: AbortSignal,
        preparing: bool,
    ) -> Result<Self, AiError> {
        let phase = if preparing {
            ErrorPhase::Prepare
        } else {
            ErrorPhase::Send
        };
        if abort.is_aborted() {
            return Err(error(
                ErrorKind::Cancelled,
                phase,
                DispatchStatus::NotDispatched,
            ));
        }
        let call = capability
            .open()
            .map_err(|_| error(ErrorKind::Transport, phase, DispatchStatus::NotDispatched))?;
        let decoder = Decoder::with_control_limit(
            WIRE_BUDGET.clone(),
            if preparing {
                portable::MAXIMUM_METADATA_BYTES
            } else {
                portable::MAXIMUM_CONTROL_BYTES
            },
        )
        .expect("static phase ceiling");
        Ok(Self {
            call,
            abort,
            decoder: Some(decoder),
            phase,
            dispatch: DispatchStatus::NotDispatched,
            dependencies: 0,
            dependency_bytes: 0,
        })
    }
    pub(crate) fn invalid(&self) -> AiError {
        error(ErrorKind::Protocol, self.phase, self.dispatch)
    }
    fn call_error(&self, error_value: &MetaError) -> AiError {
        let kind = match error_value {
            MetaError::Cancelled => ErrorKind::Cancelled,
            MetaError::Timeout(_) => ErrorKind::Timeout,
            _ => ErrorKind::Transport,
        };
        error(kind, self.phase, self.dispatch)
    }
    pub(crate) async fn send_request(
        &mut self,
        request: &ControlRequest,
        start: bool,
    ) -> Result<(), AiError> {
        let body = WIRE_BUDGET
            .encode(request, portable::MAXIMUM_CONTROL_BYTES)
            .map_err(|_| self.invalid())?;
        if start {
            // Once any Start fragment can leave this process, malformed exchange or
            // cancellation cannot establish absence of external provider effects.
            self.dispatch = DispatchStatus::Unknown;
            self.phase = ErrorPhase::Stream;
        }
        self.send_bytes(Kind::Json, body.as_bytes()).await
    }
    async fn send_bytes(&self, kind: Kind, bytes: &[u8]) -> Result<(), AiError> {
        for frame in portable::frames(kind, bytes).map_err(|_| self.invalid())? {
            tokio::select! {
                biased;
                () = self.abort.cancelled() => return Err(error(ErrorKind::Cancelled, self.phase, self.dispatch)),
                result = self.call.send(Message::new(frame)) => result.map_err(|e| self.call_error(&e))?,
            }
        }
        Ok(())
    }
    pub(crate) fn finish_request(&mut self) {
        self.call.finish();
    }
    pub(crate) async fn packet(&mut self) -> Result<Option<Packet>, AiError> {
        loop {
            let message = tokio::select! {
                biased;
                () = self.abort.cancelled() => return Err(error(ErrorKind::Cancelled, self.phase, self.dispatch)),
                result = self.call.recv() => result.map_err(|e| self.call_error(&e))?,
            };
            let Some(message) = message else {
                if let Some(decoder) = self.decoder.take() {
                    decoder.finish().map_err(|_| self.invalid())?;
                }
                return Ok(None);
            };
            if !message.capabilities().is_empty() {
                return Err(self.invalid());
            }
            let Some(decoder) = self.decoder.as_mut() else {
                return Err(self.invalid());
            };
            if let Some(packet) = decoder
                .push(message.as_bytes())
                .map_err(|_| self.invalid())?
            {
                return Ok(Some(packet));
            }
        }
    }
    pub(crate) async fn control(&mut self) -> Result<ControlResponse, AiError> {
        let packet = self.packet().await?.ok_or_else(|| self.invalid())?;
        if packet.kind != Kind::Json {
            return Err(self.invalid());
        }
        portable::decode_control(packet.bytes.as_bytes()).map_err(|_| self.invalid())
    }
    pub(crate) async fn terminal(&mut self) -> Result<(), AiError> {
        self.call.finish();
        if self.packet().await?.is_some() {
            return Err(self.invalid());
        }
        Ok(())
    }
    pub(crate) async fn cancel_and_drain(&mut self) {
        self.call.cancel();
        while matches!(self.call.recv().await, Ok(Some(_))) {}
        self.decoder.take();
    }
    pub(crate) async fn dependency(
        &mut self,
        response: &ControlResponse,
        context: &PrepareContext,
        allowed: &[MediaDescriptor],
    ) -> Result<bool, AiError> {
        if !matches!(
            response,
            ControlResponse::Credential {} | ControlResponse::Media { .. }
        ) {
            return Ok(false);
        }
        if self.dependencies >= portable::MAXIMUM_DEPENDENCY_REQUESTS {
            return Err(self.invalid());
        }
        self.dependencies += 1;
        match response {
            ControlResponse::Credential {} => {
                let credential = context.credential().ok_or_else(|| self.invalid())?;
                let bytes = credential.secret.expose_secret().as_bytes();
                self.charge_dependency(bytes.len())?;
                self.send_bytes(Kind::Binary, bytes).await?;
            }
            ControlResponse::Media {
                descriptor,
                offset,
                length,
            } => {
                let count = usize::try_from(*length).map_err(|_| self.invalid())?;
                if count == 0
                    || count > portable::MAXIMUM_BINARY_BYTES
                    || !allowed.contains(descriptor)
                    || offset
                        .checked_add(u64::from(*length))
                        .is_none_or(|end| end > descriptor.byte_len())
                {
                    return Err(self.invalid());
                }
                self.charge_dependency(count)?;
                let bytes = context
                    .resolve_media(descriptor, self.abort.clone())
                    .await
                    .map_err(|e| error(e.kind(), self.phase, self.dispatch))?;
                let start = usize::try_from(*offset).map_err(|_| self.invalid())?;
                let slice = bytes
                    .get(start..start + count)
                    .ok_or_else(|| self.invalid())?;
                self.send_bytes(Kind::Binary, slice).await?;
            }
            _ => unreachable!("dependency variants checked"),
        }
        Ok(true)
    }
    fn charge_dependency(&mut self, bytes: usize) -> Result<(), AiError> {
        self.dependency_bytes = self
            .dependency_bytes
            .checked_add(bytes as u64)
            .ok_or_else(|| self.invalid())?;
        if self.dependency_bytes > portable::MAXIMUM_DEPENDENCY_BYTES {
            return Err(self.invalid());
        }
        Ok(())
    }
}

pub(crate) async fn metadata(
    capability: &Capability,
    request: &ControlRequest,
) -> Result<ControlResponse, AiError> {
    let mut exchange = Exchange::open(capability, AbortSignal::new(), true)?;
    let result = async {
        exchange.send_request(request, false).await?;
        exchange.finish_request();
        let response = exchange.control().await?;
        exchange.terminal().await?;
        Ok(response)
    }
    .await;
    if result.is_err() {
        exchange.cancel_and_drain().await;
    }
    result
}
