use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use switchyard_store::{SessionCatalog, SessionRepository, StoreHandle};
use uuid::Uuid;

pub fn resolve_work_dir(base: &Path, candidate: &Path) -> PathBuf {
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base.join(candidate)
    }
}

pub fn resolve_resume_session(
    store: &StoreHandle,
    session_selector: Option<&str>,
    resume_latest: bool,
) -> Result<Option<Uuid>, String> {
    match session_selector {
        Some(selector) => resolve_session_selector(store, selector).map(Some),
        None if resume_latest => latest_session_id(store).map(Some),
        None => Ok(None),
    }
}

pub fn resolve_session_selector(store: &StoreHandle, selector: &str) -> Result<Uuid, String> {
    let normalized = selector.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Err("`--session` cannot be empty".to_string());
    }

    let matches = store
        .list_sessions()
        .map_err(|err| format!("list sessions: {err}"))?
        .into_iter()
        .filter(|session_id| session_id.to_string().starts_with(&normalized))
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [session_id] => Ok(*session_id),
        [] => Err(format!("session '{selector}' not found in selected store")),
        _ => Err(format!(
            "session prefix '{selector}' is ambiguous; matches: {}",
            matches
                .iter()
                .map(Uuid::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

pub fn latest_session_id(store: &StoreHandle) -> Result<Uuid, String> {
    let mut sessions = load_sessions_with_updated_at(store)?;
    sessions.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    sessions
        .into_iter()
        .map(|(session_id, _)| session_id)
        .next()
        .ok_or_else(|| "selected store contains no sessions".to_string())
}

fn load_sessions_with_updated_at(
    store: &StoreHandle,
) -> Result<Vec<(Uuid, DateTime<Utc>)>, String> {
    Ok(store
        .list_sessions()
        .map_err(|err| format!("list sessions: {err}"))?
        .into_iter()
        .filter_map(|session_id| {
            store
                .load_session(session_id)
                .ok()
                .flatten()
                .map(|session| (session.session_id, session.updated_at))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use switchyard_session::Session;

    fn open_temp_jsonl_store() -> (tempfile::TempDir, StoreHandle) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".switchyard").join("sessions");
        let store = StoreHandle::open(switchyard_store::StoreBackend::Jsonl, path).unwrap();
        (dir, store)
    }

    #[test]
    fn resolve_resume_session_returns_none_when_not_requested() {
        let (_dir, store) = open_temp_jsonl_store();
        let selected = resolve_resume_session(&store, None, false).unwrap();
        assert_eq!(selected, None);
    }

    #[test]
    fn resolve_session_selector_accepts_unique_prefix() {
        let (_dir, mut store) = open_temp_jsonl_store();
        let mut session = Session::new("codex".to_string());
        session.session_id = Uuid::parse_str("11111111-1111-7111-8111-111111111111").unwrap();
        store.save_session(&session).unwrap();

        let selected = resolve_session_selector(&store, "11111111").unwrap();
        assert_eq!(selected, session.session_id);
    }

    #[test]
    fn resolve_session_selector_rejects_ambiguous_prefix() {
        let (_dir, mut store) = open_temp_jsonl_store();
        let mut session_a = Session::new("codex".to_string());
        session_a.session_id = Uuid::parse_str("aaaaaaaa-1111-7111-8111-111111111111").unwrap();
        let mut session_b = Session::new("claude".to_string());
        session_b.session_id = Uuid::parse_str("aaaaaaaa-2222-7222-8222-222222222222").unwrap();
        store.save_session(&session_a).unwrap();
        store.save_session(&session_b).unwrap();

        let err = resolve_session_selector(&store, "aaaaaaaa").unwrap_err();
        assert!(err.contains("ambiguous"));
        assert!(err.contains(&session_a.session_id.to_string()));
        assert!(err.contains(&session_b.session_id.to_string()));
    }

    #[test]
    fn latest_session_id_uses_most_recent_updated_at() {
        let (_dir, mut store) = open_temp_jsonl_store();
        let mut older = Session::new("codex".to_string());
        older.updated_at = Utc::now() - Duration::minutes(10);
        let mut newer = Session::new("gemini".to_string());
        newer.updated_at = Utc::now();
        store.save_session(&older).unwrap();
        store.save_session(&newer).unwrap();

        let selected = latest_session_id(&store).unwrap();
        assert_eq!(selected, newer.session_id);
    }
}
