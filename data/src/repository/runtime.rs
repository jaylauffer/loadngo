//! Per-command proactor, thermal admission, and bounded CPU workers for repository I/O.

use super::{SignedHead, Signer};
use crate::archive_cas::worker::{ArchiveCasWorkerPool, WorkerTask};
use crate::archive_cas::ArchiveCasRestoreIo;
use crate::cas::CasHash;
use anyhow::{Context, Result};
use loadngo_pq_crypto::PrivateKey;
use loadngo_proactor::{
    new_platform_proactor, CompletionKind, PlatformPort, Proactor, ProactorHandle,
};
use loadngo_thermal::{
    platform_provider, ThermalGovernor, ThermalProvider, ThermalRecommendation, ThermalSnapshot,
};
use std::io;
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

const FAIR_WORK_DELAY: Duration = Duration::from_millis(10);
const SERIOUS_WORK_DELAY: Duration = Duration::from_millis(100);

/// Owns the event loop and bounded resources for one repository command.
pub struct RepositoryRuntime {
    proactor: Proactor<PlatformPort>,
    handle: ProactorHandle<PlatformPort>,
    thermal: ThermalAdmission,
    workers: ArchiveCasWorkerPool,
    blocking_io: Option<ArchiveCasWorkerPool>,
}

/// Runtime adapter used by Archive CAS restores for bounded, thermally admitted
/// file lifecycle calls while the command thread continues dispatching proactor I/O.
pub struct RepositoryArchiveCasRestoreIo<'a> {
    proactor: &'a Proactor<PlatformPort>,
    handle: &'a ProactorHandle<PlatformPort>,
    thermal: &'a mut ThermalAdmission,
    blocking_io: &'a mut ArchiveCasWorkerPool,
}

impl<'a> RepositoryArchiveCasRestoreIo<'a> {
    pub fn new(
        proactor: &'a Proactor<PlatformPort>,
        handle: &'a ProactorHandle<PlatformPort>,
        thermal: &'a mut ThermalAdmission,
        blocking_io: &'a mut ArchiveCasWorkerPool,
    ) -> Self {
        Self {
            proactor,
            handle,
            thermal,
            blocking_io,
        }
    }
}

impl ArchiveCasRestoreIo<PlatformPort> for RepositoryArchiveCasRestoreIo<'_> {
    fn admit(
        &mut self,
        proactor: &Proactor<PlatformPort>,
        handle: &ProactorHandle<PlatformPort>,
    ) -> Result<()> {
        self.thermal
            .admit(proactor, handle)
            .context("thermal admission failed before CAS restore operation")
    }

    fn run_blocking<T, F>(&mut self, work: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        while self.blocking_io.is_saturated() {
            self.proactor.run_once().context(
                "failed to dispatch CAS restore I/O completion while awaiting worker capacity",
            )?;
            self.thermal.observe_if_due(false);
        }
        let mut task = self
            .blocking_io
            .try_submit(self.handle, work)
            .context("could not submit CAS restore file operation")?;
        loop {
            if let Some(result) = task.try_take() {
                return result.map_err(anyhow::Error::new)?;
            }
            self.proactor
                .run_once()
                .context("failed to dispatch CAS restore I/O completion")?;
            self.thermal.observe_if_due(false);
        }
    }
}

impl RepositoryRuntime {
    /// Creates a platform proactor, thermal provider, and one-worker bounded lane.
    pub fn new() -> Result<Self> {
        let proactor = new_platform_proactor().context("failed to create platform proactor")?;
        let handle = proactor.handle();
        let workers = ArchiveCasWorkerPool::new(1, 1)
            .context("failed to create bounded repository worker lane")?;
        Ok(Self {
            proactor,
            handle,
            thermal: ThermalAdmission::new(),
            workers,
            blocking_io: None,
        })
    }

    /// Borrows the I/O driver, its handle, thermal admission, and CPU lane as disjoint parts.
    pub fn parts_mut(
        &mut self,
    ) -> (
        &Proactor<PlatformPort>,
        &ProactorHandle<PlatformPort>,
        &mut ThermalAdmission,
        &mut ArchiveCasWorkerPool,
    ) {
        (
            &self.proactor,
            &self.handle,
            &mut self.thermal,
            &mut self.workers,
        )
    }

