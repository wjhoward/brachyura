// Logic for selecting the request backend
use std::sync::{Arc, Mutex};

use super::{Backend, BackendState, ProxyState};

pub fn router(
    backends_config: &[Backend],
    proxy_state: Arc<Mutex<ProxyState>>,
    host_authority: String,
) -> Option<String> {
    // Matches a given host header or authority with a backend
    // Performs load balancing when configured

    // Proxy state mutex is unlocked within this function (rather than in calling code)
    // so that the mutex guard goes out of scope once the function completes
    let backends_state = &mut proxy_state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .backends;

    let backend = match_backend(backends_config, &host_authority)?;

    match backend {
        Backend::Single { location, .. } => Some(location.clone()),
        Backend::LoadBalanced { name, locations } => {
            let backend_state = backends_state.get_mut(name)?.as_mut()?;
            round_robin_select(locations, backend_state)
        }
    }
}

fn match_backend<'a>(backends: &'a [Backend], host_authority: &str) -> Option<&'a Backend> {
    backends
        .iter()
        .find(|backend| backend.name() == host_authority)
}

fn round_robin_select(
    backend_locations: &[String],
    backend_state: &mut BackendState,
) -> Option<String> {
    let backend_count = backend_locations.len() as isize;
    let rr_count = &mut backend_state.rr_count;

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

        let proxy_state = Arc::new(Mutex::new(ProxyState::new(&config)));

        let backend = router(&config.backends, proxy_state, "test.home".to_string());
        assert_eq!(backend.unwrap(), "127.0.0.1:8000")
    }

    #[tokio::test]
    async fn test_router_loadbalanced_backend() {
        let config = read_proxy_config_yaml("tests/config.yaml".to_string())
            .await
            .unwrap();
        let proxy_state = Arc::new(Mutex::new(ProxyState::new(&config)));

        let backend = router(&config.backends, proxy_state, "test-lb.home".to_string());
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
        let backend_locations = match &config.backends[1] {
            Backend::LoadBalanced { locations, .. } => locations,
            _ => panic!("expected load balanced backend at index 1"),
        };

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

    #[tokio::test]
    async fn test_unknown_backend() {
        let config = read_proxy_config_yaml("tests/config.yaml".to_string())
            .await
            .unwrap();
        let proxy_state = Arc::new(Mutex::new(ProxyState::new(&config)));

        let backend = router(&config.backends, proxy_state, "unknown.host".to_string());
        assert_eq!(backend, None);
    }
}
