use brachyura::run_server;

#[tokio::main(worker_threads = 4)]
async fn main() {
    let config_path = String::from("./config.yaml");
    run_server(config_path).await;
}