    /// Borrows the runtime parts needed to alternate proactor reads with writes on
    /// the bounded blocking-I/O lane.
    pub fn blocking_io_parts_mut(
        &mut self,
    ) -> Result<(
        &Proactor<PlatformPort>,
        &ProactorHandle<PlatformPort>,
        &mut ThermalAdmission,
        &mut ArchiveCasWorkerPool,
    )> {
        if self.blocking_io.is_none() {
            self.blocking_io = Some(
                ArchiveCasWorkerPool::new(1, 1)
                    .context("failed to create bounded repository blocking-I/O lane")?,
            );
        }
        let blocking_io = self
            .blocking_io
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("repository blocking-I/O lane is unavailable"))?;
        Ok((&self.proactor, &self.handle, &mut self.thermal, blocking_io))
    }

    /// Dispatches ready completions and updates the coarse thermal sample.
    pub fn service_ready(&mut self) -> io::Result<()> {
        self.proactor.run_once_until(Instant::now())?;
        self.thermal.observe_if_due(false);
        Ok(())
    }

    /// Samples current pressure, admitted work as appropriate, and returns the latest snapshot.
    pub fn admit_optional_work(&mut self) -> io::Result<ThermalSnapshot> {
        let (proactor, handle, thermal, _) = self.parts_mut();
        thermal.admit(proactor, handle)?;
        Ok(thermal.snapshot().clone())
    }

    /// Submits CPU work after thermal admission; the queue and total in-flight count are bounded.
    pub fn try_submit_cpu<T, F>(&mut self, work: F) -> Result<WorkerTask<T>>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let (proactor, handle, thermal, workers) = self.parts_mut();
        thermal
            .admit(proactor, handle)
            .context("thermal admission failed before repository CPU work")?;
        workers
            .try_submit(handle, work)
            .map_err(anyhow::Error::new)
            .context("could not submit bounded repository CPU work")
    }

    /// Drives completions until a submitted worker result is ready.
    pub fn wait_cpu<T>(&mut self, mut task: WorkerTask<T>) -> Result<T> {
        loop {
            if let Some(result) = task.try_take() {
                return result.map_err(anyhow::Error::new);
            }
            self.proactor
                .run_once()
                .context("failed to dispatch repository worker completion")?;
            self.thermal.observe_if_due(false);
        }
    }

    /// Runs one required blocking stream or file operation away from the command
    /// thread, while continuing to dispatch proactor completions. This lane is
    /// separate from CPU work and is bounded to one running or queued task.
    pub fn run_blocking_io<T, F>(&mut self, work: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        if self.blocking_io.is_none() {
            self.blocking_io = Some(
                ArchiveCasWorkerPool::new(1, 1)
                    .context("failed to create bounded repository blocking-I/O lane")?,
            );
        }
        let mut task = self
            .blocking_io
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("repository blocking-I/O lane is unavailable"))?
            .try_submit(&self.handle, work)
            .context("could not submit bounded repository blocking I/O")?;
        loop {
            if let Some(result) = task.try_take() {
                return result.map_err(anyhow::Error::new);
            }
            self.proactor
                .run_once()
                .context("failed to dispatch repository blocking-I/O completion")?;
            self.thermal.observe_if_due(false);
        }
    }

    /// Signs a repository head on the thermally admitted bounded CPU lane.
    /// The copied secret key bytes remain zeroizing while queued and are moved
    /// into the worker's `PrivateKey` before signing.
    pub fn sign_head(
        &mut self,
        repository_id: &str,
        branch: &str,
        version: CasHash,
        signer: &Signer,
        signed_at_unix_secs: u64,
    ) -> Result<SignedHead> {
        let repository_id = repository_id.to_string();
        let branch = branch.to_string();
        let identity = signer.identity.clone();
        let public = signer.public.clone();
        let scheme = signer.private.scheme;
        let mut private_bytes = Zeroizing::new(signer.private.bytes.to_vec());
        let task = self.try_submit_cpu(move || {
            let private = PrivateKey::new(scheme, std::mem::take(&mut *private_bytes))?;
            let signer = Signer {
                identity,
                public,
                private,
            };
            SignedHead::sign(
                &repository_id,
                &branch,
                version,
                &signer,
                signed_at_unix_secs,
            )
        })?;
        self.wait_cpu(task)?
    }

    /// Verifies a signed repository head on the thermally admitted bounded CPU lane.
    pub fn verify_head(&mut self, head: &SignedHead, trust: &super::Trust) -> Result<CasHash> {
        self.verify_head_candidate(head, trust)?
    }

    /// Separates a rejected signature from failures in thermal admission, worker
    /// submission, or proactor completion while checking a candidate head.
    pub fn verify_head_candidate(
        &mut self,
        head: &SignedHead,
        trust: &super::Trust,
    ) -> Result<Result<CasHash>> {
        let head = head.clone();
        let trust = trust.clone();
        let task = self.try_submit_cpu(move || head.verify(&trust))?;
        self.wait_cpu(task)
    }
}

