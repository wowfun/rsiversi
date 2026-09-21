use rsi_language_addon_example::{host, program};
use std::io::Read as _;
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let config = arguments.next().ok_or("explicit server config required")?;
    let workspace = arguments.next().ok_or("explicit workspace required")?;
    let query = arguments.next().ok_or("explicit query JSON required")?;
    let wait = match arguments.next().as_deref().and_then(|s| s.to_str()) {
        None => false,
        Some("--wait-for-result") => true,
        _ => return Err("unexpected argument".into()),
    };
    if arguments.next().is_some() {
        return Err("unexpected argument".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(config)?
        .take(65537)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        return Err("server config exceeds bound".into());
    }
    let config = serde_json::from_slice(&bytes)?;
    let workspace = std::fs::canonicalize(workspace)?;
    let query = query.to_str().ok_or("query must be UTF-8")?;
    if query.len() > 8192 {
        return Err("query exceeds bound".into());
    }
    let query: rsi_lsp::Query = serde_json::from_str(query)?;
    let running = host()?.start_program(program(config)).await?;
    let result = match running.lookup_local::<rsi_lsp::LanguageContract>() {
        Some(service) => {
            let started = std::time::Instant::now();
            loop {
                let result = service
                    .query(
                        workspace.clone(),
                        query.clone(),
                        tokio_util::sync::CancellationToken::new(),
                    )
                    .await
                    .map_err(|e| e.to_string());
                let empty = match &result {
                    Ok(output) => match &output.result {
                        rsi_lsp::QueryResult::Locations { locations } => locations.is_empty(),
                        rsi_lsp::QueryResult::Hover { text, .. } => text.is_empty(),
                    },
                    Err(_) => false,
                };
                if !wait || !empty || started.elapsed() >= std::time::Duration::from_secs(10) {
                    break result;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
        None => Err("language service unavailable".into()),
    };
    let cleanup = running.shutdown().await;
    if !cleanup.is_clean() {
        return Err("language addon did not cleanly stop".into());
    }
    println!("{}", serde_json::to_string(&result?)?);
    Ok(())
}
