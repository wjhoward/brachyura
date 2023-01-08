# Brachyura 
![Rust](https://img.shields.io/badge/rust-%23000000.svg?style=for-the-badge&logo=rust&logoColor=white)

A TLS terminating, load balancing reverse proxy, which I am primarily using as a Rust learning project. **Currently a work in progress.**

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
      - name: "origin.home"
        location: "127.0.0.1:10000"

A request with the host header `origin.home` would be proxied to `127.0.0.1:10000`

**Load balancing**

Multiple backends can be defined for a given host header, where requests to these backends is load balanced. Currently only round robin load balancing is supported. Example config:

    backends:
      - name: "test-lb.home"
        backend_type: "loadbalanced"
        locations:
          - "127.0.0.1:8000"
          - "127.0.0.1:8001"


---

## Testing

### Library tests
To run the library / unit tests run: `cargo test --lib`

### Full tests

The full test suite (`cargo test`) requires a TLS key and cert configured and existing at the relative path.

## Running

**Requirements**
* A configured TLS key and cert
* A running backend / origin server
* A configuration file defining the proxy listen address / port, as well as a backend server config

The server can simply be run via `cargo run`. Below are some curl manual test examples (using self signed certs).

**HTTP 1.1 client example**

```
curl -v --http1.1  https://localhost:4000/ -H "Host: origin.home" --insecure

> GET / HTTP/1.1
> Host: origin.home
> User-Agent: curl/7.68.0
> Accept: */*
>
* TLSv1.3 (IN), TLS handshake, Newsession Ticket (4):
<
This is the Python origin server, listening on port: 10000 request HTTP version: HTTP/1.1
```

**HTTP 2 client example**

```
curl -v --http2  https://localhost:4000/ -H "Host: origin.home" --insecure

> GET / HTTP/2
> Host: origin.home
> user-agent: curl/7.68.0
> accept: */*
>
* TLSv1.3 (IN), TLS handshake, Newsession Ticket (4):
<
This is the Python origin server, listening on port: 10000 request HTTP version: HTTP/1.1
```

You will notice that the origin server reports an HTTP 1.1 protocol, this is due to the code currently downgrading the downstream connection, see the code for more details.