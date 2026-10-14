//! Opt-in Windows SDK transport evidence with an isolated, hidden document.
use std::sync::{Arc, Mutex};
use tauri::Manager as _;

#[derive(Default)]
struct Evidence {
    requests: Vec<serde_json::Value>,
    result: Option<serde_json::Value>,
}

#[test]
#[ignore = "opt-in live WebView2 probe; creates a hidden window and temporary browser profile"]
fn webview2_origin_methods_and_document_ipc_permissions() {
    let profile = tempfile::tempdir().unwrap();
    let evidence = Arc::new(Mutex::new(Evidence::default()));
    let app_slot = Arc::new(Mutex::new(None::<tauri::AppHandle>));
    let app = build_probe(
        profile.path().to_path_buf(),
        evidence.clone(),
        app_slot.clone(),
    );
    *app_slot.lock().unwrap() = Some(app.handle().clone());
    let (stop, stopping) = std::sync::mpsc::channel();
    let watchdog_handle = app.handle().clone();
    let watchdog = std::thread::spawn(move || {
        if stopping
            .recv_timeout(std::time::Duration::from_secs(15))
            .is_err()
        {
            watchdog_handle.exit(2);
        }
    });
    let exit_code = app.run_return(|_, _| {});
    // Break the callback-to-manager handle cycle before dropping the profile.
    app_slot.lock().unwrap().take();
    let _ = stop.send(());
    watchdog.join().unwrap();
    let evidence = evidence.lock().unwrap();
    println!(
        "{}",
        serde_json::json!({"requests": evidence.requests, "result": evidence.result})
    );
    cleanup_profile(profile);
    assert_eq!(exit_code, 0, "WebView2 probe finished before its deadline");
    let result = evidence
        .result
        .as_ref()
        .expect("WebView2 probe completed within its deadline");
    assert_eq!(result["post"], 200);
    assert_eq!(result["get"], 400);
    assert_eq!(result["native_mode"], true);
    assert_eq!(result["close_events"], 1);
    assert_eq!(result["inline_blocked"], true);
    for permission in ["dialog", "clipboard"] {
        assert!(
            result[permission].as_str().unwrap().contains("not allowed"),
            "{result}"
        );
    }
    let post = evidence
        .requests
        .iter()
        .find(|request| request["method"] == "POST")
        .unwrap();
    assert_eq!(post["uri"], "rsi://localhost/_frame");
    assert_eq!(post["origin"], crate::document::ORIGIN);
    assert_eq!(post["admitted"], true);
}

fn cleanup_profile(profile: tempfile::TempDir) {
    let path = profile.keep();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match std::fs::remove_dir_all(&path) {
            Ok(()) => return,
            Err(error)
                if matches!(error.raw_os_error(), Some(32 | 33))
                    && std::time::Instant::now() < deadline =>
            {
                // WebView2 releases sharing locks after controller destruction.
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(error) => panic!(
                "WebView2 profile cleanup failed at {}: {error}",
                path.display()
            ),
        }
    }
}

fn build_probe(
    profile: std::path::PathBuf,
    evidence: Arc<Mutex<Evidence>>,
    app_slot: Arc<Mutex<Option<tauri::AppHandle>>>,
) -> tauri::App {
    let record = evidence;
    let completion = app_slot;
    tauri::Builder::default()
        .any_thread()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .on_window_event(|window, event| {
            if matches!(event, tauri::WindowEvent::Destroyed) {
                window.app_handle().exit(0);
            }
        })
        .register_asynchronous_uri_scheme_protocol("rsi", move |context, request, responder| {
            let path = request.uri().path();
            let admitted = super::validate_request(context.webview_label(), &request).is_ok();
            let mut record = record.lock().unwrap();
            if path == "/_frame" {
                record.requests.push(serde_json::json!({
                    "uri": request.uri().to_string(), "method": request.method().as_str(),
                    "origin": request.headers().get("origin").and_then(|value| value.to_str().ok()),
                    "admitted": admitted
                }));
            }
            let finished = path == "/_probe_done" && admitted;
            if finished {
                assert!(request.body().len() <= 16384);
                record.result = Some(serde_json::from_slice(request.body()).unwrap());
            }
            drop(record);
            let (body, mime) = match path {
                "/" => (b"<!doctype html><title>Native transport probe</title><script>globalThis.__inlineRan=true</script><script type='module' src='/probe.js'></script>".to_vec(), "text/html"),
                "/probe.js" => (SCRIPT.as_bytes().to_vec(), "text/javascript"),
                "/document-mode.js" => (include_str!("../../web/document-mode.js").as_bytes().to_vec(), "text/javascript"),
                _ => (b"{}".to_vec(), "application/json"),
            };
            responder.respond(
                tauri::http::Response::builder()
                    .status(if admitted { 200 } else { 400 })
                    .header(
                        "Content-Type",
                        mime,
                    )
                    .header("Content-Security-Policy", rsi_api_http::DocumentPolicy::default()
                        .content_security_policy(crate::document::ORIGIN).unwrap())
                    .body(body)
                    .unwrap(),
            );
            if finished && let Some(app) = completion.lock().unwrap().as_ref() {
                app.get_webview_window("main").unwrap().destroy().unwrap();
            }
        })
        .setup(move |app| {
            tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::External(crate::document::root_url()),
            )
            .visible(false)
            .data_directory(profile)
            .on_page_load(|view, event| {
                if event.event() == tauri::webview::PageLoadEvent::Finished {
                    view.eval(crate::document::REQUEST_CLOSE).unwrap();
                }
            })
            .build()?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .unwrap()
}

const SCRIPT: &str = r"import {isNativeDocument} from '/document-mode.js';
let close_events = 0;
window.addEventListener('rsi-native-close', () => { close_events++; });
(async () => {
    const post = await fetch('/_frame', {method:'POST'});
    const get = await fetch('/_frame');
    const denied = async (command, args) => {
        try { await window.__TAURI_INTERNALS__.invoke(command, args); return 'unexpected admission'; }
        catch (error) { return String(error); }
    };
    // Malformed arguments cannot invoke a dialog or modify the clipboard even
    // if an unexpected ACL admits the command; expected rejection precedes decode.
    const dialog = await denied('plugin:dialog|open', {options:'invalid options'});
    const clipboard = await denied('plugin:clipboard-manager|write_text', {text:{invalid:true}});
    const native_mode = isNativeDocument(location), inline_blocked = globalThis.__inlineRan !== true;
    await fetch('/_probe_done', {method:'POST',body:JSON.stringify({post:post.status,get:get.status,dialog,clipboard,native_mode,close_events,inline_blocked})});
})()";
