// Logic for selecting the request backend
use std::collections::HashMap;

use super::{Backend, BackendState};

pub fn router(
    backends_config: &[Backend],
    backends_state: &mut HashMap<String, Option<BackendState>>,
    host_header: &str,
) -> Option<String> {
    // Matches a given host header with a backend
    // Performs load balancing when configured

    let backend = match_backend(backends_config, host_header)?;

    // Check if load balancing is enabled
    if backend.backend_type.as_deref() == Some("loadbalanced") {
        if backend.locations.is_some() {
            let backend_state = backends_state.get_mut(&backend.name.clone()?)?.as_mut()?;
            round_robin_select(backend.locations.as_ref()?, backend_state)
        } else {
            // Config not valid
            None
        }
    } else if backend.location.is_some() {
        // Load balancing not enabled, return the single location / backend
        backend.location.clone()
    } else {
        // Config not valid
        None
    }
}

fn match_backend<'a>(backends: &'a [Backend], host_header: &str) -> Option<&'a Backend> {
    backends
        .iter()
        .find(|&backend| backend.name.as_deref() == Some(host_header))
}

fn round_robin_select(
    backend_locations: &Vec<String>,
    backend_state: &mut BackendState,
) -> Option<String> {
    let backend_count = backend_locations.len() as isize;
    let rr_count = backend_state.rr_count.get_mut();

    // If this is the first request or if we've exceeded the number of backends
    // set the counter to zero and return the first backend
    if *rr_count == -1 || *rr_count == (backend_count - 1) {
        *rr_count = 0;
        Some(backend_locations[0].clone())
    }
    // return the next backend
    else {
        *rr_count += 1;
        Some(backend_locations[*rr_count as usize].clone())
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::{read_proxy_config_yaml, router, ProxyState};

    #[tokio::test]
    async fn test_router_single_backend() {
        let config = read_proxy_config_yaml("tests/config.yaml".to_string())
            .await
            .unwrap();

        let mut proxy_mut_state = ProxyState::new(&config).backends;

        let backend = router(&config.backends, &mut proxy_mut_state, "test.home");
        assert_eq!(backend.unwrap(), "127.0.0.1:8000")
    }

    #[tokio::test]
    async fn test_router_loadbalanced_backend() {
        let config = read_proxy_config_yaml("tests/config.yaml".to_string())
            .await
            .unwrap();
        let mut proxy_mut_state = ProxyState::new(&config).backends;

        let backend = router(&config.backends, &mut proxy_mut_state, "test-lb.home");
        assert_eq!(backend.unwrap(), "127.0.0.1:8000")
    }

    #[tokio::test]
    async fn test_round_robin_select() {
        let config = read_proxy_config_yaml("tests/config.yaml".to_string())
            .await
            .unwrap();
        let mut proxy_mut_state = ProxyState::new(&config).backends;
        let backend_name = String::from("test-lb2.home");
        let backend_state = proxy_mut_state
            .get_mut(&backend_name.clone())
            .unwrap()
            .as_mut()
            .unwrap();
        let backend_locations = config.backends[1].locations.as_ref().unwrap();

        let first_backend = round_robin_select(backend_locations, backend_state).unwrap();
        assert_eq!(first_backend, String::from("127.0.0.1:8000"));
        let second_backend = round_robin_select(backend_locations, backend_state).unwrap();
        assert_eq!(second_backend, String::from("127.0.0.1:8001"));
        let third_backend = round_robin_select(backend_locations, backend_state).unwrap();
        assert_eq!(third_backend, String::from("127.0.0.1:8000"));
        let fourth_backend = round_robin_select(backend_locations, backend_state).unwrap();
        assert_eq!(fourth_backend, String::from("127.0.0.1:8001"));
        let fifth_backend = round_robin_select(backend_locations, backend_state).unwrap();
        assert_eq!(fifth_backend, String::from("127.0.0.1:8000"));
    }
}
