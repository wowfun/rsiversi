use async_trait::async_trait;
use rsi_api_protocol::{
    ApiError, ApiMessage, ByteBudget, ByteReceiver, ByteReservation, Result, RetainedBytes,
    portable::{FRAGMENT_TAG, HEADER_TAG, Header, MAXIMUM_FRAGMENT_BYTES, MAXIMUM_HEADER_BYTES},
};
use rsi_meta::{CapabilityCall, Message, ProviderChannel};

#[async_trait]
pub(crate) trait Port: Send {
    async fn send(&mut self, message: Message) -> Result<()>;
    async fn recv(&mut self) -> Result<Option<Message>>;
}
#[async_trait]
impl Port for CapabilityCall {
    async fn send(&mut self, message: Message) -> Result<()> {
        CapabilityCall::send(self, message)
            .await
            .map_err(|_| lost())
    }
    async fn recv(&mut self) -> Result<Option<Message>> {
        CapabilityCall::recv(self).await.map_err(|_| lost())
    }
}
#[async_trait]
impl Port for ProviderChannel<'_> {
    async fn send(&mut self, message: Message) -> Result<()> {
        ProviderChannel::send(self, message)
            .await
            .map_err(|_| lost())
    }
    async fn recv(&mut self) -> Result<Option<Message>> {
        Ok(ProviderChannel::recv(self).await)
    }
}
pub(crate) fn invalid() -> ApiError {
    ApiError::Invalid("invalid Portable API framing".into())
}
pub(crate) fn lost() -> ApiError {
    ApiError::Backend("Portable API exchange did not complete".into())
}

pub(crate) async fn send_header(
    port: &mut impl Port,
    scratch: &ByteBudget,
    header: &Header,
) -> Result<()> {
    let json = scratch.encode(header, MAXIMUM_HEADER_BYTES)?;
    let reservation = scratch.reserve(json.len() + 1)?;
    let mut frame = Vec::with_capacity(json.len() + 1);
    frame.push(HEADER_TAG);
    frame.extend_from_slice(json.as_bytes());
    port.send(Message::new(frame)).await?;
    drop(reservation);
    Ok(())
}
pub(crate) async fn header(port: &mut impl Port) -> Result<Header> {
    let message = port.recv().await?.ok_or_else(invalid)?;
    if !message.capabilities().is_empty()
        || message.as_bytes().first() != Some(&HEADER_TAG)
        || message.as_bytes().len() > MAXIMUM_HEADER_BYTES + 1
    {
        return Err(invalid());
    }
    serde_json::from_slice(&message.as_bytes()[1..]).map_err(|_| invalid())
}
pub(crate) async fn eof(port: &mut impl Port) -> Result<()> {
    if port.recv().await?.is_some() {
        Err(invalid())
    } else {
        Ok(())
    }
}

pub(crate) async fn send_payload(
    port: &mut impl Port,
    scratch: &ByteBudget,
    json: &[u8],
    binary: &[u8],
) -> Result<()> {
    let total = json
        .len()
        .checked_add(binary.len())
        .filter(|total| *total <= rsi_api_protocol::MAXIMUM_API_BYTES)
        .ok_or_else(invalid)?;
    let mut offset = 0;
    while offset < total {
        let end = (offset + MAXIMUM_FRAGMENT_BYTES).min(total);
        let reservation = scratch.reserve(end - offset + 5)?;
        let mut frame = Vec::with_capacity(end - offset + 5);
        frame.push(FRAGMENT_TAG);
        frame.extend_from_slice(&u32::try_from(offset).map_err(|_| invalid())?.to_le_bytes());
        if offset < json.len() {
            frame.extend_from_slice(&json[offset..end.min(json.len())]);
        }
        if end > json.len() {
            frame.extend_from_slice(&binary[offset.saturating_sub(json.len())..end - json.len()]);
        }
        port.send(Message::new(frame)).await?;
        drop(reservation);
        offset = end;
    }
    Ok(())
}
async fn receive(
    port: &mut impl Port,
    mut reservation: ByteReservation,
    total: usize,
) -> Result<ByteReceiver> {
    reservation.shrink(total)?;
    let mut body = reservation.receive();
    let mut received = 0;
    while received < total {
        let message = port.recv().await?.ok_or_else(invalid)?;
        let frame = message.as_bytes();
        if !message.capabilities().is_empty()
            || frame.first() != Some(&FRAGMENT_TAG)
            || frame.len() < 6
            || frame.len() > MAXIMUM_FRAGMENT_BYTES + 5
        {
            return Err(invalid());
        }
        let offset = usize::try_from(u32::from_le_bytes(
            frame[1..5].try_into().map_err(|_| invalid())?,
        ))
        .map_err(|_| invalid())?;
        let data = &frame[5..];
        if offset != received || data.len() > total - received {
            return Err(invalid());
        }
        body.append(data)?;
        received += data.len();
    }
    Ok(body)
}
pub(crate) async fn payload(
    port: &mut impl Port,
    reservation: ByteReservation,
    total: usize,
) -> Result<RetainedBytes> {
    Ok(receive(port, reservation, total).await?.finish())
}
pub(crate) async fn retained_payload(
    port: &mut impl Port,
    reservation: ByteReservation,
    total: usize,
    retained: &ByteBudget,
) -> Result<RetainedBytes> {
    receive(port, reservation, total)
        .await?
        .finish_into(retained)
}
pub(crate) fn total(json: usize, binary: Option<usize>, maximum: usize) -> Result<usize> {
    json.checked_add(binary.unwrap_or(0))
        .filter(|bytes| *bytes <= maximum)
        .ok_or_else(invalid)
}
pub(crate) async fn message(
    port: &mut impl Port,
    reservation: ByteReservation,
    retained: &ByteBudget,
    json: usize,
    binary: Option<usize>,
    maximum: usize,
) -> Result<ApiMessage> {
    let total = total(json, binary, maximum)?;
    let bytes = retained_payload(port, reservation, total, retained).await?;
    let json = bytes.slice(..json)?;
    serde_json::from_slice::<serde::de::IgnoredAny>(json.as_bytes()).map_err(|_| invalid())?;
    let binary = binary.map(|_| bytes.slice(json.len()..)).transpose()?;
    Ok(ApiMessage { json, binary })
}
