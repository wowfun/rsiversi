#![allow(unsafe_code)] // Integration tests call the audited raw loader ABI.
#![allow(clippy::similar_names)] // `loader` and its `loaded` plugin are distinct test roles.

use std::ffi::c_void;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use libloading::Library;
use rsi_meta_loader::{
    BUILD_TARGET, ContentHash, ExpectedHashes, LoaderError, PluginLoader, PluginMailboxOptions,
};
use rsi_meta_plugin::{
    CallOutcome, HostApi, Lane, PLUGIN_ENTRY_SYMBOL, POST_FRAME_ACCEPTED, PluginEntryFn,
};
use tempfile::TempDir;

const BAD_ABI_MARKER: u32 = 0xBAD0_AB10;

#[derive(Debug)]
struct BuiltFixture {
    _root: TempDir,
    library: PathBuf,
}

fn build_fixture() -> &'static BuiltFixture {
    static FIXTURE: OnceLock<BuiltFixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let root = TempDir::new().unwrap();
        let crate_root = root.path().join("fixture-crate");
        fs::create_dir_all(crate_root.join("src")).unwrap();

        let plugin_crate = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("plugin");
        let plugin_path = plugin_crate.to_string_lossy().replace('\\', "\\\\");
        let cargo_toml = format!(
            r#"[package]
name = "rsi-meta-loader-fixture"
version = "0.0.0"
edition = "2024"
publish = false

[lib]
crate-type = ["cdylib"]

[dependencies]
rsi-meta-plugin = {{ path = "{plugin_path}" }}

[workspace]
"#
        );
        fs::write(crate_root.join("Cargo.toml"), cargo_toml).unwrap();
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/echo_plugin.rs"),
            crate_root.join("src/lib.rs"),
        )
        .unwrap();

        let target_dir = root.path().join("target");
        let status = Command::new(env!("CARGO"))
            .args(["build", "--quiet", "--offline"])
            .current_dir(&crate_root)
            .env("CARGO_TARGET_DIR", &target_dir)
            .status()
            .unwrap();
        assert!(status.success(), "failed to build loader cdylib fixture");

        let library = target_dir.join("debug").join(format!(
            "{}rsi_meta_loader_fixture{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_SUFFIX
        ));
        assert!(
            library.is_file(),
            "missing fixture at {}",
            library.display()
        );
        BuiltFixture {
            _root: root,
            library,
        }
    })
}

#[repr(C)]
#[derive(Debug)]
struct HostState {
    marker: AtomicU32,
    frames: Mutex<Vec<(u32, Vec<u8>)>>,
}

unsafe extern "C" fn post_frame(
    host_handle: *mut c_void,
    lane: u32,
    data_ptr: *const u8,
    data_len: usize,
) -> u32 {
    // SAFETY: The test retains an Arc to this mutex-backed state until every
    // LoadedPlugin using the handle has been dropped.
    let state = unsafe { &*host_handle.cast::<HostState>() };
    let payload = if data_len == 0 {
        &[]
    } else {
        // SAFETY: An ABI-conforming plugin supplies a readable borrowed frame.
        unsafe { std::slice::from_raw_parts(data_ptr, data_len) }
    };
    state.frames.lock().unwrap().push((lane, payload.to_vec()));
    POST_FRAME_ACCEPTED
}

fn write_package(root: &Path, fixture: &BuiltFixture) -> (PathBuf, ExpectedHashes) {
    let package = root.join("package");
    fs::create_dir_all(package.join("lib")).unwrap();
    let artifact_path = package
        .join("lib")
        .join(format!("echo{}", std::env::consts::DLL_SUFFIX));
    let artifact_bytes = fs::read(&fixture.library).unwrap();
    fs::write(&artifact_path, &artifact_bytes).unwrap();

    let manifest = format!(
        r#"format_version = 0

[package]
id = "fixture.echo"
version = "0.0.0"

[host_api]
major = 0
minimum_minor = 0

[[artifacts]]
target = "{BUILD_TARGET}"
path = "lib/echo{}"
"#,
        std::env::consts::DLL_SUFFIX
    );
    let manifest_path = package.join("plugin.toml");
    fs::write(&manifest_path, manifest.as_bytes()).unwrap();
    (
        manifest_path,
        ExpectedHashes::new(
            ContentHash::digest(manifest.as_bytes()),
            ContentHash::digest(artifact_bytes),
        ),
    )
}

