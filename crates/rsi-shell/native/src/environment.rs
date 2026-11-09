use std::ffi::OsString;

pub(crate) fn prepare(
    variables: Vec<(OsString, OsString)>,
) -> rsi_tools_protocol::Result<Vec<(OsString, OsString)>> {
    const DEFAULTS: [(&str, &str); 4] = [
        ("NO_COLOR", "1"),
        ("TERM", "dumb"),
        ("PAGER", "cat"),
        ("GIT_PAGER", "cat"),
    ];
    let mut environment = scrub_child_environment(variables);
    environment.retain(|(name, _)| {
        !DEFAULTS.iter().any(|(key, _)| {
            if cfg!(windows) {
                name.eq_ignore_ascii_case(key)
            } else {
                name == key
            }
        })
    });
    environment.extend(DEFAULTS.map(|(name, value)| (name.into(), value.into())));
    rsi_process::validate_environment(&environment)
        .map_err(|error| rsi_tools_protocol::ToolError::InvalidInput(error.to_string()))?;
    Ok(environment)
}

/// Captures and scrubs an explicit child-environment snapshot.
///
/// Non-UTF-8 names, secret-shaped names, `RSI_*`, and credential-bearing or
/// non-UTF-8 proxy values are dropped. Ordinary raw values remain byte-exact.
pub fn scrub_child_environment<I>(variables: I) -> Vec<(OsString, OsString)>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    let mut retained = Vec::new();
    for (name, value) in variables {
        let Some(name_text) = name.to_str() else {
            continue;
        };
        if name_text.contains('=') {
            continue;
        }
        let uppercase = name_text.to_ascii_uppercase();
        if uppercase.starts_with("RSI_")
            || uppercase.starts_with("LD_")
            || uppercase.starts_with("DYLD_")
            || uppercase.starts_with("BASH_FUNC_")
            || matches!(
                uppercase.as_str(),
                "BASH_ENV"
                    | "ENV"
                    | "SHELLOPTS"
                    | "BASHOPTS"
                    | "GCONV_PATH"
                    | "LOCPATH"
                    | "NLSPATH"
                    | "GLIBC_TUNABLES"
            )
            || ["KEY", "PASSWORD", "SECRET", "TOKEN"]
                .iter()
                .any(|needle| uppercase.contains(needle))
        {
            continue;
        }
        if matches!(
            uppercase.as_str(),
            "HTTP_PROXY" | "HTTPS_PROXY" | "ALL_PROXY"
        ) {
            let Some(proxy) = value.to_str() else {
                continue;
            };
            if proxy_has_userinfo(proxy) {
                continue;
            }
        }
        retained.push((name, value));
    }
    retained.sort_by(|left, right| left.0.cmp(&right.0));
    retained
}

fn proxy_has_userinfo(proxy: &str) -> bool {
    let authority = proxy
        .split_once("://")
        .map_or(proxy, |(_, authority)| authority);
    let authority = authority.split(['/', '?', '#']).next().unwrap_or(authority);
    authority.contains('@')
}
