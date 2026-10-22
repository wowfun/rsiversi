pub(super) fn product_binary() -> std::path::PathBuf {
    let Some(path) = std::env::var_os("RSI_TEST_PRODUCT_BINARY") else {
        return env!("CARGO_BIN_EXE_rsi").into();
    };
    let path = std::path::PathBuf::from(path);
    assert!(
        path.is_absolute() && path.is_file(),
        "RSI_TEST_PRODUCT_BINARY must select an absolute product executable"
    );
    path
}
