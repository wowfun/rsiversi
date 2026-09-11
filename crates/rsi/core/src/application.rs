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
    let program = profile
        .program()
        .map_err(|error| RsiError::Boot(error.to_string()))?;
    let running = rsi::start_application(
        rsi::StandardComposition::new(paths, capture_standard_environment()?, coding),
        invocation.arguments,
        program,
    )
    .await?;
    rsi_application::ApplicationLifetime::default()
        .run(&running)
        .await
        .map_err(|error| match error {
            rsi_application::ApplicationError::MissingEntry => RsiError::Boot(error.to_string()),
            _ => RsiError::Run(error.to_string()),
        })
}

pub(super) fn report_error(error: &RsiError) -> u8 {
    eprintln!("error: {error}");
    error.exit_code()
}
