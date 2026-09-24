use rsi_session_protocol::{
    Result, SessionError,
    export::{ExportFormat, ExportOptions},
};

/// Parsed application export command; a path is interpreted only by its client.
#[derive(Clone, Debug, Default)]
pub struct ExportCommand {
    /// Optional client path or browser filename hint.
    pub path: Option<String>,
    /// Shared artifact options.
    pub options: ExportOptions,
}
/// Tokenizes quoted slash arguments without executing shell syntax.
pub fn parse_export_arguments(input: &str) -> Result<ExportCommand> {
    if input.len() > 8192 || input.contains(['\r', '\n', '\0']) {
        return Err(usage());
    }
    let mut tokens = vec![];
    let mut token = String::new();
    let mut quote = None;
    let mut started = false;
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if Some(ch) == quote {
            quote = None;
            started = true;
        } else if quote.is_none() && matches!(ch, '\'' | '"') {
            quote = Some(ch);
            started = true;
        } else if ch == '\\'
            && chars
                .peek()
                .is_some_and(|c| matches!(c, '\\' | '\'' | '"' | ' '))
        {
            if let Some(escaped) = chars.next() {
                token.push(escaped);
            }
            started = true;
        } else if quote.is_none() && ch.is_whitespace() {
            if started {
                tokens.push(std::mem::take(&mut token));
                started = false;
            }
        } else {
            token.push(ch);
            started = true;
        }
    }
    if quote.is_some() {
        return Err(usage());
    }
    if started {
        tokens.push(token);
    }
    parse_export_tokens(tokens)
}
/// Parses already separated CLI tokens with the same include/format grammar.
pub fn parse_export_tokens(tokens: impl IntoIterator<Item = String>) -> Result<ExportCommand> {
    let mut command = ExportCommand::default();
    let mut format = false;
    let mut include = false;
    let mut tokens = tokens.into_iter();
    let mut bytes = 0usize;
    while let Some(token) = tokens.next() {
        bytes = bytes.checked_add(token.len()).ok_or_else(usage)?;
        if bytes > 8192 {
            return Err(usage());
        }
        let (name, inline) = token
            .split_once('=')
            .map_or((token.as_str(), None), |(k, v)| (k, Some(v.to_string())));
        match name {
            "-f" | "--format" => {
                if format {
                    return Err(usage());
                }
                format = true;
                command.options.format =
                    match inline.or_else(|| tokens.next()).ok_or_else(usage)?.as_str() {
                        "md" | "markdown" => ExportFormat::Markdown,
                        "json" => ExportFormat::Json,
                        _ => return Err(usage()),
                    };
            }
            "-i" | "--include" => {
                if include {
                    return Err(usage());
                }
                include = true;
                let value = inline.or_else(|| tokens.next()).ok_or_else(usage)?;
                if value.len() > 8192 {
                    return Err(usage());
                }
                command.options.select(&value)?;
            }
            "-o" | "--export-path" => {
                if command.path.is_some() {
                    return Err(usage());
                }
                command.path = Some(inline.or_else(|| tokens.next()).ok_or_else(usage)?);
            }
            _ if !token.starts_with('-') && command.path.is_none() => command.path = Some(token),
            _ => return Err(usage()),
        }
    }
    if command
        .path
        .as_ref()
        .is_some_and(|p| p.is_empty() || p.len() > 4096 || p.contains(['\0', '\r', '\n']))
    {
        return Err(usage());
    }
    command.options.validate()?;
    Ok(command)
}
fn usage() -> SessionError {
    SessionError::Invalid("usage: /export [PATH] [-f markdown|md|json] [-i header,messages,reasoning,provider-input-evidence,last-provider-request,last-provider-response]".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_session_protocol::export::ExportInclude;
    #[test]
    fn quoted_paths_aliases_and_exact_selection() {
        let parsed = parse_export_arguments("'a b.json' -f=json --include h,r").unwrap();
        assert_eq!(parsed.path.as_deref(), Some("a b.json"));
        assert_eq!(parsed.options.format, ExportFormat::Json);
        assert!(parsed.options.has(ExportInclude::Messages));
        assert!(!parsed.options.has(ExportInclude::ProviderInputEvidence));
        assert_eq!(
            parse_export_arguments(r"C:\logs\out.md")
                .unwrap()
                .path
                .as_deref(),
            Some(r"C:\logs\out.md")
        );
        for bad in [
            "'unterminated",
            "-f yaml",
            "-i unknown",
            "a b",
            "-i ''",
            "-f json -f md",
            "a\nb",
        ] {
            assert!(parse_export_arguments(bad).is_err(), "{bad}");
        }
    }
}
