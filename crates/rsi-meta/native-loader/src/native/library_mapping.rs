use libloading::Library;
use std::path::Path;

/// # Safety
/// The caller trusts execution of the selected absolute artifact and retains
/// its pinned bytes for the mapping lifetime. Its native dependencies are also
/// trusted process code; this function does not authenticate dependency bytes.
pub(super) unsafe fn load_library(path: &Path) -> Result<Library, libloading::Error> {
    #[cfg(windows)]
    {
        use libloading::os::windows::{
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
            Library as NativeLibrary,
        };
        // SAFETY: The caller supplies the trusted absolute pinned artifact.
        // These per-load flags do not change process-wide search configuration.
        unsafe {
            NativeLibrary::load_with_flags(
                path,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        }
        .map(Into::into)
    }
    #[cfg(not(windows))]
    {
        // SAFETY: The caller supplies and retains the trusted pinned artifact.
        unsafe { Library::new(path) }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::process::Command;

    fn build(source: &Path, output: &Path, name: &str, dependency: Option<&Path>) {
        let mut command = Command::new("rustc");
        command.args([
            "--edition=2024",
            "--crate-type=cdylib",
            "--crate-name",
            name,
        ]);
        command.arg(source).arg("--out-dir").arg(output);
        if let Some(dependency) = dependency {
            command.arg("-L").arg(dependency);
        }
        assert!(
            command
                .status()
                .expect("compile native DLL fixture")
                .success()
        );
    }

    #[test]
    fn windows_dependencies_load_only_from_dll_directory_and_system32() {
        let fixture = tempfile::tempdir().unwrap();
        let dependency = fixture.path().join("dependency");
        let plugin = fixture.path().join("plugin");
        std::fs::create_dir(&dependency).unwrap();
        std::fs::create_dir(&plugin).unwrap();
        let source = fixture.path().join("dependency.rs");
        std::fs::write(
            &source,
            "#[unsafe(no_mangle)] pub extern \"C\" fn rsi_dependency_value() -> u32 { 42 }",
        )
        .unwrap();
        build(&source, &dependency, "rsi_search_dependency", None);
        let source = fixture.path().join("plugin.rs");
        std::fs::write(
            &source,
            r#"#[link(name="rsi_search_dependency.dll")]
unsafe extern "C" { fn rsi_dependency_value() -> u32; }
#[unsafe(no_mangle)] pub unsafe extern "C" fn rsi_search_value() -> u32 {
    unsafe { rsi_dependency_value() }
}"#,
        )
        .unwrap();
        build(&source, &plugin, "rsi_search_plugin", Some(&dependency));
        let artifact = plugin.join("rsi_search_plugin.dll");

        // A child gives each load a fresh module table and isolates PATH/CWD;
        // the parent process and concurrent tests keep their original state.
        for adjacent in [false, true] {
            if adjacent {
                std::fs::copy(
                    dependency.join("rsi_search_dependency.dll"),
                    plugin.join("rsi_search_dependency.dll"),
                )
                .unwrap();
            }
            let result = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "native::library_mapping::tests::windows_dependency_load_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env("RSI_LOADER_FIXTURE", &artifact)
                .env(
                    "RSI_LOADER_EXPECT_LOAD",
                    if adjacent { "yes" } else { "no" },
                )
                .env("PATH", &dependency)
                .current_dir(&dependency)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "adjacent={adjacent}: {}{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
        }
    }

    #[test]
    #[ignore = "subprocess harness launched by the dependency-search test"]
    fn windows_dependency_load_child() {
        let path = std::env::var_os("RSI_LOADER_FIXTURE").expect("parent-provided DLL fixture");
        // SAFETY: The test parent compiled these trusted fixture bytes and
        // retains the TempDir until the child exits. No untrusted DLL executes.
        let result = unsafe { load_library(Path::new(&path)) };
        if std::env::var_os("RSI_LOADER_EXPECT_LOAD").as_deref() == Some("yes".as_ref()) {
            let library = result.expect("adjacent dependency must load");
            // SAFETY: The fixture exports this exact ABI and symbol. The
            // Library remains alive through symbol use and the synchronous call.
            let function =
                unsafe { library.get::<unsafe extern "C" fn() -> u32>(b"rsi_search_value\0") }
                    .unwrap();
            // SAFETY: The trusted fixture takes no pointers and returns u32.
            assert_eq!(unsafe { function() }, 42);
        } else {
            assert!(result.is_err(), "CWD/PATH-only dependency was admitted");
        }
    }
}
