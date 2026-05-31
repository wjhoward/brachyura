use anyhow::Result;
use brachyura::run_server;

#[tokio::main]
async fn main() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    run_server("./config.yaml").await
}
