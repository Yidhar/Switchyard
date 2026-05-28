//! Persistent instance pool keyed by `(provider, session_id)`.
//!
//! Each bucket holds zero or more [`InstanceEntry`] values; identity travels
//! with the entry via [`InstanceMetadata`]. The pool is the **only** source of
//! truth for instance lifecycle state — callers transition state via
//! [`LiveInstanceRegistry::update_state`] or implicitly through
//! checkout/release.
//!
//! Cross-session isolation is hard: an instance registered for
//! `(claude, session-A)` is never visible to lookups in `(claude, session-B)`.
//! This matches the "single window, single project" model and keeps provider
//! subprocess/session state isolated between Switchyard sessions.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use uuid::Uuid;

use crate::{InstanceMetadata, InstanceState, LabelConflict, LiveInstance, LiveInstanceRegistry};

type BucketKey = (String, Uuid);

struct InstanceEntry {
    metadata: InstanceMetadata,
    instance: Arc<tokio::sync::Mutex<dyn LiveInstance>>,
}

#[derive(Default)]
pub struct InstancePool {
    /// (provider, session_id) -> entries in registration order.
    by_key: Arc<Mutex<HashMap<BucketKey, Vec<InstanceEntry>>>>,
    /// instance_id -> bucket key, for O(1) id lookups.
    by_id: Arc<Mutex<HashMap<Uuid, BucketKey>>>,
}

impl InstancePool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Tear down every instance and clear the pool. Used at shutdown.
    pub fn clear(&self) {
        let mut by_key = self.by_key.lock().unwrap();
        let mut by_id = self.by_id.lock().unwrap();
        for (_, bucket) in by_key.drain() {
            for entry in bucket {
                by_id.remove(&entry.metadata.instance_id);
                let inst = entry.instance;
                tokio::spawn(async move {
                    let mut guard = inst.lock().await;
                    let _ = guard.terminate().await;
                });
            }
        }
    }

    /// Terminate every instance registered for `session_id` and remove them
    /// from the pool. Used when a Switchyard session is closed.
    pub fn clear_session(&self, session_id: Uuid) {
        let mut by_key = self.by_key.lock().unwrap();
        let mut by_id = self.by_id.lock().unwrap();
        let keys: Vec<BucketKey> = by_key
            .keys()
            .filter(|(_, sid)| *sid == session_id)
            .cloned()
            .collect();
        for key in keys {
            if let Some(bucket) = by_key.remove(&key) {
                for entry in bucket {
                    by_id.remove(&entry.metadata.instance_id);
                    let inst = entry.instance;
                    tokio::spawn(async move {
                        let mut guard = inst.lock().await;
                        let _ = guard.terminate().await;
                    });
                }
            }
        }
    }

    fn locate(&self, instance_id: Uuid) -> Option<BucketKey> {
        self.by_id.lock().unwrap().get(&instance_id).cloned()
    }

    fn transition_to_busy_if_idle(entry: &mut InstanceEntry) -> bool {
        if matches!(entry.metadata.state, InstanceState::Idle) {
            entry.metadata.state = InstanceState::Busy {
                turn_id: Uuid::nil(),
            };
            true
        } else {
            false
        }
    }
}

impl LiveInstanceRegistry for InstancePool {
    fn register(
        &self,
        metadata: InstanceMetadata,
        instance: Box<dyn LiveInstance>,
    ) -> Result<Uuid, LabelConflict> {
        let key = (metadata.provider.clone(), metadata.session_id);

        // Label conflict check — must happen under the same lock as insertion
        // to avoid races with concurrent register calls.
        let mut by_key = self.by_key.lock().unwrap();
        if let Some(label) = metadata.label.as_deref()
            && let Some(bucket) = by_key.get(&key)
            && bucket
                .iter()
                .any(|e| e.metadata.label.as_deref() == Some(label))
        {
            return Err(LabelConflict {
                provider: metadata.provider.clone(),
                session_id: metadata.session_id,
                label: label.to_string(),
            });
        }

        let instance_id = metadata.instance_id;
        let entry = InstanceEntry {
            metadata,
            instance: Arc::new(tokio::sync::Mutex::new(instance)),
        };

        self.by_id.lock().unwrap().insert(instance_id, key.clone());
        by_key.entry(key).or_default().push(entry);
        Ok(instance_id)
    }

    fn checkout_by_id(
        &self,
        instance_id: Uuid,
    ) -> Option<Arc<tokio::sync::Mutex<dyn LiveInstance>>> {
        let key = self.locate(instance_id)?;
        let mut by_key = self.by_key.lock().unwrap();
        let bucket = by_key.get_mut(&key)?;
        let entry = bucket
            .iter_mut()
            .find(|e| e.metadata.instance_id == instance_id)?;
        if Self::transition_to_busy_if_idle(entry) {
            Some(entry.instance.clone())
        } else {
            None
        }
    }

