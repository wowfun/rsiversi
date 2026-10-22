//! Dedicated Worker adapter for the shared Rust GUI application.
#![deny(unsafe_code)]
#![warn(missing_docs)]

#[cfg(target_arch = "wasm32")]
mod assets;
#[cfg(target_arch = "wasm32")]
mod worker;

/// Internal Web bundle identity checked before Profile activation.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn build_family() -> String {
    option_env!("RSI_WEB_BUNDLE_ID")
        .unwrap_or_default()
        .to_owned()
}
