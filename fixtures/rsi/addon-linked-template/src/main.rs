use rsi_addon_linked_template::{GreetingContract, host, program};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let live = Arc::new(AtomicUsize::new(0));
    let running = host(rsi::AddonScope::Application, live.clone())?
        .start_program(program("Hello from an independent linked addon"))
        .await?;
    println!(
        "{}",
        running
            .lookup_local::<GreetingContract>()
            .ok_or("greeting unavailable")?
            .0
    );
    if !running.shutdown().await.is_clean() || live.load(Ordering::SeqCst) != 0 {
        return Err("addon did not cleanly stop".into());
    }
    Ok(())
}
