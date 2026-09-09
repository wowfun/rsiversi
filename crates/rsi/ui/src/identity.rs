/// Allocates a fresh opaque application or request identity in native or Worker execution.
/// The fixed prefix is at most 32 ASCII letters, digits, hyphens or underscores.
/// These identities are staleness labels, not authentication credentials.
pub fn fresh_identity(prefix: &str) -> Result<String, String> {
    if prefix.is_empty()
        || prefix.len() > 32
        || !prefix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
    {
        return Err("Invalid application identity prefix".into());
    }
    let mut bytes = [0_u8; 16];
    #[cfg(not(target_arch = "wasm32"))]
    getrandom::fill(&mut bytes).map_err(|_| "Cannot allocate a fresh identity".to_owned())?;
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast;
        let worker: web_sys::WorkerGlobalScope = js_sys::global()
            .dyn_into()
            .map_err(|_| "A Dedicated Worker is required".to_owned())?;
        worker
            .crypto()
            .map_err(|_| "Worker entropy is unavailable".to_owned())?
            .get_random_values_with_u8_array(&mut bytes)
            .map_err(|_| "Worker entropy failed".to_owned())?;
    }
    Ok(format!("{prefix}-{}", hex::encode(bytes)))
}
