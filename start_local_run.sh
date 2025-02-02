function sigint_handler() {
    docker compose -f utilities/local_run/docker-compose.yaml down
}
trap "sigint_handler" 2

docker compose -f utilities/local_run/docker-compose.yaml build
docker compose -f utilities/local_run/docker-compose.yaml up -d
cargo run