use super::*;

pub(super) trait FrameShape: serde::de::DeserializeOwned {
    fn validate_shape(value: &serde_json::Value) -> Result<(), SessionHostError>;
}

impl FrameShape for ClientFrame {
    fn validate_shape(value: &serde_json::Value) -> Result<(), SessionHostError> {
        validate_client_frame_shape(value)
    }
}

impl FrameShape for ServerFrame {
    fn validate_shape(value: &serde_json::Value) -> Result<(), SessionHostError> {
        validate_server_frame_shape(value)
    }
}

#[cfg(test)]
impl FrameShape for Option<()> {
    fn validate_shape(_value: &serde_json::Value) -> Result<(), SessionHostError> {
        Ok(())
    }
}

fn tagged_object<'a>(
    value: &'a serde_json::Value,
    tag: &str,
) -> Option<(&'a serde_json::Map<String, serde_json::Value>, &'a str)> {
    let object = value.as_object()?;
    Some((object, object.get(tag)?.as_str()?))
}

fn reject_unknown_fields(
    object: &serde_json::Map<String, serde_json::Value>,
    tag: &str,
    fields: &[&str],
    path: &str,
) -> Result<(), SessionHostError> {
    if let Some(field) = object
        .keys()
        .find(|field| field.as_str() != tag && !fields.contains(&field.as_str()))
    {
        return Err(SessionHostError::Invalid(format!(
            "wire frame contains unknown field `{field}` at {path}"
        )));
    }
    Ok(())
}

fn validate_client_frame_shape(value: &serde_json::Value) -> Result<(), SessionHostError> {
    let Some((object, variant)) = tagged_object(value, "type") else {
        return Ok(());
    };
    let fields = match variant {
        "hello" => &[
            "protocol_epoch",
            "product_build",
            "launch_key",
            "host_epoch",
        ][..],
        "request" => &["request_id", "operation"][..],
        "upload_chunk" => &["request_id", "upload_id", "index", "data"][..],
        "upload_end" => &["request_id"][..],
        _ => return Ok(()),
    };
    reject_unknown_fields(object, "type", fields, "client frame")?;
    if variant == "request"
        && let Some(operation) = object.get("operation")
    {
        validate_wire_operation_shape(operation)?;
    }
    Ok(())
}

