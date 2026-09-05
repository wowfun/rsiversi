//! Linux structured apply-patch capability plugin.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

#[cfg(target_os = "linux")]
mod patch_engine;

use async_trait::async_trait;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_process::ProcessContract;
use rsi_tools_protocol::{ToolError, ToolRegistrarContract};
use serde_json::Value;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Maximum post-decode UTF-8 bytes in one apply-patch document.
pub const MAXIMUM_APPLY_PATCH_BYTES: usize = 2_000_000;

#[cfg(target_os = "linux")]
const APPLY_PATCH_HELPER_MARKER: &str = "--rsi-run-as-apply-patch";
#[cfg(target_os = "linux")]
const APPLY_PATCH_TOOL_TIMEOUT_MS: u64 = 45_000;
#[cfg(target_os = "linux")]
const HELPER_STDERR_CAPTURE_BYTES: usize = 64_000;
#[cfg(target_os = "linux")]
const HELPER_TERMINATION_GRACE_MS: u64 = 3_000;

/// Runs the hidden apply-patch helper only for its exact sole argv marker.
///
/// The caller must pass arguments after `argv[0]`. Non-matching invocations do
/// not read stdin or write output and return `None` for normal CLI dispatch.
#[cfg(target_os = "linux")]
pub fn maybe_run_apply_patch_helper<I>(arguments: I) -> Option<u8>
where
    I: IntoIterator<Item = OsString>,
{
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    if !is_apply_patch_helper_invocation(&arguments) {
        return None;
    }
    let root = match std::env::current_dir() {
        Ok(root) => root,
        Err(error) => {
            let response = patch_engine::rejection(
                "cwd_unavailable",
                format!("helper cwd is unavailable: {error}"),
            );
            return Some(write_helper_response(&response));
        }
    };
    Some(run_apply_patch_helper_io(
        &root,
        std::io::stdin().lock(),
        std::io::stdout().lock(),
    ))
}

/// Declines the Linux-only hidden helper dispatch on unsupported hosts.
#[cfg(not(target_os = "linux"))]
pub fn maybe_run_apply_patch_helper<I>(arguments: I) -> Option<u8>
where
    I: IntoIterator<Item = OsString>,
{
    let _ = arguments.into_iter();
    None
}

#[cfg(target_os = "linux")]
fn is_apply_patch_helper_invocation(arguments: &[OsString]) -> bool {
    arguments.len() == 1 && arguments[0] == APPLY_PATCH_HELPER_MARKER
}

