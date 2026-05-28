// Logic for selecting the request backend
use std::sync::{atomic::Ordering, Arc};

use super::{Backend, BackendState, RoutingState};

pub fn router(
    backends_config: &[Backend],
    proxy_state: Arc<RoutingState>,
    host_authority: String,
) -> Option<String> {
    // Matches a given host header or authority with a backend
    // Performs load balancing when configured
    let backend = match_backend(backends_config, &host_authority)?;

    match backend {
        Backend::Single { location, .. } => Some(location.clone()),
        Backend::LoadBalanced { name, locations } => {
            let backend_state = proxy_state.backends.get(name)?.as_ref()?;
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
    backend_state: &BackendState,
) -> Option<String> {
    // Takes &BackendState rather than &mut: AtomicUsize provides interior mutability,
    // allowing the counter to be incremented through a shared reference via atomic CPU instructions
    // Atomically increment the counter and use modulo to wrap it to a valid index
    // e.g. with 2 backends: 0, 1, 2, 3, ... becomes 0, 1, 0, 1, ...
    // fetch_add returns the old value and uses wrapping arithmetic, so overflow is safe
    let idx = backend_state.rr_count.fetch_add(1, Ordering::Relaxed) % backend_locations.len();
    Some(backend_locations[idx].clone())
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::{read_proxy_config_yaml, router, RoutingState};

    #[test]
    fn test_router_single_backend() {
        let config = read_proxy_config_yaml("tests/config.yaml".to_string()).unwrap();

        let proxy_state = Arc::new(RoutingState::new(&config));

        let backend = router(&config.backends, proxy_state, "test.home".to_string());
        assert_eq!(backend.unwrap(), "127.0.0.1:8000")
    }

    #[test]
    fn test_router_loadbalanced_backend() {
        let config = read_proxy_config_yaml("tests/config.yaml".to_string()).unwrap();
        let proxy_state = Arc::new(RoutingState::new(&config));

        let backend = router(&config.backends, proxy_state, "test-lb.home".to_string());
        assert_eq!(backend.unwrap(), "127.0.0.1:8000")
    }

    #[test]
    fn test_round_robin_select() {
        let config = read_proxy_config_yaml("tests/config.yaml".to_string()).unwrap();
        let proxy_state = RoutingState::new(&config);
        let backend_name = String::from("test-lb.home");
        let backend_state = proxy_state
            .backends
            .get(&backend_name)
            .unwrap()
            .as_ref()
            .unwrap();
        let backend_locations = match &config.backends[1] {
            Backend::LoadBalanced { locations, .. } => locations,
            _ => panic!("expected load balanced backend at index 1"),
        };

        let first_backend = round_robin_select(backend_locations, backend_state).unwrap();
        assert_eq!(first_backend, "127.0.0.1:8000");
        let second_backend = round_robin_select(backend_locations, backend_state).unwrap();
        assert_eq!(second_backend, "127.0.0.1:8001");
        let third_backend = round_robin_select(backend_locations, backend_state).unwrap();
        assert_eq!(third_backend, "127.0.0.1:8000");
        let fourth_backend = round_robin_select(backend_locations, backend_state).unwrap();
        assert_eq!(fourth_backend, "127.0.0.1:8001");
        let fifth_backend = round_robin_select(backend_locations, backend_state).unwrap();
        assert_eq!(fifth_backend, "127.0.0.1:8000");
    }

    #[test]
    fn test_unknown_backend() {
        let config = read_proxy_config_yaml("tests/config.yaml".to_string()).unwrap();
        let proxy_state = Arc::new(RoutingState::new(&config));

        let backend = router(&config.backends, proxy_state, "unknown.host".to_string());
        assert_eq!(backend, None);
    }
}