fn validate_wire_operation_shape(value: &serde_json::Value) -> Result<(), SessionHostError> {
    let Some((object, variant)) = tagged_object(value, "type") else {
        return Ok(());
    };
    let fields = match variant {
        "probe" => &[][..],
        "create" => &["cwd", "session_id", "agent_preset_id", "workspace_trust"][..],
        "attach" | "header" | "pending_approvals" => &["session_id"][..],
        "list_recent" => &["after", "limit"][..],
        "submit_input" => &["session_id", "message_id", "content", "model", "sandbox"][..],
        "message_status" => &["session_id", "message_id"][..],
        "submit_image" => &["session_id", "turn_id", "model", "request"][..],
        "cancel" => &["session_id", "target", "reason"][..],
        "history" => &["session_id", "exclusive_before_seq", "limit"][..],
        "subscribe" => &["session_id", "cursor"][..],
        "answer_approval" => &["session_id", "approval_id", "decision"][..],
        _ => return Ok(()),
    };
    reject_unknown_fields(object, "type", fields, "wire operation")?;
    match variant {
        "submit_input" => {
            if let Some(content) = object.get("content").and_then(serde_json::Value::as_array) {
                for block in content {
                    validate_wire_input_block_shape(block)?;
                }
            }
        }
        "cancel" => {
            if let Some(target) = object.get("target") {
                validate_wire_cancel_target_shape(target)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_wire_input_block_shape(value: &serde_json::Value) -> Result<(), SessionHostError> {
    let Some((object, variant)) = tagged_object(value, "type") else {
        return Ok(());
    };
    let fields = match variant {
        "text" => &["text"][..],
        "image" => &["upload_id", "bytes", "sha256"][..],
        _ => return Ok(()),
    };
    reject_unknown_fields(object, "type", fields, "wire input block")
}

fn validate_wire_cancel_target_shape(value: &serde_json::Value) -> Result<(), SessionHostError> {
    let Some((object, variant)) = tagged_object(value, "type") else {
        return Ok(());
    };
    let fields = match variant {
        "message" => &["message_id"][..],
        "turn" => &["turn_id"][..],
        _ => return Ok(()),
    };
    reject_unknown_fields(object, "type", fields, "wire cancel target")
}

fn validate_server_frame_shape(value: &serde_json::Value) -> Result<(), SessionHostError> {
    let Some((object, variant)) = tagged_object(value, "type") else {
        return Ok(());
    };
    let fields = match variant {
        "hello_ok" => &[
            "protocol_epoch",
            "product_build",
            "launch_key",
            "host_epoch",
        ][..],
        "hello_rejected" => &["reason"][..],
        "response" => &["request_id", "response", "error"][..],
        "event" => &["request_id", "session_id", "update"][..],
        "item" => &["request_id", "item"][..],
        "end" => &["request_id", "error"][..],
        _ => return Ok(()),
    };
    reject_unknown_fields(object, "type", fields, "server frame")?;
    match variant {
        "response" => {
            if let Some(response) = object.get("response").filter(|value| !value.is_null()) {
                validate_wire_response_shape(response)?;
            }
            if let Some(error) = object.get("error").filter(|value| !value.is_null()) {
                validate_wire_error_shape(error)?;
            }
        }
        "event" => {
            if let Some(update) = object.get("update") {
                validate_wire_update_shape(update)?;
            }
        }
        "item" => {
            if let Some(item) = object.get("item") {
                validate_wire_item_shape(item)?;
            }
        }
        "end" => {
            if let Some(error) = object.get("error").filter(|value| !value.is_null()) {
                validate_wire_error_shape(error)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_wire_response_shape(value: &serde_json::Value) -> Result<(), SessionHostError> {
    let Some((object, variant)) = tagged_object(value, "type") else {
        return Ok(());
    };
    let fields = match variant {
        "ready" | "pending_approvals_start" | "subscribed" => &[][..],
        "session" => &["header"][..],
        "recent_start" => &["has_more"][..],
        "turn_receipt" => &["session_id", "turn_id", "accepted_seq"][..],
        "message_receipt" => &[
            "session_id",
            "message_id",
            "accepted_control_seq",
            "observed_fact_seq",
            "state",
        ][..],
        "cancel" => &["accepted", "already_terminal"][..],
        "history_start" => &["before_seq", "durable_seq", "has_more"][..],
        "approval_answer" => &["accepted"][..],
        _ => return Ok(()),
    };
    reject_unknown_fields(object, "type", fields, "wire response")?;
    if variant == "message_receipt"
        && let Some(state) = object.get("state")
    {
        validate_wire_message_state_shape(state)?;
    }
    Ok(())
}

fn validate_wire_message_state_shape(value: &serde_json::Value) -> Result<(), SessionHostError> {
    let Some((object, variant)) = tagged_object(value, "state") else {
        return Ok(());
    };
    let fields = match variant {
        "pending" => &[][..],
        "claimed" => &["activation_id", "turn_id", "step_id", "entered_fact_seq"][..],
        "discarded" => &["reason", "control_seq"][..],
        _ => return Ok(()),
    };
    reject_unknown_fields(object, "state", fields, "wire message state")
}

fn validate_wire_item_shape(value: &serde_json::Value) -> Result<(), SessionHostError> {
    let Some((object, variant)) = tagged_object(value, "type") else {
        return Ok(());
    };
    let fields = match variant {
        "session" => &["header"][..],
        "fact" => &["session_id", "fact"][..],
        "approval" => &["request"][..],
        _ => return Ok(()),
    };
    reject_unknown_fields(object, "type", fields, "wire item")
}

fn validate_wire_update_shape(value: &serde_json::Value) -> Result<(), SessionHostError> {
    let Some((object, variant)) = tagged_object(value, "type") else {
        return Ok(());
    };
    let fields = match variant {
        "control" => &["record", "durable_control_seq"][..],
        "fact" => &["fact", "durable_fact_seq"][..],
        _ => return Ok(()),
    };
    reject_unknown_fields(object, "type", fields, "wire update")
}

fn validate_wire_error_shape(value: &serde_json::Value) -> Result<(), SessionHostError> {
    let Some((object, variant)) = tagged_object(value, "kind") else {
        return Ok(());
    };
    let fields = match variant {
        "invalid" | "backend" => &["message"][..],
        "not_found" => &["value"][..],
        "conflict" => &["session", "turn"][..],
        "message_conflict" | "message_outcome_unknown" => &["session", "message"][..],
        "capacity" | "shutting_down" => &[][..],
        _ => return Ok(()),
    };
    reject_unknown_fields(object, "kind", fields, "wire error")
}

pub(super) async fn read_frame<R, T>(
    reader: &mut R,
    maximum_bytes: usize,
    budget: &FrameReadBudget,
) -> Result<T, SessionHostError>
where
    R: AsyncRead + Unpin,
    T: FrameShape,
{
    let length = read_frame_length(reader, maximum_bytes).await?;
    read_frame_body(reader, length, budget).await
}

pub(super) async fn read_frame_length<R>(
    reader: &mut R,
    maximum_bytes: usize,
) -> Result<usize, SessionHostError>
where
    R: AsyncRead + Unpin,
{
    let length = reader.read_u32().await.map_err(io_error)? as usize;
    if length == 0 || length > maximum_bytes {
        return Err(SessionHostError::Invalid(format!(
            "frame length must be within 1..={maximum_bytes} bytes"
        )));
    }
    Ok(length)
}

pub(super) async fn read_frame_body<R, T>(
    reader: &mut R,
    length: usize,
    budget: &FrameReadBudget,
) -> Result<T, SessionHostError>
where
    R: AsyncRead + Unpin,
    T: FrameShape,
{
    let _admission = budget.acquire(length).await?;
    let mut bytes = vec![0_u8; length];
    reader.read_exact(&mut bytes).await.map_err(io_error)?;
    decode_frame(&bytes)
}

fn decode_frame<T>(bytes: &[u8]) -> Result<T, SessionHostError>
where
    T: FrameShape,
{
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let shape = serde_json::Value::deserialize(&mut deserializer)
        .map_err(|error| SessionHostError::Invalid(error.to_string()))?;
    deserializer
        .end()
        .map_err(|error| SessionHostError::Invalid(error.to_string()))?;
    T::validate_shape(&shape)?;
    drop(shape);
    serde_json::from_slice(bytes).map_err(|error| SessionHostError::Invalid(error.to_string()))
}

pub(super) async fn read_frame_with_timeout<R, T>(
    reader: &mut R,
    maximum_bytes: usize,
    budget: &FrameReadBudget,
    timeout: Duration,
    phase: &str,
) -> Result<T, SessionHostError>
where
    R: AsyncRead + Unpin,
    T: FrameShape,
{
    tokio::time::timeout(timeout, read_frame(reader, maximum_bytes, budget))
        .await
        .map_err(|_| SessionHostError::Io(format!("Session Host {phase} read timed out")))?
}

pub(super) async fn read_frame_with_retained_budget<R, T>(
    reader: &mut R,
    maximum_bytes: usize,
    budget: &FrameReadBudget,
    timeout: Duration,
    phase: &str,
) -> Result<(T, OwnedSemaphorePermit), SessionHostError>
where
    R: AsyncRead + Unpin,
    T: FrameShape,
{
    tokio::time::timeout(timeout, async {
        let length = read_frame_length(reader, maximum_bytes).await?;
        let admission = budget.acquire(length).await?;
        let mut bytes = vec![0_u8; length];
        reader.read_exact(&mut bytes).await.map_err(io_error)?;
        let decoded = decode_frame(&bytes)?;
        Ok((decoded, admission))
    })
    .await
    .map_err(|_| SessionHostError::Io(format!("Session Host {phase} read timed out")))?
}

pub(super) async fn read_subscription_frame<R, T>(
    reader: &mut R,
    maximum_bytes: usize,
    budget: &FrameReadBudget,
) -> Result<T, SessionHostError>
where
    R: AsyncRead + Unpin,
    T: FrameShape,
{
    let length = read_frame_length(reader, maximum_bytes).await?;
    tokio::time::timeout(
        RESPONSE_READ_TIMEOUT,
        read_frame_body(reader, length, budget),
    )
    .await
    .map_err(|_| {
        SessionHostError::Io("Session Host subscription frame body read timed out".into())
    })?
}

pub(super) async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<(), SessionHostError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let bytes =
        serde_json::to_vec(value).map_err(|error| SessionHostError::Invalid(error.to_string()))?;
    if bytes.is_empty() || bytes.len() > MAXIMUM_FRAME_BYTES {
        return Err(SessionHostError::Invalid(format!(
            "encoded frame length must be within 1..={MAXIMUM_FRAME_BYTES} bytes"
        )));
    }
    let length = u32::try_from(bytes.len())
        .map_err(|_| SessionHostError::Invalid("frame length exceeds u32".into()))?;
    tokio::time::timeout(WRITE_TIMEOUT, async {
        writer.write_u32(length).await?;
        writer.write_all(&bytes).await?;
        writer.flush().await
    })
    .await
    .map_err(|_| SessionHostError::Io("Session Host frame write timed out".into()))?
    .map_err(io_error)
}

pub(super) fn validate_request_id(value: &str) -> Result<(), SessionHostError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(SessionHostError::Invalid(
            "request identity is empty, oversized, or malformed".into(),
        ));
    }
    Ok(())
}

pub(super) fn create_private_runtime_directory(path: &Path) -> Result<(), SessionHostError> {
    let parent = path.parent().ok_or_else(|| {
        SessionHostError::Invalid("Session Host runtime path has no parent".into())
    })?;
    if parent.file_name().is_some_and(|name| name == "rsi") {
        let runtime_root = parent.parent().ok_or_else(|| {
            SessionHostError::Invalid("Session Host runtime root is missing".into())
        })?;
        fs::create_dir_all(runtime_root).map_err(io_error)?;
        validate_effective_user_directory(runtime_root, "Session Host runtime root")?;
        create_directory(parent)?;
        validate_effective_user_directory(parent, "Session Host runtime parent")?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
        create_directory(path)?;
    } else {
        fs::create_dir_all(path).map_err(io_error)?;
    }
    validate_effective_user_directory(path, "Session Host runtime path")?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(io_error)
}

pub(super) fn create_directory(path: &Path) -> Result<(), SessionHostError> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(io_error(error)),
    }
}

pub(super) fn validate_effective_user_directory(
    path: &Path,
    label: &str,
) -> Result<(), SessionHostError> {
    let metadata = fs::symlink_metadata(path).map_err(io_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(SessionHostError::Invalid(format!(
            "{label} is not a real directory"
        )));
    }
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(SessionHostError::Invalid(format!(
            "{label} is not owned by the effective user"
        )));
    }
    Ok(())
}

pub(super) fn remove_stale_socket_after_failed_probe(path: &Path) -> Result<(), SessionHostError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error(error)),
    };
    if !metadata.file_type().is_socket() {
        return Err(SessionHostError::Invalid(
            "existing Session Host endpoint is not a socket".into(),
        ));
    }
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => Err(SessionHostError::OwnerActive),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            ) =>
        {
            let current = fs::symlink_metadata(path).map_err(io_error)?;
            if current.file_type().is_socket()
                && current.dev() == metadata.dev()
                && current.ino() == metadata.ino()
            {
                fs::remove_file(path).map_err(io_error)
            } else {
                Err(SessionHostError::Invalid(
                    "Session Host endpoint changed during stale probe".into(),
                ))
            }
        }
        Err(error) => Err(io_error(error)),
    }
}

#[allow(clippy::needless_pass_by_value)] // Kept as direct I/O `map_err` adapters.
pub(super) fn io_error(error: io::Error) -> SessionHostError {
    SessionHostError::Io(error.to_string())
}

#[allow(clippy::needless_pass_by_value)]
pub(super) fn io_as_session_error(error: io::Error) -> SessionApplicationError {
    SessionApplicationError::Backend(error.to_string())
}

#[allow(clippy::needless_pass_by_value)]
pub(super) fn host_as_session_error(error: SessionHostError) -> SessionApplicationError {
    SessionApplicationError::Backend(error.to_string())
}

pub(super) fn host_as_wire_error(error: SessionHostError) -> WireError {
    match error {
        SessionHostError::Invalid(message) => WireError::Invalid { message },
        other => WireError::Backend {
            message: other.to_string(),
        },
    }
}

pub(super) fn message_outcome_unknown(
    session_id: &SessionId,
    message_id: &MessageId,
) -> SessionApplicationError {
    SessionApplicationError::MessageOutcomeUnknown {
        session: session_id.to_string(),
        message: message_id.to_string(),
    }
}
