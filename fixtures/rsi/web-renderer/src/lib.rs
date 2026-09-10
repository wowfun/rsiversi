//! Actual Rust/WASM DOM presentation, with no business runtime.
use std::cell::Cell;
use wasm_bindgen::prelude::*;
use web_sys::Element;
thread_local! { static ACTIVE: Cell<u32> = const { Cell::new(0) }; }

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Data {
    label: String,
    count: u32,
}
fn decode(source: &str) -> Result<(Data, bool, bool), JsValue> {
    let invalid = || JsValue::from_str("invalid Rust renderer model");
    if source.len() > rsi_ui_protocol::MAXIMUM_VIEW_BYTES {
        return Err(invalid());
    }
    let model: rsi_ui_protocol::UiModel = serde_json::from_str(source).map_err(|_| invalid())?;
    model.validate().map_err(|_| invalid())?;
    if model.renderer != "fixture.rust"
        || model.schema.name != "fixture.counter"
        || model.schema.version != 1
        || model.actions.iter().any(|action| action.name != "refresh")
        || model.sources.iter().any(|source| source.name != "raw")
    {
        return Err(invalid());
    }
    let data: Data = serde_json::from_value(model.data).map_err(|_| invalid())?;
    if data.label.len() > 1024 || data.count > 1_000_000 {
        return Err(invalid());
    }
    Ok((data, !model.actions.is_empty(), !model.sources.is_empty()))
}
/// A mounted document resource. Dropping it removes the nodes it owns.
#[wasm_bindgen]
#[derive(Debug)]
pub struct Renderer {
    root: Element,
    title: Element,
    counter: Element,
    controls: Vec<Element>,
    action: bool,
    source: bool,
}
#[wasm_bindgen]
impl Renderer {
    /// Validates the model before touching the DOM.
    #[wasm_bindgen(constructor)]
    pub fn new(root: Element, source: &str) -> Result<Self, JsValue> {
        let (data, action, source) = decode(source)?;
        let document = root
            .owner_document()
            .ok_or_else(|| JsValue::from_str("missing document"))?;
        let title = document.create_element("h2")?;
        let counter = document.create_element("p")?;
        title.set_text_content(Some(&data.label));
        counter.set_text_content(Some(&format!("Rust/WASM count: {}", data.count)));
        let mut controls = Vec::new();
        if action {
            let button = document.create_element("button")?;
            button.set_attribute("data-fixture-action", "refresh")?;
            button.set_text_content(Some("Refresh native model"));
            controls.push(button);
        }
        if source {
            let button = document.create_element("button")?;
            button.set_attribute("data-fixture-action", "raw")?;
            button.set_text_content(Some("Read native bytes"));
            controls.push(button);
            let bytes = document.create_element("pre")?;
            bytes.set_attribute("data-fixture-bytes", "")?;
            controls.push(bytes);
        }
        ACTIVE.with(|active| active.set(active.get() + 1));
        let renderer = Self {
            root,
            title,
            counter,
            controls,
            action,
            source,
        };
        for node in [&renderer.title, &renderer.counter]
            .into_iter()
            .chain(&renderer.controls)
        {
            renderer.root.append_child(node)?;
        }
        Ok(renderer)
    }
    /// Keeps node identity while updating validated presentation data.
    pub fn update(&self, source: &str) -> Result<(), JsValue> {
        let (data, action, source) = decode(source)?;
        if self.action != action || self.source != source {
            return Err(JsValue::from_str("fixture model controls changed"));
        }
        self.title.set_text_content(Some(&data.label));
        self.counter
            .set_text_content(Some(&format!("Rust/WASM count: {}", data.count)));
        Ok(())
    }
}
impl Drop for Renderer {
    fn drop(&mut self) {
        self.title.remove();
        self.counter.remove();
        for node in &self.controls {
            node.remove();
        }
        // Retain the mount root only for this renderer's lifetime.
        self.root.set_text_content(None);
        ACTIVE.with(|active| active.set(active.get() - 1));
    }
}
/// Live DOM owners, independently of browser module-cache retention.
#[wasm_bindgen]
pub fn live_renderers() -> u32 {
    ACTIVE.with(Cell::get)
}
