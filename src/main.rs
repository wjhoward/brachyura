use brachyura::run_server;

#[tokio::main(worker_threads = 4)]
async fn main() {
    println!("Starting up...");
    let _ = rustls::crypto::ring::default_provider().install_default();
    println!("Done TLS stuff...");

    let config_path = String::from("./config.yaml");
    run_server(config_path).await;
}