    fn checkout_by_label(
        &self,
        provider: &str,
        session_id: Uuid,
        label: &str,
    ) -> Option<Arc<tokio::sync::Mutex<dyn LiveInstance>>> {
        let key = (provider.to_string(), session_id);
        let mut by_key = self.by_key.lock().unwrap();
        let bucket = by_key.get_mut(&key)?;
        let entry = bucket.iter_mut().find(|e| {
            e.metadata.label.as_deref() == Some(label)
                && matches!(e.metadata.state, InstanceState::Idle)
        })?;
        if Self::transition_to_busy_if_idle(entry) {
            Some(entry.instance.clone())
        } else {
            None
        }
    }

    fn checkout_any_idle(
        &self,
        provider: &str,
        session_id: Uuid,
    ) -> Option<(Uuid, Arc<tokio::sync::Mutex<dyn LiveInstance>>)> {
        let key = (provider.to_string(), session_id);
        let mut by_key = self.by_key.lock().unwrap();
        let bucket = by_key.get_mut(&key)?;
        let entry = bucket
            .iter_mut()
            .find(|e| matches!(e.metadata.state, InstanceState::Idle))?;
        let id = entry.metadata.instance_id;
        if Self::transition_to_busy_if_idle(entry) {
            Some((id, entry.instance.clone()))
        } else {
            None
        }
    }

    fn release(&self, instance_id: Uuid) {
        let key = match self.locate(instance_id) {
            Some(k) => k,
            None => return,
        };
        let mut by_key = self.by_key.lock().unwrap();
        if let Some(bucket) = by_key.get_mut(&key)
            && let Some(entry) = bucket
                .iter_mut()
                .find(|e| e.metadata.instance_id == instance_id)
        {
            // Keep Dead state sticky so terminate-in-progress cleanup wins
            // the race against release.
            if !matches!(entry.metadata.state, InstanceState::Dead) {
                entry.metadata.state = InstanceState::Idle;
            }
        }
    }

    fn has_live_instance(&self, provider: &str, session_id: Uuid) -> bool {
        let key = (provider.to_string(), session_id);
        let by_key = self.by_key.lock().unwrap();
        by_key.get(&key).is_some_and(|b| !b.is_empty())
    }

    fn list_session(&self, session_id: Uuid) -> Vec<InstanceMetadata> {
        let by_key = self.by_key.lock().unwrap();
        by_key
            .iter()
            .filter(|((_, sid), _)| *sid == session_id)
            .flat_map(|(_, bucket)| bucket.iter().map(|e| e.metadata.clone()))
            .collect()
    }

    fn update_state(&self, instance_id: Uuid, state: InstanceState) {
        let key = match self.locate(instance_id) {
            Some(k) => k,
            None => return,
        };
        let mut by_key = self.by_key.lock().unwrap();
        if let Some(bucket) = by_key.get_mut(&key)
            && let Some(entry) = bucket
                .iter_mut()
                .find(|e| e.metadata.instance_id == instance_id)
        {
            entry.metadata.state = state;
        }
    }

