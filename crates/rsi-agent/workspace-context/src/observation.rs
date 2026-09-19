use super::utf8_prefix;
use std::path::Path;

const MAXIMUM_DIAGNOSTIC_BYTES: usize = 2048;

/// One failure is enough to withhold a partial observation. Its bounded detail
/// is covered by the snapshot scratch reservation, including while returned.
#[derive(Default)]
pub(super) struct Observation {
    pub(super) diagnostic: Option<String>,
}

impl Observation {
    pub(super) fn is_complete(&self) -> bool {
        self.diagnostic.is_none()
    }

    pub(super) fn io(&mut self, path: &Path, operation: &str, error: &std::io::Error) {
        self.fail(path, &format!("{operation}: {}", error.kind()));
    }

    pub(super) fn fail(&mut self, path: &Path, reason: &str) {
        if self.diagnostic.is_some() {
            return;
        }
        let mut text = String::with_capacity(MAXIMUM_DIAGNOSTIC_BYTES);
        text.push_str(utf8_prefix(reason, 256));
        text.push_str(" at \"");
        for character in path.to_string_lossy().chars().flat_map(char::escape_debug) {
            if text.len() + character.len_utf8() + 4 > MAXIMUM_DIAGNOSTIC_BYTES {
                text.push_str("...");
                break;
            }
            text.push(character);
        }
        text.push('"');
        self.diagnostic = Some(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn first_failure_is_bounded_escaped_and_retained() {
        let path = PathBuf::from(format!("bad\n\0\u{7f}\u{1b}路径{}", "🦀".repeat(4096)));
        let mut observation = Observation::default();
        observation.io(
            &path,
            "read source",
            &std::io::ErrorKind::PermissionDenied.into(),
        );
        assert!(!observation.is_complete());
        let diagnostic = observation.diagnostic.clone().unwrap();
        assert!(diagnostic.len() <= MAXIMUM_DIAGNOSTIC_BYTES);
        assert!(diagnostic.contains("permission denied"));
        assert!(diagnostic.contains("bad\\n\\0\\u{7f}\\u{1b}路径"));
        assert!(!diagnostic.chars().any(char::is_control));
        assert!(diagnostic.ends_with("...\""));
        observation.fail(Path::new("later"), "another error");
        assert_eq!(observation.diagnostic.as_deref(), Some(diagnostic.as_str()));
    }
}
