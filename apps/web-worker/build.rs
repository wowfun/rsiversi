fn main() {
    println!("cargo:rerun-if-env-changed=RSI_WEB_BUNDLE_ID");
    if let Ok(identity) = std::env::var("RSI_WEB_BUNDLE_ID") {
        assert!(
            identity.len() == 64
                && identity
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
            "Web bundle identity must be lowercase SHA-256-shaped hexadecimal"
        );
        println!("cargo:rustc-env=RSI_WEB_BUNDLE_ID={identity}");
    }
}
