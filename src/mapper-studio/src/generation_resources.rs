//! Machine-local execution policy. Never part of a portable recipe or seed.
use rayon::ThreadPool;
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    sync::{Arc, Mutex, OnceLock, RwLock},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageCompute {
    #[default]
    Auto,
    Cpu,
    Gpu,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerrainMemoryMode {
    #[default]
    Auto,
    Manual,
}

pub const DEFAULT_RAM_TARGET_PERCENT: usize = 80;
pub const RAM_TARGET_RANGE: std::ops::RangeInclusive<usize> = 50..=95;

fn ram_headroom_mib(total: u64, available: u64, commit: u64, target: usize) -> usize {
    const MIB: u64 = 1024 * 1024;
    let used = total.saturating_sub(available);
    let target_bytes = total.saturating_mul(target as u64) / 100;
    let headroom = target_bytes.saturating_sub(used).min(commit);
    usize::try_from(headroom / MIB).unwrap_or(usize::MAX)
}

/// Additional memory we can commit while leaving room for Windows and other apps.
pub fn terrain_ram_headroom_mib(target: usize) -> Option<usize> {
    let (total, available, commit) = physical_memory()?;
    Some(ram_headroom_mib(total, available, commit, target))
}

#[cfg(windows)]
fn physical_memory() -> Option<(u64, u64, u64)> {
    papyrus_system_memory::physical_memory()
}

#[cfg(not(windows))]
fn physical_memory() -> Option<(u64, u64, u64)> {
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub cpu_workers: usize,
    /// None leaves admission to the CPU and terrain-memory budgets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrent_maps: Option<usize>,
    /// Aggregate live image job allocations, not the entire application's RAM.
    pub image_memory_mib: usize,
    pub terrain_memory_mib: usize,
    pub gpu_memory_mib: usize,
    pub image_compute: ImageCompute,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            cpu_workers: available_workers().div_ceil(2),
            concurrent_maps: None,
            image_memory_mib: 512,
            terrain_memory_mib: 2048,
            gpu_memory_mib: 256,
            image_compute: ImageCompute::Auto,
        }
    }
}
pub fn available_workers() -> usize {
    std::thread::available_parallelism().map_or(1, usize::from)
}
impl Limits {
    pub fn validate(&self) -> Result<(), String> {
        if self.concurrent_maps.is_some_and(|limit| !(1..=crate::generation_plan::MAX_BATCH).contains(&limit))
            || !(1..=1024).contains(&self.cpu_workers)
            || !(16..=4096).contains(&self.image_memory_mib)
            || !(256..=16384).contains(&self.terrain_memory_mib)
            || !(32..=2048).contains(&self.gpu_memory_mib)
        {
            return Err("Invalid generation resource limits".into());
        }
        Ok(())
    }
    pub fn effective_workers(self) -> usize {
        self.cpu_workers.min(available_workers())
    }
}
static LIMITS: OnceLock<RwLock<Limits>> = OnceLock::new();
static COMPUTE_JOB: Mutex<Option<ThreadPool>> = Mutex::new(None);
static STATUS: Mutex<String> = Mutex::new(String::new());
fn unconfigured_limits() -> Limits {
    // Library/CLI callers do not initialize a graphics device implicitly.
    // The desktop loads its saved Auto/CPU/GPU preference explicitly.
    Limits {
        image_compute: ImageCompute::Cpu,
        ..Limits::default()
    }
}
/// Holds an admitted map's managed terrain allowance until its workers exit.
#[derive(Clone, Debug)]
pub struct JobBudget(Arc<usize>);

thread_local! {
    static JOB_BUDGET: RefCell<Option<JobBudget>> = const { RefCell::new(None) };
}

impl JobBudget {
    pub fn new(terrain_mib: usize) -> Self {
        Self(Arc::new(terrain_mib))
    }

    pub fn terrain_mib(&self) -> usize {
        *self.0
    }

    pub fn has_workers(&self) -> bool {
        Arc::strong_count(&self.0) > 1
    }
}

pub fn with_job_budget<T>(budget: Option<JobBudget>, work: impl FnOnce() -> T) -> T {
    struct Restore(Option<JobBudget>);
    impl Drop for Restore {
        fn drop(&mut self) {
            JOB_BUDGET.replace(self.0.take());
        }
    }
    let _restore = Restore(JOB_BUDGET.replace(budget));
    work()
}

/// Carry the submitting map's allowance into an asynchronous stage.
pub fn job_worker<T>(work: impl FnOnce() -> T) -> impl FnOnce() -> T {
    let budget = JOB_BUDGET.with(|slot| slot.borrow().clone());
    move || with_job_budget(budget, work)
}

pub fn current() -> Limits {
    let mut limits = *LIMITS
        .get_or_init(|| RwLock::new(unconfigured_limits()))
        .read()
        .unwrap_or_else(|e| e.into_inner());
    JOB_BUDGET.with(|slot| {
        if let Some(budget) = slot.borrow().as_ref() {
            limits.terrain_memory_mib = budget.terrain_mib();
        }
    });
    limits
}
pub fn configure(limits: Limits) -> Result<(), String> {
    limits.validate()?;
    *LIMITS
        .get_or_init(|| RwLock::new(unconfigured_limits()))
        .write()
        .unwrap_or_else(|e| e.into_inner()) = limits;
    Ok(())
}
pub fn status() -> String {
    STATUS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}
