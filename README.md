# Brachyura 
![Rust](https://img.shields.io/badge/rust-%23000000.svg?style=for-the-badge&logo=rust&logoColor=white)

A TLS terminating reverse proxy, which I am primarily using as a Rust learning project.

I utilize Nginx as part of my home lab providing reverse proxy functionality as well as TLS termination. The idea of this project is to replace Nginx with a light weight Rust based reverse proxy. Configurable via a yaml config file.

![ci-status](https://github.com/wjhoward/brachyura/actions/workflows/main.yml/badge.svg)


---
## Configuration
Configured in `config.yaml`

### TLS config
The key and cert paths are also defined in the yaml file. Only the connection between the client and proxy is encrypted.

### Proxy backend config

The proxy uses the host header to decide where to send the request, and this is configured in the yaml config file under "backends". The host header needs to match the name value, then the request is proxied to the location. For example:

    backends:
      - name: "test.home"
        location: "127.0.0.1:8000"

A request with the host header `test.home` would be proxied to `127.0.0.1:8000`

---

## Testing / Running

A TLS key and cert is required at the configured relative path.

Can be tested using curl, e.g:

`curl -H "host: test.home"  https://localhost:4000/`

Which based on the example config would proxy the request to: `http://127.0.0.1:8000/`
