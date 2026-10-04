#![forbid(unsafe_code)]

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> std::process::ExitCode {
    let arguments = std::env::args_os().skip(1).take(5).collect::<Vec<_>>();
    if let Some(code) = rsi_apply_patch::maybe_run_apply_patch_helper(arguments.clone()) {
        return std::process::ExitCode::from(code);
    }
    if let Some(code) = rsi_agent_workspace_context::maybe_run_project_context_helper(&arguments) {
        return std::process::ExitCode::from(code);
    }
    if let Some(code) = rsi_directory_picker::maybe_run_directory_picker_helper(&arguments) {
        return std::process::ExitCode::from(code);
    }
    let result = match rsi_ssh_helper::Invocation::parse(&arguments) {
        Ok(invocation) => invocation.run().await,
        Err(error) => Err(error),
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("SSH helper: {error}");
            if error == rsi_ssh_helper::HelperError::CacheContentionTimeout {
                std::process::ExitCode::from(rsi_ssh_protocol::CACHE_CONTENTION_EXIT_CODE)
            } else {
                std::process::ExitCode::FAILURE
            }
        }
    }
}
#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("SSH helper requires Linux");
    std::process::ExitCode::FAILURE
}
