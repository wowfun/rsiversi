mod data;
mod native_bridge;
mod options;
mod save;

use native_bridge::Bridge;
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tauri::Manager as _;
use tokio::io::AsyncReadExt as _;

fn error(value: impl std::fmt::Display) -> String {
    value.to_string()
}
fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            2
        }
    };
    std::process::exit(code);
}
fn run() -> Result<i32, String> {
    let Some(options) = options::parse(std::env::args_os().skip(1))? else {
        println!("{}", options::HELP);
        return Ok(0);
    };
    #[cfg(windows)]
    options::require_webview(tauri::webview_version())?;
    let _package = options.pin_package()?;
    let managed = options.managed();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(error)?;
    tauri::async_runtime::set(runtime.handle().clone());
    let (mut stream, launch) = prepare_launch(&options, &runtime)?;
    let mut root = launch.clone();
    root.set_fragment(None);
    let data = data::prepare(options.paths.state())?;
    let owner = Arc::new(Bridge::new(root.clone(), runtime.handle().clone()));
    let app = create_window(root, launch, data, owner.clone())?;
    let control = owner.clone();
    let listener_app = app.handle().clone();
    let listener = runtime.spawn(async move {
        tokio::select! { biased;
            () = control.stop.cancelled() => {},
            () = async {
                if let Some(stream) = &mut stream { wait_for_listener_end(stream).await; }
                else { std::future::pending::<()>().await; }
            } => {
                eprintln!("desktop: Host listener connection closed");
                control.failed.store(true, Ordering::Release); control.request_close(&listener_app);
            },
        }
        drop(stream);
    });
    let closing = owner.clone();
    let handle = app.handle().clone();
    let cleanup = runtime.spawn(async move {
        closing.stop.cancelled().await;
        closing.drain().await;
        closing.finished.cancel();
        handle.exit(i32::from(closing.failed.load(Ordering::Acquire)));
    });
    let signals = owner.clone();
    let handle = app.handle().clone();
    let signal_task = runtime.spawn(async move {
        tokio::select! { biased;
            () = signals.stop.cancelled() => {},
            result = tokio::signal::ctrl_c() => if result.is_ok() { signals.request_close(&handle); },
        }
    });
    let managed_owner = owner.clone();
    let managed_app = app.handle().clone();
    let parent_thread = managed
        .then(|| {
            std::thread::Builder::new()
                .name("launcher-control".into())
                .spawn(move || {
                    use std::io::Read as _;
                    let mut byte = [0];
                    let _ = std::io::stdin().read(&mut byte);
                    managed_owner.request_close(&managed_app);
                })
        })
        .transpose()
        .map_err(error)?;
    let events = owner.clone();
    let exit = app.run_return(move |app, event| match event {
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::CloseRequested { api, .. },
            ..
        } if label == "main" && !events.finished.is_cancelled() => {
            api.prevent_close();
            events.request_close(app);
        }
        tauri::RunEvent::ExitRequested { api, .. } if !events.finished.is_cancelled() => {
            api.prevent_exit();
            events.request_close(app);
        }
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::Destroyed,
            ..
        } if label == "main" => events.stop.cancel(),
        _ => {}
    });
    owner.stop.cancel();
    runtime.block_on(async {
        cleanup.await.map_err(error)?;
        listener.await.map_err(error)?;
        signal_task.await.map_err(error)
    })?;
    // Standard input has no portable cancellable read. This observer owns no
    // mutation or Tokio blocking task; process exit closes its pipe after all
    // admitted native persistence and listener cleanup has settled.
    if let Some(thread) = parent_thread
        && thread.is_finished()
    {
        thread.join().map_err(|_| "Launcher observer failed")?;
    }
    let status = exit.max(i32::from(owner.failed.load(Ordering::Acquire)));
    eprintln!("desktop: cleanup completed with status {status}");
    Ok(status)
}