#[cfg(target_os = "linux")]
fn run_apply_patch_helper_io(
    root: &Path,
    mut input: impl std::io::Read,
    mut output: impl std::io::Write,
) -> u8 {
    use std::io::Read as _;

    let mut bytes = Vec::new();
    let response = match input
        .by_ref()
        .take((MAXIMUM_APPLY_PATCH_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
    {
        Err(error) => patch_engine::rejection(
            "stdin_read_failed",
            format!("failed to read helper stdin: {error}"),
        ),
        Ok(_) if bytes.len() > MAXIMUM_APPLY_PATCH_BYTES => patch_engine::rejection(
            "patch_too_large",
            format!("patch exceeds {MAXIMUM_APPLY_PATCH_BYTES} UTF-8 bytes"),
        ),
        Ok(_) => match std::str::from_utf8(&bytes) {
            Ok(patch) => patch_engine::apply_patch(root, patch),
            Err(_) => patch_engine::rejection("invalid_utf8", "patch stdin must be UTF-8"),
        },
    };
    u8::from(
        serde_json::to_writer(&mut output, &response)
            .and_then(|()| output.write_all(b"\n").map_err(serde_json::Error::io))
            .and_then(|()| output.flush().map_err(serde_json::Error::io))
            .is_err(),
    )
}

#[cfg(target_os = "linux")]
fn write_helper_response(response: &patch_engine::PatchHelperResponse) -> u8 {
    use std::io::Write as _;

    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    u8::from(
        serde_json::to_writer(&mut stdout, response)
            .and_then(|()| stdout.write_all(b"\n").map_err(serde_json::Error::io))
            .and_then(|()| stdout.flush().map_err(serde_json::Error::io))
            .is_err(),
    )
}

/// Ordinary factory for the model-facing structured apply-patch tool.
#[derive(Clone, Debug)]
pub struct ApplyPatchToolFactory {
    helper: PathBuf,
}

impl ApplyPatchToolFactory {
    /// Creates a factory from one explicit absolute helper executable.
    pub fn new(helper: impl Into<PathBuf>) -> rsi_tools_protocol::Result<Self> {
        let helper = canonical_executable("apply-patch helper", &helper.into())?;
        Ok(Self { helper })
    }

    /// Returns the canonical helper executable frozen into this factory.
    pub fn executable(&self) -> &Path {
        &self.helper
    }

    fn retained_bytes(&self) -> usize {
        self.helper.as_os_str().len()
    }
}

fn canonical_executable(kind: &str, path: &Path) -> rsi_tools_protocol::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(ToolError::InvalidInput(format!(
            "{kind} program path must be absolute"
        )));
    }
    let resolved = std::fs::canonicalize(path)
        .map_err(|error| ToolError::InvalidInput(format!("{kind} is unavailable: {error}")))?;
    let metadata = resolved
        .metadata()
        .map_err(|error| ToolError::InvalidInput(format!("{kind} is unavailable: {error}")))?;
    if !metadata.is_file() {
        return Err(ToolError::InvalidInput(format!(
            "{kind} path must name a canonical regular executable file"
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(ToolError::InvalidInput(format!(
                "{kind} path must name a canonical regular executable file"
            )));
        }
    }
    Ok(resolved)
}

#[async_trait]
impl PluginFactory for ApplyPatchToolFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "apply-patch tool configuration must be null".into(),
            ));
        }
        Ok(
            PreparedActivation::with_state(Value::Null, (), self.retained_bytes())
                .requiring_local::<ToolRegistrarContract>()
                .requiring_local::<ProcessContract>(),
        )
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        activate_tool(plan, self)
    }
}

#[cfg(target_os = "linux")]
fn activate_tool(plan: ActivationPlan, factory: &ApplyPatchToolFactory) -> rsi_meta::Result<()> {
    linux::activate_tool(plan, factory)
}

