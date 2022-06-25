use brachyura::run_server;
use std::net::SocketAddr;

#[tokio::main(worker_threads = 4)]
async fn main() {
    // TODO - handle config here?
    let addr = SocketAddr::from(([127, 0, 0, 1], 4000));
    // can an inner function to support tests
    run_server(addr).await;
}
