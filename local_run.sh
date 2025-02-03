function sigint_handler() {
    docker compose -f local_run_env/docker-compose.yaml down
}
trap "sigint_handler" 2

docker compose -f local_run_env/docker-compose.yaml build
docker compose -f local_run_env/docker-compose.yaml up -d
cargo run