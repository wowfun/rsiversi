use serde_json::Value;

#[derive(Clone, Debug)]
pub(crate) struct InterceptLayers {
    pub(super) values: Vec<Value>,
    pub(super) encoded_bytes: usize,
}

impl InterceptLayers {
    pub(crate) fn empty() -> Self {
        Self {
            values: Vec::new(),
            encoded_bytes: 2,
        }
    }

    pub(crate) fn as_slice(&self) -> &[Value] {
        &self.values
    }
}
