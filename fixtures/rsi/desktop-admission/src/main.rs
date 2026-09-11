use serde::{Deserialize, Serialize};
use std::sync::{Arc, atomic::{AtomicBool, AtomicI32, Ordering}};
use tauri::Manager;

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Probe {
    Number { integer: u64, fraction: f64, nested: serde_json::Value },
}

#[tauri::command]
fn echo(input: Probe) -> Probe { input }

#[tauri::command]
fn exact(json: String) -> Result<String, String> {
    let value: serde_json::Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    serde_json::to_string(&value).map_err(|e| e.to_string())
}

#[tauri::command]
fn finish(app: tauri::AppHandle, report: serde_json::Value, state: tauri::State<'_, Arc<AtomicI32>>) {
    let success = report.get("ok").and_then(serde_json::Value::as_bool) == Some(true);
    state.store(if success { 0 } else { 1 }, Ordering::SeqCst);
    let text = serde_json::to_string_pretty(&report).expect("report JSON");
    if let Some(path) = std::env::var_os("RSI_ADMISSION_REPORT") {
        if let Err(error) = std::fs::write(path, &text) { eprintln!("report: {error}"); state.store(1, Ordering::SeqCst); }
    }
    println!("{text}");
    if std::env::var("RSI_ADMISSION_AUTOMATION").as_deref() != Ok("true") { app.exit(state.load(Ordering::SeqCst)); }
}

#[tauri::command]
fn request_exit(app: tauri::AppHandle) { app.exit(0); }

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("Tokio");
    tauri::async_runtime::set(runtime.handle().clone());
    let data = tempfile::tempdir().expect("private WebView data");
    let status = Arc::new(AtomicI32::new(2));
    let result = tauri::Builder::default()
        .manage(status.clone())
        .invoke_handler(tauri::generate_handler![echo, exact, finish, request_exit])
        .register_asynchronous_uri_scheme_protocol("probe", |_context, _request, responder| {
            responder.respond(tauri::http::Response::builder()
                .header("Content-Type", "application/octet-stream")
                .header("Access-Control-Allow-Origin", "tauri://localhost")
                .body(vec![0_u8, 1, 127, 128, 255]).expect("raw response"));
        })
        .setup(move |app| {
            tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::App("index.html".into()))
                .title("RSI platform admission").inner_size(1280.0, 840.0)
                .data_directory(data.path().to_path_buf()).build()?;
            app.manage(data);
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                eprintln!("admission deadline exceeded"); handle.exit(2);
            });
            Ok(())
        })
        .build(tauri::generate_context!()).expect("Tauri build");
    let main_thread = std::thread::current().id();
    let stopping = Arc::new(AtomicBool::new(false));
    let drained = Arc::new(AtomicBool::new(false));
    let exit = result.run_return(move |app, event| {
        assert_eq!(std::thread::current().id(), main_thread, "platform events must run on the main thread");
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            if !drained.load(Ordering::SeqCst) {
                api.prevent_exit();
                if !stopping.swap(true, Ordering::SeqCst) {
                    let app = app.clone(); let drained = drained.clone();
                    tauri::async_runtime::spawn(async move {
                        tokio::task::yield_now().await;
                        drained.store(true, Ordering::SeqCst);
                        eprintln!("admission: owned async work drained");
                        app.exit(0);
                    });
                }
            } else { eprintln!("admission: main-thread exit after drain"); }
        }
    });
    std::process::exit(if exit != 0 { exit } else { status.load(Ordering::SeqCst) });
}
