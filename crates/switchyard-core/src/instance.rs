use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use switchyard_provider_api::{LiveInstance, LiveInstanceRegistry};

#[derive(Default)]
#[allow(clippy::type_complexity)]
pub struct InstancePool {
    instances: Arc<Mutex<HashMap<String, Vec<Arc<tokio::sync::Mutex<dyn LiveInstance>>>>>>,
}

impl InstancePool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, name: &str, instance: Box<dyn LiveInstance>) {
        if let Ok(mut map) = self.instances.lock() {
            map.entry(name.to_string())
                .or_default()
                .push(Arc::from(tokio::sync::Mutex::new(instance)));
        }
    }

    pub fn remove_instance(&self, name: &str) -> Option<Arc<tokio::sync::Mutex<dyn LiveInstance>>> {
        if let Ok(mut map) = self.instances.lock() {
            if let Some(vec) = map.get_mut(name) {
                vec.pop()
            } else {
                None
            }
        } else {
            None
        }
    }

    pub fn clear(&self) {
        if let Ok(mut map) = self.instances.lock() {
            for (_, vec) in map.drain() {
                for inst_lock in vec {
                    tokio::spawn(async move {
                        let mut inst = inst_lock.lock().await;
                        let _ = inst.terminate().await;
                    });
                }
            }
        }
    }

    pub fn get_active_instances(&self) -> Vec<String> {
        if let Ok(map) = self.instances.lock() {
            map.iter()
                .filter(|(_, vec)| !vec.is_empty())
                .map(|(k, _)| k.clone())
                .collect()
        } else {
            Vec::new()
        }
    }
}

impl LiveInstanceRegistry for InstancePool {
    fn has_live_instance(&self, provider: &str) -> bool {
        if let Ok(map) = self.instances.lock() {
            map.get(provider).map(|v| !v.is_empty()).unwrap_or(false)
        } else {
            false
        }
    }

    fn checkout_instance(&self, provider: &str) -> Option<Arc<tokio::sync::Mutex<dyn LiveInstance>>> {
        if let Ok(mut map) = self.instances.lock() {
            if let Some(vec) = map.get_mut(provider) {
                while let Some(inst_lock) = vec.pop() {
                    let mut is_healthy = false;
                    let mut locked = false;
                    {
                        if let Ok(mut guard) = inst_lock.try_lock() {
                            locked = true;
                            is_healthy = guard.is_healthy();
                        }
                    }
                    if locked {
                        if is_healthy {
                            return Some(inst_lock);
                        } else {
                            let dead_inst = Arc::clone(&inst_lock);
                            tokio::spawn(async move {
                                let mut inst = dead_inst.lock().await;
                                let _ = inst.terminate().await;
                            });
                        }
                    }
                }
            }
        }
        None
    }

    fn release_instance(&self, provider: &str, instance: Arc<tokio::sync::Mutex<dyn LiveInstance>>) {
        let healthy = if let Ok(mut guard) = instance.try_lock() {
            guard.is_healthy()
        } else {
            true
        };

        if healthy {
            if let Ok(mut map) = self.instances.lock() {
                map.entry(provider.to_string()).or_default().push(instance);
            }
        } else {
            tokio::spawn(async move {
                if let Ok(mut guard) = instance.try_lock() {
                    let _ = guard.terminate().await;
                } else {
                    let mut guard = instance.lock().await;
                    let _ = guard.terminate().await;
                }
            });
        }
    }

    fn register_instance(&self, provider: &str, instance: Box<dyn LiveInstance>) {
        self.register(provider, instance);
    }
}
