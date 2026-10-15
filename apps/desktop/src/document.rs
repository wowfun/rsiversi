#[cfg(windows)]
pub(super) const ORIGIN: &str = "http://rsi.localhost";
#[cfg(not(windows))]
pub(super) const ORIGIN: &str = "rsi://localhost";

pub(super) const REQUEST_CLOSE: &str = "window.dispatchEvent(new Event('rsi-native-close'))";

static ROOT_URL: std::sync::LazyLock<tauri::Url> = std::sync::LazyLock::new(|| {
    format!("{ORIGIN}/")
        .parse()
        .expect("constant desktop origin")
});

pub(super) fn root_url() -> tauri::Url {
    ROOT_URL.clone()
}

pub(super) fn admits_navigation(url: &tauri::Url) -> bool {
    let origin = &*ROOT_URL;
    url.scheme() == origin.scheme()
        && url.host_str() == origin.host_str()
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && matches!(
            url.path(),
            "/" | "/preview-local.html" | "/preview-online.html"
        )
}

#[cfg(test)]
mod tests {
    use super::{ORIGIN, admits_navigation};

    #[test]
    fn navigation_admits_only_the_document_and_fixed_preview_paths() {
        let root = super::root_url();
        assert_eq!(root.as_str(), format!("{ORIGIN}/"));
        assert_eq!(root.path(), "/");
        assert!(admits_navigation(&root));
        for path in [
            "/",
            "/#session",
            "/preview-local.html",
            "/preview-online.html",
        ] {
            assert!(admits_navigation(
                &format!("{ORIGIN}{path}").parse().unwrap()
            ));
        }
        for url in [
            format!("{ORIGIN}/_connect"),
            format!("{ORIGIN}/?source=external"),
            format!("{ORIGIN}:8888/"),
            ORIGIN.replace("://", "://user@") + "/",
            ORIGIN.replace("localhost", "localhost.evil") + "/",
            "https://rsi.localhost/".into(),
        ] {
            assert!(!admits_navigation(&url.parse().unwrap()), "{url}");
        }
        #[cfg(windows)]
        assert!(!admits_navigation(&"rsi://localhost/".parse().unwrap()));
        #[cfg(windows)]
        // URL parsing folds the default HTTP port into the same document origin.
        assert!(admits_navigation(
            &"http://rsi.localhost:80/".parse().unwrap()
        ));
        #[cfg(not(windows))]
        assert!(!admits_navigation(
            &"http://rsi.localhost/".parse().unwrap()
        ));
    }
}
