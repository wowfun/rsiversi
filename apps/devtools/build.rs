fn main() {
    println!(
        "cargo:rustc-env=RSI_APP_TARGET={}",
        std::env::var("TARGET").expect("Cargo supplies the executable target")
    );
}
