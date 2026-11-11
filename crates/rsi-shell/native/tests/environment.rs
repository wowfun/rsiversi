use rsi_shell_native::scrub_child_environment;
use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt as _;

#[test]
fn child_environment_scrub_is_case_insensitive_and_preserves_ordinary_raw_values() {
    assert!(
        scrub_child_environment([(OsString::from("=D:"), OsString::from(r"D:\project"))])
            .is_empty()
    );
    let scrubbed = scrub_child_environment(vec![
        (OsString::from("API_KEY"), OsString::from("secret")),
        (OsString::from("rSi_INTERNAL"), OsString::from("secret")),
        (
            OsString::from("HTTPS_PROXY"),
            OsString::from("http://user:pass@example.test:8080"),
        ),
        (
            OsString::from("HTTP_PROXY"),
            OsString::from("http://example.test:8080"),
        ),
        (OsString::from("NO_PROXY"), OsString::from("localhost")),
        (OsString::from("VISIBLE"), OsString::from("yes")),
        (
            OsString::from("LD_PRELOAD"),
            OsString::from("/tmp/inject.so"),
        ),
        (OsString::from("BASH_ENV"), OsString::from("/tmp/bash-env")),
        #[cfg(unix)]
        (
            OsString::from("RAW_VALUE"),
            OsString::from_vec(vec![0xff, b'x']),
        ),
        #[cfg(unix)]
        (
            OsString::from_vec(vec![0xff, b'N']),
            OsString::from("bad-name"),
        ),
    ]);
    let get = |name: &str| {
        scrubbed
            .iter()
            .find(|(candidate, _)| candidate == OsStr::new(name))
            .map(|(_, value)| value)
    };
    assert!(get("API_KEY").is_none());
    assert!(get("rSi_INTERNAL").is_none());
    assert!(get("HTTPS_PROXY").is_none());
    assert_eq!(
        get("HTTP_PROXY"),
        Some(&OsString::from("http://example.test:8080"))
    );
    assert_eq!(get("NO_PROXY"), Some(&OsString::from("localhost")));
    assert_eq!(get("VISIBLE"), Some(&OsString::from("yes")));
    assert!(get("LD_PRELOAD").is_none());
    assert!(get("BASH_ENV").is_none());
    #[cfg(unix)]
    assert_eq!(
        get("RAW_VALUE"),
        Some(&OsString::from_vec(vec![0xff, b'x']))
    );
}
