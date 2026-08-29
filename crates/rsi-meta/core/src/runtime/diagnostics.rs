use crate::MetaError;

const TRUNCATION_SUFFIX: &str = " [truncated]";

fn truncate_utf8(value: &str, maximum: usize) -> &str {
    let mut end = value.len().min(maximum);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

pub(super) fn bound_owned(mut value: String, maximum: usize) -> String {
    if value.len() <= maximum {
        return value;
    }
    if maximum <= TRUNCATION_SUFFIX.len() {
        value.clear();
        value.push_str(truncate_utf8(TRUNCATION_SUFFIX, maximum));
        return value;
    }
    let prefix = truncate_utf8(&value, maximum - TRUNCATION_SUFFIX.len()).len();
    value.truncate(prefix);
    value.push_str(TRUNCATION_SUFFIX);
    value
}

pub(super) fn bound_formatted(arguments: std::fmt::Arguments<'_>, maximum: usize) -> String {
    struct Writer {
        value: String,
        maximum: usize,
        truncated: bool,
    }

    impl std::fmt::Write for Writer {
        fn write_str(&mut self, value: &str) -> std::fmt::Result {
            if self.truncated {
                return Ok(());
            }
            let remaining = self.maximum.saturating_sub(self.value.len());
            let retained = truncate_utf8(value, remaining);
            self.value.push_str(retained);
            self.truncated |= retained.len() != value.len();
            Ok(())
        }
    }

    let mut writer = Writer {
        value: String::new(),
        maximum,
        truncated: false,
    };
    std::fmt::write(&mut writer, arguments).expect("bounded diagnostic formatting cannot fail");
    if !writer.truncated {
        return writer.value;
    }
    if maximum <= TRUNCATION_SUFFIX.len() {
        return truncate_utf8(TRUNCATION_SUFFIX, maximum).to_owned();
    }
    let prefix = truncate_utf8(&writer.value, maximum - TRUNCATION_SUFFIX.len()).len();
    writer.value.truncate(prefix);
    writer.value.push_str(TRUNCATION_SUFFIX);
    writer.value
}

pub(super) fn bound_error(error: MetaError, maximum: usize) -> MetaError {
    match error {
        MetaError::RuntimeTerminal(message) => {
            MetaError::RuntimeTerminal(bound_owned(message, maximum))
        }
        MetaError::InvalidConfig(message) => {
            MetaError::InvalidConfig(bound_owned(message, maximum))
        }
        MetaError::Activation(message) => MetaError::Activation(bound_owned(message, maximum)),
        MetaError::Service(message) => MetaError::Service(bound_owned(message, maximum)),
        MetaError::InvalidInput(message) => MetaError::InvalidInput(bound_owned(message, maximum)),
        error => error,
    }
}

pub(super) fn bound_service_error(error: MetaError, maximum: usize) -> MetaError {
    match error {
        MetaError::Service(message) => MetaError::Service(bound_owned(message, maximum)),
        error => MetaError::Service(bound_formatted(format_args!("{error}"), maximum)),
    }
}
