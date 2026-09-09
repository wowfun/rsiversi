#[cfg(target_os = "linux")]
use super::standard_coding_tools;
use super::{
    ApplicationInvocation, ProfileCatalog, RsiError, capture_standard_environment,
    profile_management_error, standard_paths,
};

pub(super) async fn run_application(invocation: ApplicationInvocation) -> u8 {
    if invocation
        .arguments
        .iter()
        .take_while(|argument| argument.to_str() != Some("--"))
        .any(|argument| matches!(argument.to_str(), Some("-h" | "--help")))
    {
        print!(
            "{}\n{}\n{}\n{}\n{}",
            rsi_terminal::HELP,
            rsi_serve::HELP,
            rsi_terminal::DEVICES_HELP,
            rsi_terminal::INSPECTOR_HELP,
            rsi_terminal::NATIVE_ADDONS_HELP
        );
        return 0;
    }
    match start(invocation).await {
        Ok(exit) => exit,
        Err(error) => report_error(&error),
    }
}

async fn start(invocation: ApplicationInvocation) -> rsi::Result<u8> {
    let paths = standard_paths()?;
    let profile = ProfileCatalog::new(paths.clone())
        .application(&invocation.profile)
        .map_err(profile_management_error)?;
    #[cfg(target_os = "linux")]
    let coding = standard_coding_tools()?;
    #[cfg(not(target_os = "linux"))]
    let coding = None;
    let (host, diagnostics) = rsi::standard_application_host(
        rsi::StandardComposition::new(paths, capture_standard_environment()?, coding),
        invocation.arguments,
    )?;
    let program = profile
        .program()
        .map_err(|error| RsiError::Boot(error.to_string()))?;
    let running = host.start_program(program).await.map_err(|error| {
        diagnostics
            .take()
            .unwrap_or_else(|| RsiError::Boot(error.to_string()))
    })?;
    let result = match running.lookup_local::<rsi_application::ApplicationRunContract>() {
        Some(application) => application
            .run()
            .await
            .map_err(|error| RsiError::Run(error.to_string())),
        None => Err(RsiError::Boot(
            "Application Profile did not publish an entry point".into(),
        )),
    };
    let cleanup = running.shutdown().await;
    if !cleanup.is_clean() {
        return Err(RsiError::Run("application Runtime cleanup failed".into()));
    }
    result
}

pub(super) fn report_error(error: &RsiError) -> u8 {
    eprintln!("error: {error}");
    error.exit_code()
}