unsafe fn next_residency_counter(path: &Path) -> u32 {
    // SAFETY: This fixture library is trusted test code and remains mapped for
    // the duration of the copied symbol invocation below.
    let library = unsafe { Library::new(path) }.unwrap();
    // SAFETY: The fixture defines this exact no-argument C function.
    let next = unsafe {
        library
            .get::<unsafe extern "C" fn() -> u32>(b"rsi_meta_fixture_next_counter\0")
            .unwrap()
    };
    // SAFETY: The function has no preconditions and the Library is still live.
    unsafe { next() }
}

unsafe fn bad_destroy_calls(path: &Path) -> u32 {
    // SAFETY: This fixture library is trusted test code and remains mapped for
    // the duration of the copied symbol invocation below.
    let library = unsafe { Library::new(path) }.unwrap();
    // SAFETY: The fixture defines this exact no-argument C function.
    let count = unsafe {
        library
            .get::<unsafe extern "C" fn() -> u32>(b"rsi_meta_fixture_bad_destroy_calls\0")
            .unwrap()
    };
    // SAFETY: The function has no preconditions and the Library is still live.
    unsafe { count() }
}

#[test]
fn real_cdylib_uses_v0_symbol_rejects_bad_abi_and_stays_resident() {
    let fixture = build_fixture();
    let temp = TempDir::new().unwrap();
    let (manifest_path, hashes) = write_package(temp.path(), fixture);
    let loader = PluginLoader::for_current_process(temp.path().join("cache"));
    let staged = loader.stage(manifest_path, hashes).unwrap();

    // SAFETY: The staged file was built above as this test's trusted cdylib.
    let direct = unsafe { Library::new(staged.cached_artifact_path()) }.unwrap();
    // SAFETY: PluginEntryFn is the ABI crate's exact symbol signature.
    assert!(unsafe { direct.get::<PluginEntryFn>(PLUGIN_ENTRY_SYMBOL) }.is_ok());
    // SAFETY: This lookup is diagnostic and does not invoke the symbol.
    assert!(unsafe { direct.get::<PluginEntryFn>(b"rsi_meta_plugin_entry\0") }.is_err());
    drop(direct);

    let state = Arc::new(HostState {
        marker: AtomicU32::new(0),
        frames: Mutex::new(Vec::new()),
    });
    let handle = Arc::as_ptr(&state).cast_mut().cast::<c_void>();
    // SAFETY: The Arc's mutex-backed context remains live and is safe for
    // concurrent calls until LoadedPlugin is dropped.
    let host = unsafe { HostApi::new(handle, post_frame) };
    // SAFETY: the boxed callback state outlives `loaded`, is thread-safe, and
    // `post_frame` copies every accepted payload without unwinding.
    let mut loaded = unsafe { loader.load(&staged, host) }.unwrap();
    assert_eq!(loaded.dispatch(Lane::Control, b"hello"), CallOutcome::Ok);
    assert_eq!(
        state.frames.lock().unwrap()[0],
        (Lane::Control.as_raw(), b"hello".to_vec())
    );
    assert_eq!(loaded.shutdown(), CallOutcome::Ok);

    // The first external handle closes here; the loader's deliberately leaked
    // handle must keep the fixture static alive across LoadedPlugin destruction.
    assert_eq!(
        // SAFETY: The test fixture exports this exact diagnostic symbol and
        // the copied call completes while its direct library handle is live.
        unsafe { next_residency_counter(staged.cached_artifact_path()) },
        1
    );
    drop(loaded);
    assert_eq!(
        // SAFETY: Same trusted diagnostic symbol and scoped direct handle.
        unsafe { next_residency_counter(staged.cached_artifact_path()) },
        2
    );

    // The same real entry point can return a table with the wrong ABI major.
    // Loader must reject it before exposing any callback.
    // SAFETY: Same live concurrent host context as above.
    state.marker.store(BAD_ABI_MARKER, Ordering::SeqCst);
    let bad_host = unsafe { HostApi::new(handle, post_frame) };
    assert!(matches!(
        // SAFETY: the callback state remains valid; this call deliberately
        // exercises only the incompatible table-version rejection.
        unsafe { loader.load(&staged, bad_host) },
        Err(LoaderError::IncompatiblePluginTable)
    ));
    // A rejected table has an unknown layout contract. In particular, the
    // loader must not trust and call its apparent destroy pointer.
    assert_eq!(
        // SAFETY: Same trusted diagnostic symbol and scoped direct handle.
        unsafe { bad_destroy_calls(staged.cached_artifact_path()) },
        0
    );
}

