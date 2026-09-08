#[cfg(not(target_arch = "wasm32"))]
mod server;

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() {
    server::run().await;
}

#[cfg(target_arch = "wasm32")]
fn main() {}
