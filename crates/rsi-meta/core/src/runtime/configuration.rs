#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::*;

impl Runtime {
    pub(super) fn normalize_config(
        factory: &Arc<dyn PluginFactory>,
        config: ConfigValue,
        maximum_bytes: usize,
    ) -> Result<ConfigValue> {
        Self::validate_config_size(&config, maximum_bytes)?;
        let config = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            factory.validate_config(config)
        }))
        .map_err(|_| MetaError::InvalidConfig("plugin validation panicked".to_owned()))??;
        Self::validate_config_size(&config, maximum_bytes)?;
        Ok(config)
    }

    fn validate_config_size(config: &ConfigValue, maximum_bytes: usize) -> Result<()> {
        let encoded_bytes = encoded_json_size(config)
            .map_err(|error| MetaError::InvalidConfig(error.to_string()))?;
        if encoded_bytes > maximum_bytes {
            return Err(MetaError::InvalidConfig(format!(
                "canonical encoding exceeds the configured {maximum_bytes}-byte limit"
            )));
        }
        Ok(())
    }
}

pub(super) fn encoded_json_size<T: Serialize + ?Sized>(value: &T) -> serde_json::Result<usize> {
    struct Counter(usize);

    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| std::io::Error::other("JSON size overflowed"))?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.0)
}
