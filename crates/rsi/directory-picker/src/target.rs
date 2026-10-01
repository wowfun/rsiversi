use super::native;
use rsi_directory_picker_api::{
    CreateRequest, Created, Failure, ListRequest, Listing, MAXIMUM_REPLY, Result,
};
use rsi_execution::ExecutionLease;
use rsi_process::{ProcessError, ProcessSpec};
use rsi_sandbox::{ProcessRequest, ProcessStdio, SandboxMode};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    io::{Read as _, Write as _},
    path::PathBuf,
};
use tokio_util::sync::CancellationToken;

const MARKER: &str = "--rsi-directory-picker";
const MAXIMUM_REQUEST: usize = 128 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(
    tag = "operation",
    content = "request",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum Request {
    List(ListRequest),
    Create(CreateRequest),
}
impl Request {
    fn validate(&self) -> Result<()> {
        use rsi_directory_picker_api::{validate_name, validate_path};
        match self {
            Self::List(request) => request.path.as_deref().map_or(Ok(()), validate_path),
            Self::Create(request) => {
                validate_path(&request.parent).and_then(|()| validate_name(&request.name))
            }
        }
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(
    tag = "result",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum Reply {
    Listing(Listing),
    Created(Created),
}

/// Runs the bounded fixed picker entry. The launcher supplies only the target account HOME.
/// This entry must be launched under explicitly admitted directory-picker authority.
pub fn maybe_run_directory_picker_helper(arguments: &[OsString]) -> Option<u8> {
    if arguments.first().is_none_or(|value| value != MARKER) {
        return None;
    }
    if arguments.len() != 1 {
        return Some(2);
    }
    let result = (|| {
        let mut bytes = Vec::new();
        std::io::stdin()
            .lock()
            .take(MAXIMUM_REQUEST as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Failure::Invalid)?;
        if bytes.len() > MAXIMUM_REQUEST {
            return Err(Failure::Invalid);
        }
        let request: Request = serde_json::from_slice(&bytes).map_err(|_| Failure::Invalid)?;
        request.validate()?;
        let stop = CancellationToken::new();
        match request {
            Request::List(request) => {
                native::list(request, std::env::var_os("HOME").map(PathBuf::from), stop)
                    .map(Reply::Listing)
            }
            Request::Create(request) => native::create(request, stop).map(Reply::Created),
        }
    })();
    let bytes = serde_json::to_vec(&result)
        .ok()
        .filter(|bytes| bytes.len() <= MAXIMUM_REPLY + 1024);
    Some(match bytes {
        Some(bytes) if std::io::stdout().lock().write_all(&bytes).is_ok() => 0,
        _ => 2,
    })
}

fn failure(error: &ProcessError) -> Failure {
    match error {
        ProcessError::Unsupported => Failure::Unsupported,
        ProcessError::OutcomeUnknown => Failure::OutcomeUnknown,
        ProcessError::ShuttingDown | ProcessError::Api(_) => Failure::Cancelled,
        _ => Failure::Io {
            message: "target directory operation unavailable".into(),
        },
    }
}
fn check(stop: &CancellationToken) -> Result<()> {
    if stop.is_cancelled() {
        Err(Failure::Cancelled)
    } else {
        Ok(())
    }
}
pub(super) async fn exchange(
    execution: ExecutionLease,
    request: Request,
    stop: CancellationToken,
) -> Result<Reply> {
    request.validate()?;
    let _admission = execution.admit().map_err(|error| failure(&error))?;
    check(&stop)?;
    let program = execution
        .resolve_program("directory_picker")
        .await
        .map_err(|error| failure(&error))?;
    let home = program
        .environment()
        .iter()
        .find(|(key, _)| key == "HOME")
        .map(|(_, value)| PathBuf::from(value))
        .ok_or(Failure::HomeUnavailable)?;
    let stdin = serde_json::to_vec(&request).map_err(|_| Failure::Invalid)?;
    if stdin.len() > MAXIMUM_REQUEST {
        return Err(Failure::Invalid);
    }
    check(&stop)?;
    let plan = execution
        .prepare(ProcessRequest {
            stdio: ProcessStdio::Pipes,
            mode: SandboxMode::DangerFullAccess,
            program,
            arguments: vec![MARKER.into()],
            cwd: home.clone(),
            workspace: home,
        })
        .await
        .map_err(|error| failure(&error))?;
    check(&stop)?;
    let environment = plan.environment().to_vec();
    let process = execution
        .spawn(ProcessSpec {
            process: plan,
            stdin,
            environment,
            stdout_max_bytes: MAXIMUM_REPLY + 1024,
            stderr_max_bytes: 4096,
            termination_grace_ms: 100,
        })
        .await
        .map_err(|error| failure(&error))?;
    let uncertain = || {
        if matches!(request, Request::Create(_)) {
            Failure::OutcomeUnknown
        } else {
            Failure::Cancelled
        }
    };
    let outcome = tokio::select! {
        biased;
        () = stop.cancelled() => {
            process.terminate();
            let _ = process.wait().await;
            return Err(uncertain());
        }
        outcome = process.wait() => outcome,
    };
    if outcome.map_err(|_| uncertain())?.exit_code != Some(0) {
        return Err(uncertain());
    }
    let output = process.stdout().read_from(0).map_err(|_| uncertain())?;
    if output.lossy || output.oldest_offset != 0 || output.next_offset != output.bytes.len() as u64
    {
        return Err(uncertain());
    }
    let reply: Result<Reply> = serde_json::from_slice(&output.bytes).map_err(|_| uncertain())?;
    match (&request, &reply) {
        (Request::List(request), Ok(Reply::Listing(value))) => value.validate(request)?,
        (Request::Create(request), Ok(Reply::Created(value))) => value.validate(request)?,
        (_, Err(Failure::Io { message })) if message.len() > 512 => return Err(uncertain()),
        (_, Err(_)) => (),
        _ => return Err(uncertain()),
    }
    reply
}