pub(crate) fn report(message: String) {
    *STATUS.lock().unwrap_or_else(|e| e.into_inner()) = message;
}
/// Serializes CPU generation and complete image jobs, so concurrent previews/exports cannot each
/// create their own pools or exceed the image allocation allowance. Cancelled
/// queued work can leave without waiting for the current image to finish.
/// The callback must not recursively request another compute job.
pub fn with_cpu_pool<T>(
    cancelled: impl Fn() -> bool,
    run: impl FnOnce(&ThreadPool, Limits) -> Result<Option<T>, String>,
) -> Result<Option<T>, String> {
    let mut job = loop {
        if cancelled() {
            return Ok(None);
        }
        match COMPUTE_JOB.try_lock() {
            Ok(job) => break job,
            Err(std::sync::TryLockError::Poisoned(e)) => break e.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                std::thread::sleep(std::time::Duration::from_millis(5))
            }
        }
    };
    let limits = current();
    run(prepare_pool(&mut job, limits)?, limits)
}

/// Allow background validation to use idle workers without waiting for a
/// running generation or image job. Callers can continue serially when busy.
pub fn try_with_cpu_pool<T>(
    run: impl FnOnce(&ThreadPool) -> Result<T, String>,
) -> Result<Option<T>, String> {
    let mut job = match COMPUTE_JOB.try_lock() {
        Ok(job) => job,
        Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return Ok(None),
    };
    let Ok(pool) = prepare_pool(&mut job, current()) else {
        return Ok(None);
    };
    run(pool).map(Some)
}

fn prepare_pool(job: &mut Option<ThreadPool>, limits: Limits) -> Result<&ThreadPool, String> {
    if job
        .as_ref()
        .is_none_or(|p| p.current_num_threads() != limits.effective_workers())
    {
        // Drop the old workers before constructing their replacement.
        *job = None;
        *job = Some(
            rayon::ThreadPoolBuilder::new()
                .num_threads(limits.effective_workers())
                .thread_name(|i| format!("mapper-image-{i}"))
                .build()
                .map_err(|e| format!("Could not start image workers: {e}"))?,
        );
    }
    Ok(job.as_ref().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn auto_terrain_headroom_tracks_ram_pressure_and_commit_limit() {
        const MIB: u64 = 1024 * 1024;
        let total = 32_000 * MIB;
        assert_eq!(ram_headroom_mib(total, 14_000 * MIB, u64::MAX, 80), 7_600);
        assert_eq!(ram_headroom_mib(total, 6_000 * MIB, u64::MAX, 80), 0);
        assert_eq!(ram_headroom_mib(total, 14_000 * MIB, 2048 * MIB, 80), 2048);
        assert_eq!(ram_headroom_mib(total, 14_000 * MIB, u64::MAX, 70), 4_400);
    }

    #[test]
    fn terrain_memory_upgrades_saved_limits_and_validates_its_own_range() {
        let mut limits: Limits = serde_json::from_str(
            r#"{"cpu_workers":1,"image_memory_mib":512,"gpu_memory_mib":256,"image_compute":"cpu"}"#,
        )
        .unwrap();
        assert_eq!(limits.terrain_memory_mib, 2048);
        limits.validate().unwrap();
        for invalid in [255, 16385] {
            limits.terrain_memory_mib = invalid;
            assert!(limits.validate().is_err());
        }
    }

    #[test]
    fn limits_upgrade_old_documents_and_reject_impossible_budgets() {
        assert_eq!(
            serde_json::from_str::<Limits>("{}").unwrap(),
            Limits::default()
        );
        let mut limits = Limits::default();
        assert!(limits.validate().is_ok());
        limits.cpu_workers = 0;
        assert!(limits.validate().is_err());
        limits.cpu_workers = 1024;
        assert!(limits.effective_workers() <= available_workers());
        limits.image_memory_mib = 0;
        assert!(limits.validate().is_err());
    }
    #[test]
    fn concurrent_image_jobs_serialize_and_cancelled_requests_do_not_run() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let active = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..3 {
                let active = &active;
                scope.spawn(move || {
                    with_cpu_pool(
                        || false,
                        |pool, limits| {
                            assert_eq!(active.fetch_add(1, Ordering::SeqCst), 0);
                            assert_eq!(pool.current_num_threads(), limits.effective_workers());
                            assert!(try_with_cpu_pool::<()>(|_| panic!("busy pool was acquired"))
                                .unwrap()
                                .is_none());
                            std::thread::sleep(std::time::Duration::from_millis(10));
                            assert_eq!(active.fetch_sub(1, Ordering::SeqCst), 1);
                            Ok(Some(()))
                        },
                    )
                    .unwrap();
                });
            }
        });
        assert!(
            with_cpu_pool::<()>(|| true, |_, _| panic!("cancelled request ran"))
                .unwrap()
                .is_none()
        );
    }
}
