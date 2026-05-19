use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use switchyard_provider_api::{LiveInstance, LiveInstanceRegistry};

pub struct InstancePool {
    instances: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<dyn LiveInstance>>>>>,
}

impl InstancePool {
    pub fn new() -> Self {
        Self {
            instances: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn register(&self, name: &str, instance: Box<dyn LiveInstance>) {
        if let Ok(mut map) = self.instances.lock() {
            map.insert(name.to_string(), Arc::from(tokio::sync::Mutex::new(instance)));
        }
    }

    pub fn remove_instance(&self, name: &str) -> Option<Arc<tokio::sync::Mutex<dyn LiveInstance>>> {
        if let Ok(mut map) = self.instances.lock() {
            map.remove(name)
        } else {
            None
        }
    }

    pub fn clear(&self) {
        if let Ok(mut map) = self.instances.lock() {
            for (_, inst_lock) in map.drain() {
                tokio::spawn(async move {
                    let mut inst = inst_lock.lock().await;
                    let _ = inst.terminate().await;
                });
            }
        }
    }
}

impl LiveInstanceRegistry for InstancePool {
    fn get_live_instance(&self, provider: &str) -> Option<Arc<tokio::sync::Mutex<dyn LiveInstance>>> {
        if let Ok(map) = self.instances.lock() {
            map.get(provider).cloned()
        } else {
            None
        }
    }
}
