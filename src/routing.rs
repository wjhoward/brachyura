// Logic for selecting the request backend
use std::collections::HashMap;
use std::sync::Mutex;

use super::Backend;

lazy_static! {
    // Round robin counter hashmap, shared by all threads
    static ref RR_COUNTER: Mutex<HashMap<String, u64>> = {
        Mutex::new(HashMap::new())
    };
}

pub fn router(backends: &[Backend], host_header: &str) -> Option<String> {
    // Matches a given host header with a backend
    // Performs load balancing when configured

    let backend = match_backend(backends, host_header)?;

    // Check if load balancing is enabled
    if backend.backend_type.is_some() && backend.backend_type.clone()? == "loadbalanced" {
        if backend.locations.is_some() {
            round_robin_select(
                backend.name.clone().unwrap(),
                backend.locations.as_ref().unwrap(),
            )
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
        .find(|&backend| backend.name.is_some() && host_header == backend.name.clone().unwrap())
}

fn round_robin_select(backend_name: String, backends: &Vec<String>) -> Option<String> {
    // Uses the rr_counter to keep an index of the next backend to select.
    // This counter is incremented or reset to zero when exceeding the number of backends
    let backend_count = backends.len();
    let mut rr_map = RR_COUNTER.lock().unwrap();

    // Check if key exists for this backend
    if rr_map.contains_key(&backend_name) {
        // Check if we've reached the last backend, if so reset counter to 0
        // and return the first backend
        if rr_map[&backend_name] == (backend_count - 1) as u64 {
            rr_map.insert(backend_name, 0);
            Some(backends[0].clone())
        } else {
            // Increment counter and return specific backend
            let current_count = rr_map[&backend_name];
            rr_map.insert(backend_name.clone(), current_count + 1);
            Some(backends[rr_map[&backend_name] as usize].clone())
        }
    } else {
        // First time dealing with this backend
        // So add a key and initial count and
        // return the first backend
        rr_map.insert(backend_name, 0);
        Some(backends[0].clone())
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::{read_proxy_config_yaml, router};

    #[tokio::test]
    async fn test_router_single_backend() {
        let config = read_proxy_config_yaml("tests/config.yaml".to_string())
            .await
            .unwrap();
        let backend = router(&config.backends, "test.home");
        assert_eq!(backend.unwrap(), "127.0.0.1:8000")
    }

    #[tokio::test]
    async fn test_router_loadbalanced_backend() {
        let config = read_proxy_config_yaml("tests/config.yaml".to_string())
            .await
            .unwrap();
        let backend = router(&config.backends, "test-lb.home");
        assert_eq!(backend.unwrap(), "127.0.0.1:8000")
    }

    #[tokio::test]
    async fn test_round_robin_select() {
        let config = read_proxy_config_yaml("tests/config.yaml".to_string())
            .await
            .unwrap();
        let backend_name = String::from("test-lb2.home");
        let backends = config.backends[1].locations.as_ref().unwrap();
        let first_backend = round_robin_select(backend_name.clone(), backends).unwrap();
        assert_eq!(first_backend, String::from("127.0.0.1:8000"));
        let second_backend = round_robin_select(backend_name.clone(), backends).unwrap();
        assert_eq!(second_backend, String::from("127.0.0.1:8001"));
        let third_backend = round_robin_select(backend_name.clone(), backends).unwrap();
        assert_eq!(third_backend, String::from("127.0.0.1:8000"));
        let fourth_backend = round_robin_select(backend_name.clone(), backends).unwrap();
        assert_eq!(fourth_backend, String::from("127.0.0.1:8001"));
        let fifth_backend = round_robin_select(backend_name, backends).unwrap();
        assert_eq!(fifth_backend, String::from("127.0.0.1:8000"));
    }
}
