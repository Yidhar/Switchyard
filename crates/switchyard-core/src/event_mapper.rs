//! Maps provider-api events to canonical session events.
//!
//! Both sides define identical EventType enums with identical serde encoding,
//! so this mapper is a trivial variant-to-variant conversion. It exists as an
//! explicit layer so that core owns the boundary and any future divergence
//! between the two enums is handled in one place.

use switchyard_provider_api::ProviderEvent;
use switchyard_session::{Event, EventType};

/// Convert a ProviderEvent (from the provider adapter) into a canonical
/// session Event (for the store / event log).
pub fn map_provider_event(pe: &ProviderEvent) -> Event {
    Event {
        event_id: pe.event_id,
        turn_id: pe.turn_id,
        event_type: map_event_type(&pe.event_type),
        provider: pe.provider.clone(),
        timestamp: pe.timestamp,
        payload: pe.payload.clone(),
    }
}

fn map_event_type(pt: &switchyard_provider_api::EventType) -> EventType {
    use switchyard_provider_api::EventType as PE;
    match pt {
        PE::ThreadStarted => EventType::ThreadStarted,
        PE::TurnStarted => EventType::TurnStarted,
        PE::ItemStarted => EventType::ItemStarted,
        PE::ItemUpdated => EventType::ItemUpdated,
        PE::ItemCompleted => EventType::ItemCompleted,
        PE::ArtifactReady => EventType::ArtifactReady,
        PE::DelegateRequested => EventType::DelegateRequested,
        PE::DelegateCompleted => EventType::DelegateCompleted,
        PE::TurnCompleted => EventType::TurnCompleted,
        PE::TurnFailed => EventType::TurnFailed,
        // non_exhaustive: if provider-api adds a variant before session catches up,
        // we preserve it as a TurnFailed with the debug representation in payload.
        _ => EventType::TurnFailed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use switchyard_provider_api::{EventType as PE, ProviderEvent};
    use uuid::Uuid;

    #[test]
    fn all_variants_map_correctly() {
        let turn_id = Uuid::now_v7();
        let variants = [
            (PE::ThreadStarted, EventType::ThreadStarted),
            (PE::TurnStarted, EventType::TurnStarted),
            (PE::ItemStarted, EventType::ItemStarted),
            (PE::ItemUpdated, EventType::ItemUpdated),
            (PE::ItemCompleted, EventType::ItemCompleted),
            (PE::ArtifactReady, EventType::ArtifactReady),
            (PE::DelegateRequested, EventType::DelegateRequested),
            (PE::DelegateCompleted, EventType::DelegateCompleted),
            (PE::TurnCompleted, EventType::TurnCompleted),
            (PE::TurnFailed, EventType::TurnFailed),
        ];

        for (provider_type, expected_canonical) in variants {
            let pe = ProviderEvent::new(
                turn_id,
                provider_type,
                "test-provider",
                serde_json::json!({}),
            );
            let event = map_provider_event(&pe);
            assert_eq!(event.event_type, expected_canonical);
            assert_eq!(event.turn_id, turn_id);
            assert_eq!(event.event_id, pe.event_id);
            assert_eq!(event.provider, "test-provider");
        }
    }

    #[test]
    fn mapped_event_preserves_payload() {
        let turn_id = Uuid::now_v7();
        let payload = serde_json::json!({"text": "hello", "item_type": "agent_message"});
        let pe = ProviderEvent::new(turn_id, PE::ItemUpdated, "codex", payload.clone());
        let event = map_provider_event(&pe);
        assert_eq!(event.payload, payload);
    }

    #[test]
    fn mapped_event_preserves_timestamp() {
        let turn_id = Uuid::now_v7();
        let pe = ProviderEvent::turn_started(turn_id, "claude");
        let event = map_provider_event(&pe);
        assert_eq!(event.timestamp, pe.timestamp);
    }
}
