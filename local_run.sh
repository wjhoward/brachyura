function sigint_handler() {
    docker compose -f local_run_env/docker-compose.yaml down
}
trap "sigint_handler" 2

docker compose -f local_run_env/docker-compose.yaml build
docker compose -f local_run_env/docker-compose.yaml up -d

# Export spans to the Jaeger container over OTLP/gRPC (localhost:4317 by default)
OTEL_TRACES_EXPORTER=otlp cargo run