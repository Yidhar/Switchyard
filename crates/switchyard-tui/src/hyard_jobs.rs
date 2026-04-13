use std::path::Path;

#[cfg(test)]
use switchyard_host_jobs::list_job_summaries_with;
use switchyard_host_jobs::{HostJobSource, list_job_summaries};

use crate::state::{HyardJobSource, HyardJobSummary};

const MAX_HYARD_JOBS: usize = 8;

pub fn read_hyard_job_summaries(job_dir: &Path) -> Vec<HyardJobSummary> {
    map_hyard_summaries(list_job_summaries(job_dir, MAX_HYARD_JOBS))
}

#[cfg(test)]
fn read_hyard_job_summaries_with<F>(job_dir: &Path, is_process_alive: F) -> Vec<HyardJobSummary>
where
    F: Fn(u32) -> bool,
{
    map_hyard_summaries(list_job_summaries_with(
        job_dir,
        MAX_HYARD_JOBS,
        is_process_alive,
    ))
}

fn map_hyard_summaries(jobs: Vec<switchyard_host_jobs::HostJobSummary>) -> Vec<HyardJobSummary> {
    jobs.into_iter()
        .map(|job| HyardJobSummary {
            job_id: job.job_id,
            provider: job.provider,
            status: job.status,
            last_event: job.last_event,
            last_output_preview: job.last_output_preview,
            execution: job.execution,
            wait_timeout_count: job.wait_timeout_count,
            artifact_count: job.artifact_count,
            result_ready: job.result_ready,
            error: job.error,
            updated_at: job.updated_at,
            source: match job.source {
                HostJobSource::Live => HyardJobSource::Live,
                HostJobSource::Persisted => HyardJobSource::Store,
                HostJobSource::Recovered => HyardJobSource::Reconciled,
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use switchyard_host_jobs::{HostJobState, HostJobStatus, HostJobStore};

    use super::*;

    #[test]
    fn read_hyard_job_summaries_sorts_active_before_completed() {
        let dir = tempfile::tempdir().unwrap();
        let store = HostJobStore::new(dir.path().to_path_buf());

        let mut completed = HostJobState::new("claude", "task", PathBuf::from("."));
        completed.status = HostJobStatus::Completed;
        store.save(&completed).unwrap();

        let mut running = HostJobState::new("codex", "task", PathBuf::from("."));
        running.status = HostJobStatus::Running;
        running.wait_timeout_count = 1;
        store.save(&running).unwrap();

        let jobs = read_hyard_job_summaries(dir.path());
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].job_id, running.job_id.to_string());
        assert_eq!(jobs[1].job_id, completed.job_id.to_string());
    }

    #[test]
    fn read_hyard_job_summaries_marks_reconciled_missing_worker() {
        let dir = tempfile::tempdir().unwrap();
        let store = HostJobStore::new(dir.path().to_path_buf());

        let mut queued = HostJobState::new("claude", "task", PathBuf::from("."));
        queued.status = HostJobStatus::Queued;
        queued.pid = Some(u32::MAX);
        queued.wait_timeout_count = 1;
        store.save(&queued).unwrap();

        let jobs = read_hyard_job_summaries_with(dir.path(), |_| false);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].status, "failed");
        assert_eq!(jobs[0].source, HyardJobSource::Reconciled);
        assert_eq!(jobs[0].last_event.as_deref(), Some("worker_missing"));
    }
}
