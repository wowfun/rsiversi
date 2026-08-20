#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    rsi_agent_fixture_conformance::run_conformance().await?;
    println!("rsi-agent keyless conformance passed");
    Ok(())
}