#[cfg(not(target_os = "linux"))]
fn activate_tool(
    mut plan: ActivationPlan,
    factory: &ApplyPatchToolFactory,
) -> rsi_meta::Result<()> {
    let (): () = plan.take_state()?;
    let _ = factory;
    Err(MetaError::Activation(
        "apply-patch requires Linux descriptor-relative filesystem semantics".into(),
    ))
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{
        APPLY_PATCH_HELPER_MARKER, APPLY_PATCH_TOOL_TIMEOUT_MS, ActivationPlan,
        ApplyPatchToolFactory, HELPER_STDERR_CAPTURE_BYTES, HELPER_TERMINATION_GRACE_MS, MetaError,
        PathBuf, ProcessContract, ToolError, ToolRegistrarContract, Value, patch_engine,
    };
    use async_trait::async_trait;
    use rsi_process::{Process, ProcessError, ProcessSpec};
    use rsi_tools_protocol::{
        ToolContent, ToolDefinition, ToolExecution, ToolExecutor, ToolRegistration, ToolResult,
    };
    use serde::Deserialize;
    use serde_json::json;
    use std::sync::Arc;

    pub(super) fn activate_tool(
        mut plan: ActivationPlan,
        factory: &ApplyPatchToolFactory,
    ) -> rsi_meta::Result<()> {
        let (): () = plan.take_state()?;
        let registrar = plan.local::<ToolRegistrarContract>()?;
        let process = plan.local::<ProcessContract>()?;
        let registration =
            apply_patch_registration(factory.helper.clone(), process).map_err(|error| {
                MetaError::Activation(format!("invalid apply-patch tool definition: {error}"))
            })?;
        let lease = registrar
            .register_batch(vec![registration])
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "release apply-patch Tool contribution lease",
            Box::new(move || {
                Box::pin(async move { lease.retire().map_err(|error| error.to_string()) })
            }),
        )
    }

    fn apply_patch_registration(
        helper: PathBuf,
        process: Arc<dyn Process>,
    ) -> rsi_tools_protocol::Result<ToolRegistration> {
        Ok(ToolRegistration {
            definition: ToolDefinition::new(
                "apply_patch",
                "Apply one bounded structured patch relative to the tool cwd. The helper preflights every operation; a later commit failure returns partial effects and is never replayed automatically.",
                json!({
                    "type":"object",
                    "properties":{"patch":{"type":"string"}},
                    "required":["patch"],
                    "additionalProperties":false
                }),
            )?,
            timeout_ms: APPLY_PATCH_TOOL_TIMEOUT_MS,
            executor: Arc::new(ApplyPatchTool { helper, process }),
        })
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ApplyPatchArguments {
        patch: String,
    }

    #[derive(Debug)]
    struct ApplyPatchTool {
        helper: PathBuf,
        process: Arc<dyn Process>,
    }

    #[async_trait]
    impl ToolExecutor for ApplyPatchTool {
        async fn execute(
            &self,
            arguments: Value,
            execution: ToolExecution,
        ) -> rsi_tools_protocol::Result<ToolResult> {
            let arguments: ApplyPatchArguments = match parse_arguments(arguments) {
                Ok(arguments) => arguments,
                Err(result) => return Ok(*result),
            };
            if let Err(failure) = patch_engine::validate_patch_document(&arguments.patch) {
                return error_result(&failure.code, failure.message);
            }
            let confined = execution
                .confine(self.helper.clone(), vec![APPLY_PATCH_HELPER_MARKER.into()])
                .await?;
            let managed = match self.process.spawn(ProcessSpec {
                process: confined,
                stdin: arguments.patch.into_bytes(),
                environment: Vec::new(),
                stdout_max_bytes: rsi_process::MAXIMUM_PROCESS_STREAM_BYTES,
                stderr_max_bytes: HELPER_STDERR_CAPTURE_BYTES,
                termination_grace_ms: HELPER_TERMINATION_GRACE_MS,
            }) {
                Ok(managed) => managed,
                Err(error) => return process_error_result(&error),
            };
            let waiting = managed.clone();
            let outcome = tokio::select! {
                biased;
                outcome = waiting.wait() => outcome,
                () = execution.cancellation.cancelled() => {
                    managed.terminate();
                    let _outcome = managed.wait().await;
                    return unknown_effects_result(
                        "apply-patch was interrupted after its helper started; filesystem effects are unknown and this invocation must not be replayed",
                    );
                }
            }
            .map_err(|error| ToolError::Execution(error.to_string()))?;
            if outcome.exit_code != Some(0) || outcome.signal.is_some() {
                return Err(ToolError::Execution(format!(
                    "apply-patch helper exited unexpectedly (code {:?}, signal {:?}); invocation was not replayed",
                    outcome.exit_code, outcome.signal
                )));
            }
            let stdout = managed
                .stdout()
                .read_from(0)
                .map_err(|error| ToolError::Execution(error.to_string()))?;
            let stderr = managed
                .stderr()
                .read_from(0)
                .map_err(|error| ToolError::Execution(error.to_string()))?;
            if stdout.lossy || stderr.lossy {
                return Err(ToolError::Execution(
                    "apply-patch helper output was truncated; invocation was not replayed".into(),
                ));
            }
            if !stderr.bytes.is_empty() {
                return Err(ToolError::Execution(
                    "apply-patch helper wrote unexpected stderr; invocation was not replayed"
                        .into(),
                ));
            }
            let response = parse_helper_response(&stdout.bytes)?;
            let is_error = response.status != patch_engine::PatchStatus::Applied;
            let text = String::from_utf8(stdout.bytes[..stdout.bytes.len() - 1].to_vec()).map_err(
                |_| ToolError::Execution("apply-patch helper stdout was not UTF-8".into()),
            )?;
            let value = serde_json::to_value(response)
                .map_err(|error| ToolError::Execution(error.to_string()))?;
            result_with_text(value, text, is_error)
        }
    }

    fn parse_arguments<T>(arguments: Value) -> std::result::Result<T, Box<ToolResult>>
    where
        T: for<'de> Deserialize<'de>,
    {
        serde_json::from_value(arguments).map_err(|error| {
            Box::new(
                error_result(
                    "invalid_arguments",
                    format!("arguments do not match the tool schema: {error}"),
                )
                .expect("bounded static error result is valid"),
            )
        })
    }

    fn parse_helper_response(
        bytes: &[u8],
    ) -> rsi_tools_protocol::Result<patch_engine::PatchHelperResponse> {
        let Some(json) = bytes.strip_suffix(b"\n") else {
            return Err(ToolError::Execution(
                "apply-patch helper stdout lacked its sole final LF; invocation was not replayed"
                    .into(),
            ));
        };
        if json.is_empty() || json.contains(&b'\n') || json.contains(&b'\r') {
            return Err(ToolError::Execution(
                "apply-patch helper stdout was not exactly one JSON line; invocation was not replayed"
                    .into(),
            ));
        }
        let response: patch_engine::PatchHelperResponse =
            serde_json::from_slice(json).map_err(|_| {
                ToolError::Execution(
                    "apply-patch helper stdout was invalid JSON; invocation was not replayed"
                        .into(),
                )
            })?;
        response.validate().map_err(|message| {
            ToolError::Execution(format!(
                "apply-patch helper response was invalid ({message}); invocation was not replayed"
            ))
        })?;
        Ok(response)
    }

    fn process_error_result(error: &ProcessError) -> rsi_tools_protocol::Result<ToolResult> {
        let code = match error {
            ProcessError::Capacity => "process_capacity",
            ProcessError::ShuttingDown => "process_shutting_down",
            ProcessError::Unsupported => "process_unsupported",
            ProcessError::SettlementTimeout => "process_settlement_timeout",
            ProcessError::ShutdownTimeout => "process_shutdown_timeout",
            ProcessError::InvalidInput(_) | ProcessError::Spawn(_) | ProcessError::Io(_) => {
                "process_error"
            }
        };
        error_result(code, error.to_string())
    }

    fn error_result(
        code: &str,
        message: impl Into<String>,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        let message = message.into();
        result_with_text(json!({"code":code,"message":message}), message, true)
    }

    fn unknown_effects_result(message: &str) -> rsi_tools_protocol::Result<ToolResult> {
        result_with_text(
            json!({
                "code":"effects_unknown",
                "message":message,
                "effects_known":false,
                "replay_safe":false
            }),
            message.to_owned(),
            true,
        )
    }

    fn result_with_text(
        value: Value,
        text: String,
        is_error: bool,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        ToolResult::new(value, vec![ToolContent::Text { text }], is_error)
    }

    #[cfg(test)]
    mod tests {
        #![allow(clippy::wildcard_imports)]

        use super::*;

        #[test]
        fn helper_response_parser_rejects_extra_lines_and_invalid_status_shapes() {
            assert!(parse_helper_response(b"{}\nextra\n").is_err());
            assert!(parse_helper_response(b"{}").is_err());
            let invalid = serde_json::json!({
                "status":"applied",
                "delta_exact":true,
                "effects":[],
                "fuzzy_matches":[],
                "failure":null
            });
            let mut line = serde_json::to_vec(&invalid).unwrap();
            line.push(b'\n');
            assert!(parse_helper_response(&line).is_err());
        }

        #[test]
        fn helper_response_parser_rejects_unknown_effect_kinds() {
            let unknown_effect = serde_json::json!({
                "status":"applied",
                "delta_exact":true,
                "effects":[{
                    "operation":0,
                    "kind":"overwrite",
                    "path":"target.txt",
                    "bytes_before":0,
                    "bytes_after":1
                }],
                "fuzzy_matches":[],
                "failure":null
            });
            let mut line = serde_json::to_vec(&unknown_effect).unwrap();
            line.push(b'\n');
            assert!(parse_helper_response(&line).is_err());
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #![allow(clippy::wildcard_imports)]

    use super::*;
    use std::fmt::Write as _;

    #[test]
    fn helper_dispatch_requires_the_exact_sole_marker() {
        assert!(is_apply_patch_helper_invocation(&[OsString::from(
            APPLY_PATCH_HELPER_MARKER
        )]));
        assert!(!is_apply_patch_helper_invocation(&[]));
        assert!(!is_apply_patch_helper_invocation(&[
            OsString::from(APPLY_PATCH_HELPER_MARKER),
            OsString::from("extra"),
        ]));
    }

    #[test]
    fn helper_io_returns_one_json_line_for_applied_and_rejected_outcomes() {
        let root = tempfile::tempdir().unwrap();
        let mut applied_output = Vec::new();
        assert_eq!(
            run_apply_patch_helper_io(
                root.path(),
                b"*** Begin Patch\n*** Add File: added.txt\n+value\n*** End Patch\n".as_slice(),
                &mut applied_output,
            ),
            0
        );
        assert!(applied_output.ends_with(b"\n"));
        let applied: patch_engine::PatchHelperResponse =
            serde_json::from_slice(&applied_output[..applied_output.len() - 1]).unwrap();
        assert_eq!(applied.status, patch_engine::PatchStatus::Applied);

        let mut rejected_output = Vec::new();
        assert_eq!(
            run_apply_patch_helper_io(root.path(), b"not a patch".as_slice(), &mut rejected_output,),
            0
        );
        let rejected: patch_engine::PatchHelperResponse =
            serde_json::from_slice(&rejected_output[..rejected_output.len() - 1]).unwrap();
        assert_eq!(rejected.status, patch_engine::PatchStatus::Rejected);
        assert!(rejected.effects.is_empty());
    }

    #[test]
    fn helper_io_rejects_invalid_utf8_and_input_over_the_budget() {
        let root = tempfile::tempdir().unwrap();
        for (input, code) in [
            (vec![0xff], "invalid_utf8"),
            (vec![b'x'; MAXIMUM_APPLY_PATCH_BYTES + 1], "patch_too_large"),
        ] {
            let mut output = Vec::new();
            assert_eq!(
                run_apply_patch_helper_io(root.path(), input.as_slice(), &mut output),
                0
            );
            let response: patch_engine::PatchHelperResponse =
                serde_json::from_slice(&output[..output.len() - 1]).unwrap();
            assert_eq!(response.status, patch_engine::PatchStatus::Rejected);
            assert_eq!(response.failure.unwrap().code, code);
        }
    }

    #[test]
    fn helper_io_charges_only_directories_the_patch_may_create() {
        let root = tempfile::tempdir().unwrap();
        let mut patch = String::from("*** Begin Patch\n");
        for index in 0..130 {
            let parent = format!("tree-{index:03}/one/two/three/four");
            std::fs::create_dir_all(root.path().join(&parent)).unwrap();
            writeln!(patch, "*** Add File: {parent}/file.txt\n+value").unwrap();
        }
        patch.push_str("*** End Patch\n");

        let mut output = Vec::new();
        assert_eq!(
            run_apply_patch_helper_io(root.path(), patch.as_bytes(), &mut output),
            0
        );
        let response: patch_engine::PatchHelperResponse =
            serde_json::from_slice(&output[..output.len() - 1]).unwrap();

        assert_eq!(response.status, patch_engine::PatchStatus::Applied);
        assert_eq!(response.effects.len(), 130);
        assert!(
            response
                .effects
                .iter()
                .all(|effect| effect.kind == patch_engine::PatchEffectKind::Add)
        );
    }
}
