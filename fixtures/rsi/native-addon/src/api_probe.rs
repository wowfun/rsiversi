use rsi_api_protocol::portable::{MAXIMUM_FRAGMENT_BYTES, MAXIMUM_HEADER_BYTES};
use rsi_meta_native::{Message, ProviderChannel};

fn bounded(message: &Message) -> Result<(), String> {
    if message.bytes.len() > (MAXIMUM_FRAGMENT_BYTES + 5).max(MAXIMUM_HEADER_BYTES + 1)
        || !message.capabilities.is_empty()
    {
        return Err("invalid API probe frame".into());
    }
    Ok(())
}

pub fn serve(channel: &mut ProviderChannel<'_>) -> Result<(), String> {
    let mut first = channel
        .receive()
        .map_err(|error| error.to_string())?
        .ok_or("missing API probe request")?;
    if first.capabilities.len() != 1 {
        return Err("expected one explicit API grant".into());
    }
    let grant = first.capabilities.pop().expect("one grant");
    bounded(&first)?;
    let host = channel.host();
    let mut call = host.open(&grant).map_err(|error| error.to_string())?;
    call.send(&first).map_err(|error| error.to_string())?;
    while let Some(message) = channel.receive().map_err(|error| error.to_string())? {
        bounded(&message)?;
        call.send(&message).map_err(|error| error.to_string())?;
    }
    call.finish_requests().map_err(|error| error.to_string())?;
    while let Some(message) = call.receive().map_err(|error| error.to_string())? {
        bounded(&message)?;
        channel.send(&message).map_err(|error| error.to_string())?;
    }
    call.terminal().map_err(|error| error.to_string())
}