impl Drop for RepositoryRuntime {
    fn drop(&mut self) {
        // Stop producers first. Joining each bounded lane guarantees that every
        // worker has finished posting its completion before the event loop is
        // stopped and drains outstanding proactor I/O.
        self.workers.shutdown();
        if let Some(blocking_io) = &mut self.blocking_io {
            blocking_io.shutdown();
        }

        // Worker jobs post their capacity-release callbacks to the proactor.
        // Drain those immediate completions before stopping it; the port's
        // shutdown drain is specifically for outstanding kernel I/O.
        loop {
            match self.proactor.run_ready() {
                Ok(report)
                    if report.dispatched_completions == 0
                        && report.dispatched_deferred == 0
                        && !report.woke =>
                {
                    break;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }

        // Drop cannot report shutdown errors. The proactor and its backend
        // still perform their own last-resort cleanup if either call fails.
        let _ = self.handle.stop();
        let _ = self.proactor.run_until_stopped();
    }
}

/// Coarse, event-driven thermal admission shared by repository operations.
pub struct ThermalAdmission {
    provider: Box<dyn ThermalProvider>,
    governor: ThermalGovernor,
    samples: u64,
    next_sample: Instant,
}

impl ThermalAdmission {
    pub fn new() -> Self {
        let mut provider = platform_provider();
        let mut governor = ThermalGovernor::new(Default::default());
        let now = Instant::now();
        governor.observe(&provider.observe(), now);
        let next_sample = now + governor.snapshot().pressure.sample_interval();
        Self {
            provider,
            governor,
            samples: 1,
            next_sample,
        }
    }

    pub fn snapshot(&self) -> &ThermalSnapshot {
        self.governor.snapshot()
    }

    pub fn samples(&self) -> u64 {
        self.samples
    }

    pub fn observe_if_due(&mut self, force: bool) {
        let now = Instant::now();
        if force || now >= self.next_sample {
            self.governor.observe(&self.provider.observe(), now);
            self.samples += 1;
            self.next_sample = now + self.governor.snapshot().pressure.sample_interval();
        }
    }

    pub fn admit(
        &mut self,
        proactor: &Proactor<PlatformPort>,
        handle: &ProactorHandle<PlatformPort>,
    ) -> io::Result<()> {
        self.observe_if_due(false);
        loop {
            match self.governor.snapshot().recommendation {
                ThermalRecommendation::Baseline => return Ok(()),
                ThermalRecommendation::ReduceOptional => {
                    wait_for_deadline(proactor, handle, FAIR_WORK_DELAY)?;
                    self.observe_if_due(false);
                    match self.governor.snapshot().recommendation {
                        ThermalRecommendation::Baseline | ThermalRecommendation::ReduceOptional => {
                            return Ok(())
                        }
                        ThermalRecommendation::MinimizeOptional
                        | ThermalRecommendation::PauseOptional => continue,
                    }
                }
                ThermalRecommendation::MinimizeOptional => {
                    wait_for_deadline(proactor, handle, SERIOUS_WORK_DELAY)?;
                    self.observe_if_due(false);
                    match self.governor.snapshot().recommendation {
                        ThermalRecommendation::Baseline
                        | ThermalRecommendation::ReduceOptional
                        | ThermalRecommendation::MinimizeOptional => return Ok(()),
                        ThermalRecommendation::PauseOptional => continue,
                    }
                }
                ThermalRecommendation::PauseOptional => {
                    let delay = self.next_sample.saturating_duration_since(Instant::now());
                    wait_for_deadline(proactor, handle, delay.max(Duration::from_millis(1)))?;
                    self.observe_if_due(true);
                }
            }
        }
    }
}

impl Default for ThermalAdmission {
    fn default() -> Self {
        Self::new()
    }
}

fn wait_for_deadline(
    proactor: &Proactor<PlatformPort>,
    handle: &ProactorHandle<PlatformPort>,
    delay: Duration,
) -> io::Result<()> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    handle.defer_for(delay, CompletionKind::Timer, 0, move |_| {
        let _ = sender.send(());
    })?;
    loop {
        match receiver.try_recv() {
            Ok(()) => return Ok(()),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "thermal deadline completion channel closed",
                ));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                proactor.run_once()?;
            }
        }
    }
}
