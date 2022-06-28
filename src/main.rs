use brachyura::run_server;
use std::net::SocketAddr;

#[tokio::main(worker_threads = 4)]
async fn main() {
    let config_path = String::from("./config.yaml");
    let addr = SocketAddr::from(([127, 0, 0, 1], 4000));
    run_server(addr, config_path).await;
}