type Launch = (Option<Box<dyn rsi_service_host::LocalStream>>, tauri::Url);
fn prepare_launch(
    options: &options::Options,
    runtime: &tokio::runtime::Runtime,
) -> Result<Launch, String> {
    let result = if options.frontend {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let input = std::thread::spawn(move || {
            let _ = sender.send(rsi_web_control::DesktopLaunch::read(
                &mut std::io::stdin().lock(),
            ));
        });
        let launch = receiver
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| "Desktop launch handoff deadline elapsed")?
            .map_err(error)?;
        input
            .join()
            .map_err(|_| "Desktop launch input worker failed")?;
        (None, tauri::Url::parse(&launch.url).map_err(error)?)
    } else {
        runtime.block_on(async {
            let paths = rsi_service_host::ServiceHostPaths::from_host_paths(&options.paths)
                .map_err(error)?;
            let owner = paths
                .read_metadata()
                .map_err(error)?
                .ok_or("No active Host; start Desktop through rsi desktop")?;
            let mut stream = rsi_service_host::connect_local_service(&owner, "web")
                .await
                .map_err(error)?;
            rsi_web_control::encode_frame(
                &mut stream,
                &rsi_web_control::Request {
                    epoch: owner.host_epoch.clone(),
                    protocol: rsi_web_control::protocol_descriptor(),
                    command: rsi_web_control::Command::Desktop {
                        assets: options.assets.clone(),
                    },
                },
            )
            .await
            .map_err(error)?;
            let reply = tokio::time::timeout(
                Duration::from_secs(30),
                rsi_web_control::decode_frame::<rsi_web_control::Reply>(&mut stream),
            )
            .await
            .map_err(error)?
            .map_err(error)?;
            match reply {
                rsi_web_control::Reply::Ready {
                    address,
                    ticket: Some(ticket),
                } if address.ip().is_loopback() => Ok((
                    Some(stream),
                    tauri::Url::parse(&format!("http://{address}/#rsi-launch={ticket}"))
                        .map_err(error)?,
                )),
                rsi_web_control::Reply::Error { message } => Err(message),
                _ => Err("Host did not publish a Desktop listener".into()),
            }
        })?
    };
    Ok(result)
}

fn create_window(
    root: tauri::Url,
    launch: tauri::Url,
    data: std::path::PathBuf,
    owner: Arc<Bridge>,
) -> Result<tauri::App<tauri::Wry>, String> {
    let setup_owner = owner.clone();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(owner)
        .invoke_handler(tauri::generate_handler![native_bridge::rsi_bridge])
        .setup(move |app| {
            // Capabilities admit the Origin; mandatory preview CSP isolates child frames.
            app.add_capability(serde_json::json!({
                "identifier":"desktop-main-document", "local":false,
                "remote":{"urls":[root.as_str()]}, "windows":["main"],
                "permissions":["allow-rsi-bridge"]
            }).to_string())?;
            tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::External(launch))
                .on_navigation(move |url| native_bridge::admits_navigation(&setup_owner.origin, url))
                .initialization_script("if(window===window.top)Object.defineProperty(window,'__RSI_DESKTOP__',{value:true});")
                .title("RSIversi").inner_size(1440.0, 980.0).data_directory(data).build()?;
            Ok(())
        }).build(tauri::generate_context!()).map_err(error)
}

// Lease traffic carries no product messages after readiness. Only EOF or an I/O
// failure retires the attachment; unexpected bytes cannot bypass document drain.
async fn wait_for_listener_end(stream: &mut (impl tokio::io::AsyncRead + Unpin)) {
    let mut bytes = [0; 256];
    loop {
        match stream.read(&mut bytes).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
    }
}
#[cfg(test)]
mod listener_tests {
    #[tokio::test]
    async fn listener_noise_does_not_signal_loss_but_eof_does() {
        use tokio::io::AsyncWriteExt as _;
        let (mut reader, mut writer) = tokio::io::duplex(16);
        writer.write_all(b"noise").await.unwrap();
        let wait = super::wait_for_listener_end(&mut reader);
        tokio::pin!(wait);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut wait)
                .await
                .is_err()
        );
        drop(writer);
        tokio::time::timeout(std::time::Duration::from_secs(1), wait)
            .await
            .unwrap();
    }
}
