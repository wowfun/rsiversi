pub(crate) fn allocate(prefix: &str) -> Result<String, String> {
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