    fn terminate(&self, instance_id: Uuid) {
        let key = match self.by_id.lock().unwrap().remove(&instance_id) {
            Some(k) => k,
            None => return,
        };
        let entry = {
            let mut by_key = self.by_key.lock().unwrap();
            by_key.get_mut(&key).and_then(|bucket| {
                let idx = bucket
                    .iter()
                    .position(|e| e.metadata.instance_id == instance_id)?;
                Some(bucket.swap_remove(idx))
            })
        };
        if let Some(entry) = entry {
            let inst = entry.instance;
            tokio::spawn(async move {
                let mut guard = inst.lock().await;
                let _ = guard.terminate().await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContextBundle, InstanceKind, InstanceMetadata, ProviderError, ProviderEvent};
    use async_trait::async_trait;
    use tokio::sync::mpsc;

    /// Minimal LiveInstance for pool testing — never spawns a real process.
    struct StubInstance {
        healthy: bool,
    }

    #[async_trait]
    impl LiveInstance for StubInstance {
        async fn send_message(
            &mut self,
            _text: &str,
        ) -> Result<mpsc::Receiver<ProviderEvent>, ProviderError> {
            let (_tx, rx) = mpsc::channel(1);
            Ok(rx)
        }
        async fn update_context(&mut self, _ctx: ContextBundle) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn terminate(&mut self) -> Result<(), ProviderError> {
            self.healthy = false;
            Ok(())
        }
        fn is_healthy(&mut self) -> bool {
            self.healthy
        }
    }

    fn make_meta(provider: &str, session: Uuid, label: Option<&str>) -> InstanceMetadata {
        let mut meta = InstanceMetadata::new(
            provider,
            session,
            label.map(|s| s.to_string()),
            InstanceKind::Worker,
        );
        meta.state = InstanceState::Idle;
        meta
    }

    #[test]
    fn register_distinct_labels_succeeds() {
        let pool = InstancePool::new();
        let session = Uuid::now_v7();
        let id1 = pool
            .register(
                make_meta("claude", session, Some("a")),
                Box::new(StubInstance { healthy: true }),
            )
            .unwrap();
        let id2 = pool
            .register(
                make_meta("claude", session, Some("b")),
                Box::new(StubInstance { healthy: true }),
            )
            .unwrap();
        assert_ne!(id1, id2);
        assert_eq!(pool.list_session(session).len(), 2);
    }

    #[test]
    fn register_duplicate_label_in_same_session_conflicts() {
        let pool = InstancePool::new();
        let session = Uuid::now_v7();
        pool.register(
            make_meta("claude", session, Some("dup")),
            Box::new(StubInstance { healthy: true }),
        )
        .unwrap();
        let err = pool
            .register(
                make_meta("claude", session, Some("dup")),
                Box::new(StubInstance { healthy: true }),
            )
            .unwrap_err();
        assert_eq!(err.label, "dup");
        assert_eq!(err.provider, "claude");
        assert_eq!(err.session_id, session);
    }

    #[test]
    fn duplicate_label_across_sessions_is_fine() {
        let pool = InstancePool::new();
        let session_a = Uuid::now_v7();
        let session_b = Uuid::now_v7();
        pool.register(
            make_meta("claude", session_a, Some("dup")),
            Box::new(StubInstance { healthy: true }),
        )
        .unwrap();
        pool.register(
            make_meta("claude", session_b, Some("dup")),
            Box::new(StubInstance { healthy: true }),
        )
        .unwrap();
        assert_eq!(pool.list_session(session_a).len(), 1);
        assert_eq!(pool.list_session(session_b).len(), 1);
    }

    #[test]
    fn unlabelled_instances_never_conflict() {
        let pool = InstancePool::new();
        let session = Uuid::now_v7();
        pool.register(
            make_meta("claude", session, None),
            Box::new(StubInstance { healthy: true }),
        )
        .unwrap();
        pool.register(
            make_meta("claude", session, None),
            Box::new(StubInstance { healthy: true }),
        )
        .unwrap();
        assert_eq!(pool.list_session(session).len(), 2);
    }

    #[test]
    fn checkout_by_label_excludes_busy_match() {
        let pool = InstancePool::new();
        let session = Uuid::now_v7();
        pool.register(
            make_meta("claude", session, Some("worker-a")),
            Box::new(StubInstance { healthy: true }),
        )
        .unwrap();
        let first = pool.checkout_by_label("claude", session, "worker-a");
        assert!(first.is_some());
        // Same label, already busy → None
        let second = pool.checkout_by_label("claude", session, "worker-a");
        assert!(second.is_none());
    }

    #[test]
    fn checkout_any_idle_skips_busy() {
        let pool = InstancePool::new();
        let session = Uuid::now_v7();
        let id_busy = pool
            .register(
                make_meta("claude", session, Some("a")),
                Box::new(StubInstance { healthy: true }),
            )
            .unwrap();
        let id_idle = pool
            .register(
                make_meta("claude", session, Some("b")),
                Box::new(StubInstance { healthy: true }),
            )
            .unwrap();
        // Force the first into Busy
        pool.update_state(
            id_busy,
            InstanceState::Busy {
                turn_id: Uuid::nil(),
            },
        );
        let (returned_id, _) = pool
            .checkout_any_idle("claude", session)
            .expect("idle available");
        assert_eq!(returned_id, id_idle);
    }

    #[test]
    fn release_after_checkout_transitions_back_to_idle() {
        let pool = InstancePool::new();
        let session = Uuid::now_v7();
        let id = pool
            .register(
                make_meta("claude", session, Some("a")),
                Box::new(StubInstance { healthy: true }),
            )
            .unwrap();
        let _ = pool.checkout_by_id(id).expect("checkout once");
        // Second checkout while busy → None
        assert!(pool.checkout_by_id(id).is_none());
        pool.release(id);
        // After release → checkout works again
        assert!(pool.checkout_by_id(id).is_some());
    }

    #[test]
    fn has_live_instance_partitions_by_session() {
        let pool = InstancePool::new();
        let session_a = Uuid::now_v7();
        let session_b = Uuid::now_v7();
        pool.register(
            make_meta("claude", session_a, Some("a")),
            Box::new(StubInstance { healthy: true }),
        )
        .unwrap();
        assert!(pool.has_live_instance("claude", session_a));
        assert!(!pool.has_live_instance("claude", session_b));
        assert!(!pool.has_live_instance("codex", session_a));
    }

    #[tokio::test]
    async fn terminate_removes_from_pool_and_id_index() {
        let pool = InstancePool::new();
        let session = Uuid::now_v7();
        let id = pool
            .register(
                make_meta("claude", session, Some("a")),
                Box::new(StubInstance { healthy: true }),
            )
            .unwrap();
        assert!(pool.has_live_instance("claude", session));
        // terminate spawns a tokio task to drop the instance, so we need a
        // runtime — this test uses #[tokio::test] for that reason.
        pool.terminate(id);
        assert!(!pool.has_live_instance("claude", session));
        assert!(pool.checkout_by_id(id).is_none());
    }
}
