# Brachyura 
![Rust](https://img.shields.io/badge/rust-%23000000.svg?style=for-the-badge&logo=rust&logoColor=white)

A reverse proxy, which I am primarily using as a Rust / Hyper learning project.

I utilize Nginx as part of my home lab providing reverse proxy functionality as well as TLS termination. The idea of this project is to replace Nginx with a light weight Rust based reverse proxy.

## Testing / Running

The request is proxied to the host included in the host header.

#### For example:

`curl -v -H "host: localhost:5000" http://localhost:3000/test-path`

Would proxy the request to: `http://localhost:5000/test-path`