#[test]
fn queued_callback_copies_frames_and_keeps_control_capacity_independent() {
    let fixture = build_fixture();
    let temp = TempDir::new().unwrap();
    let (manifest_path, hashes) = write_package(temp.path(), fixture);
    let loader = PluginLoader::for_current_process(temp.path().join("cache"));
    let staged = loader.stage(manifest_path, hashes).unwrap();
    let (mut loaded, mut mailbox) = loader
        .load_queued(
            &staged,
            PluginMailboxOptions {
                control_capacity: 1,
                data_capacity: 1,
                max_frame_bytes: 32,
            },
        )
        .unwrap();

    let mut first = b"first".to_vec();
    assert_eq!(loaded.dispatch(Lane::Data, &first), CallOutcome::Ok);
    first.fill(b'x');
    // DATA is full, but an independently bounded control terminal still fits.
    assert_eq!(loaded.dispatch(Lane::Data, b"second"), CallOutcome::Ok);
    assert_eq!(loaded.dispatch(Lane::Control, b"terminal"), CallOutcome::Ok);

    assert_eq!(mailbox.try_recv_data().unwrap().payload(), b"first");
    assert!(mailbox.try_recv_data().is_err());
    assert_eq!(mailbox.try_recv_control().unwrap().payload(), b"terminal");

    // The fixture posts from a plugin-created thread; the host callback remains
    // callable there and still copies before returning.
    assert_eq!(
        loaded.dispatch(Lane::Control, b"thread:worker"),
        CallOutcome::Ok
    );
    assert_eq!(mailbox.try_recv_control().unwrap().payload(), b"worker");

    // Oversized output is rejected at the ABI adapter and never enters a lane.
    assert_eq!(loaded.dispatch(Lane::Control, &[0; 33]), CallOutcome::Ok);
    assert!(mailbox.try_recv_control().is_err());
}

#[tokio::test]
async fn mailbox_can_split_into_independently_awaited_fixed_lanes() {
    let fixture = build_fixture();
    let temp = TempDir::new().unwrap();
    let (manifest_path, hashes) = write_package(temp.path(), fixture);
    let loader = PluginLoader::for_current_process(temp.path().join("cache"));
    let staged = loader.stage(manifest_path, hashes).unwrap();
    let (mut loaded, mailbox) = loader
        .load_queued(&staged, PluginMailboxOptions::default())
        .unwrap();
    let (mut control, mut data) = mailbox.into_lanes();

    assert_eq!(control.lane(), Lane::Control);
    assert_eq!(data.lane(), Lane::Data);
    assert_eq!(
        loaded.dispatch(Lane::Data, b"service-event"),
        CallOutcome::Ok
    );
    assert_eq!(
        loaded.dispatch(Lane::Control, b"unsolicited:control-notification"),
        CallOutcome::Ok
    );

    let (control_frame, data_frame) = tokio::join!(control.recv(), data.recv());
    let control_frame = control_frame.unwrap();
    let data_frame = data_frame.unwrap();
    assert_eq!(control_frame.lane(), Lane::Control);
    assert_eq!(control_frame.payload(), b"control-notification");
    assert_eq!(data_frame.lane(), Lane::Data);
    assert_eq!(data_frame.payload(), b"service-event");
    assert!(control.try_recv().is_err());
    assert!(data.try_recv().is_err());
}
