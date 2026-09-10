use super::{
    ApplicationWork, CLI_RENDER_CHANNEL_CAPACITY, OsString, OutputMode, Result, RsiError,
    SessionHandle, arm_signal, join_cli_renderer, report_error, session_cli, spawn_cli_renderer,
    usage,
};
use rsi_agent_session_protocol::{DomainRequestId, SessionCommandInvocation};
use tokio_util::sync::CancellationToken;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arguments::Command;
    #[test]
    fn headless_command_grammar_rejects_ambiguous_modes_and_bounds_full_invocation() {
        let parse =
            |arguments: Vec<&str>| Command::parse(arguments.into_iter().map(OsString::from));
        for arguments in [
            vec!["--commands", "task"],
            vec!["--commands", "--command-status", "id"],
            vec!["--command-status", "id", "--stdin"],
            vec!["--commands", "--message-id", "message"],
            vec!["--command", "{}"],
        ] {
            assert!(parse(arguments).is_err());
        }
        assert!(parse(vec!["--commands"]).is_ok());
        assert!(parse(vec!["--command-status", "request"]).is_ok());
        let valid = serde_json::json!({"command":"fixture.plan","request_id":"request","expected_revision":{"kind":"draft","revision":0},"arguments":"on"}).to_string();
        assert!(parse(vec!["--command", &valid]).is_ok());
        assert!(parse(vec!["--command", &valid, "task"]).is_ok());
        assert!(parse(vec!["--command", &" ".repeat(32 * 1024)]).is_err());
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Extension {
    List,
    Execute(Box<SessionCommandInvocation>),
    Status(DomainRequestId),
}
impl Extension {
    pub fn parse(flag: &str, arguments: &mut impl Iterator<Item = OsString>) -> Result<Self> {
        use rsi_application::arguments::string_value;
        match flag {
            "--commands" => Ok(Self::List),
            "--command" => {
                let source = string_value(arguments, flag)?;
                if source.len() > rsi_agent_session_protocol::MAXIMUM_COMMAND_ARGUMENT_BYTES + 4096
                {
                    return Err(usage("command JSON exceeds its bound"));
                }
                serde_json::from_str(&source)
                    .map(|invocation| Self::Execute(Box::new(invocation)))
                    .map_err(|error| usage(error.to_string()))
            }
            "--command-status" => DomainRequestId::new(string_value(arguments, flag)?)
                .map(Self::Status)
                .map_err(|error| usage(error.to_string())),
            _ => Err(usage("unknown Session command operation")),
        }
    }
    pub fn validate(&self, has_task: bool) -> Result<()> {
        if has_task && !matches!(self, Self::Execute(_)) {
            return Err(usage(
                "--commands and --command-status cannot submit a task",
            ));
        }
        Ok(())
    }
    async fn execute(&self, handle: &dyn SessionHandle) -> Result<serde_json::Value> {
        let execution = rsi_meta::Execution::native(tokio::runtime::Handle::current());
        let value = match self {
            Self::List => serde_json::to_value(
                rsi_client::read_with_capacity_retry(&execution, || handle.commands())
                    .await
                    .map_err(|e| RsiError::Run(e.to_string()))?,
            ),
            Self::Execute(invocation) => serde_json::to_value(
                rsi_client::execute_command_once(handle, invocation.as_ref().clone())
                    .await
                    .map_err(|e| RsiError::Run(e.to_string()))?,
            ),
            Self::Status(id) => serde_json::to_value(
                rsi_client::read_with_capacity_retry(&execution, || handle.command_status(id))
                    .await
                    .map_err(|e| RsiError::Run(e.to_string()))?,
            ),
        };
        value.map_err(|error| RsiError::Run(error.to_string()))
    }
}

pub(crate) async fn run(
    extension: &Extension,
    handle: &dyn SessionHandle,
    output: OutputMode,
    work: &ApplicationWork,
) -> u8 {
    let cancellation = CancellationToken::new();
    let signal = match arm_signal(cancellation.clone(), work).await {
        Ok(signal) => signal,
        Err(error) => return report_error(&error),
    };
    let (sender, receiver) = tokio::sync::mpsc::channel(CLI_RENDER_CHANNEL_CAPACITY);
    let rendering = spawn_cli_renderer(output, receiver, work);
    let render_stop = rendering.stop.clone();
    let mut action = Box::pin(async {
        let value = extension.execute(handle).await?;
        session_cli::notice(&sender, "command_result", value).await
    });
    let result = tokio::select! { biased;
        () = cancellation.cancelled() => None,
        () = work.stop.cancelled() => None,
        result = &mut action => Some(result),
    };
    drop(action);
    drop(sender);
    let mut flush = Box::pin(join_cli_renderer(rendering));
    let interrupted = result.is_none();
    if interrupted {
        render_stop.cancel();
    }
    let flushed = tokio::select! { biased;
        () = cancellation.cancelled(), if !interrupted => { render_stop.cancel(); (&mut flush).await },
        () = work.stop.cancelled(), if !interrupted => { render_stop.cancel(); (&mut flush).await },
        result = &mut flush => result,
    };
    signal.abort();
    let _ = signal.await;
    if interrupted || cancellation.is_cancelled() || work.stop.is_cancelled() {
        return 130;
    }
    match result.expect("not interrupted").and(flushed) {
        Ok(()) => 0,
        Err(error) => report_error(&error),
    }
}
