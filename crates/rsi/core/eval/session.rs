//! Linux-only external-oracle driver over the real standard local Session API.
#![forbid(unsafe_code)]

#[cfg(target_os = "linux")]
mod driver;

fn main() -> std::process::ExitCode {
    if let Some(exit) = rsi::maybe_run_apply_patch_helper(std::env::args_os().skip(1)) {
        return std::process::ExitCode::from(exit);
    }
    #[cfg(target_os = "linux")]
    {
        match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime.block_on(driver::main()),
            Err(error) => {
                eprintln!("evaluation Runtime: {error}");
                std::process::ExitCode::FAILURE
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("Session API evaluation requires Linux namespace supervision");
        std::process::ExitCode::FAILURE
    }
}
