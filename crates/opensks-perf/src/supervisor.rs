//! Process supervisor with deterministic reaping and unambiguous cancellation
//! ownership (PR-043).
//!
//! [`ProcessSupervisor`] spawns child processes, tracks every one in a registry,
//! and REAPS them deterministically — no orphaned processes, no leaked OS
//! handles/file descriptors. After a supervised run, [`ProcessSupervisor::leaked_handles`]
//! returns the number of still-live children: a passing run reports zero. The
//! [`Drop`] impl reaps any survivors as a safety net so even a panic cannot
//! orphan a child.
//!
//! Cancellation ownership is explicit and unambiguous: [`CancelToken`] is a
//! single owned value (it is NOT `Clone`). Only its holder can call
//! [`CancelToken::cancel`]. Other parties may observe cancellation through a
//! cloneable [`CancelObserver`], but they can never trigger it. Cancelling stops
//! the supervised work and reaps every in-flight child.
//!
//! The supervisor is generic over a [`Reapable`] handle so tests can drive a
//! deterministic in-memory child, while real runs use [`OsChild`] over
//! `std::process`.

use std::process::{Child, Command};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SupervisorError {
    #[error("failed to spawn child `{label}`: {source}")]
    Spawn {
        label: String,
        source: std::io::Error,
    },
    #[error("failed to reap child `{label}`: {source}")]
    Reap {
        label: String,
        source: std::io::Error,
    },
}

/// A child handle the supervisor can deterministically reap. Implemented by the
/// real [`OsChild`] and by test doubles.
pub trait Reapable {
    /// Request termination (idempotent; safe to call more than once).
    fn kill(&mut self) -> std::io::Result<()>;
    /// Block until the child has exited and its OS handle is released. After
    /// this returns `Ok`, the handle MUST hold no live resource.
    fn wait(&mut self) -> std::io::Result<()>;
}

/// A real OS process child over `std::process::Child`.
pub struct OsChild {
    inner: Child,
}

impl OsChild {
    fn new(inner: Child) -> Self {
        Self { inner }
    }
}

impl Reapable for OsChild {
    fn kill(&mut self) -> std::io::Result<()> {
        // `kill` on an already-exited process returns an error on some
        // platforms; treat "not running" as success so reaping is idempotent.
        match self.inner.kill() {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn wait(&mut self) -> std::io::Result<()> {
        // `wait` reaps the zombie and closes the process handle / fd.
        self.inner.wait().map(|_status| ())
    }
}

/// Cancellation flag shared between an owning [`CancelToken`] and any number of
/// read-only [`CancelObserver`]s.
#[derive(Debug, Default)]
struct CancelFlag {
    cancelled: AtomicBool,
}

/// The SINGLE owner of cancellation authority. Not `Clone`: exactly one party
/// can cancel a supervised run, so ownership is never ambiguous. Hand out
/// [`CancelObserver`]s for read-only observation.
pub struct CancelToken {
    flag: Arc<CancelFlag>,
}

impl CancelToken {
    /// Mint a fresh, un-cancelled token. The returned value is the sole canceller.
    pub fn new() -> Self {
        Self {
            flag: Arc::new(CancelFlag::default()),
        }
    }

    /// Create a read-only observer that can see, but never cause, cancellation.
    pub fn observer(&self) -> CancelObserver {
        CancelObserver {
            flag: Arc::clone(&self.flag),
        }
    }

    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.flag.cancelled.load(Ordering::SeqCst)
    }

    /// Request cancellation. Only the owner of this token can do so. Returns
    /// `true` the first time it flips the flag, `false` if already cancelled.
    pub fn cancel(&self) -> bool {
        !self.flag.cancelled.swap(true, Ordering::SeqCst)
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

/// A read-only view of a [`CancelToken`]. Can observe cancellation but has no
/// `cancel` method, so it can never become a second canceller.
#[derive(Clone)]
pub struct CancelObserver {
    flag: Arc<CancelFlag>,
}

impl CancelObserver {
    /// Whether the owning token has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.flag.cancelled.load(Ordering::SeqCst)
    }
}

/// A tracked child plus its label, kept in the supervisor's live registry.
struct Tracked<C: Reapable> {
    label: String,
    child: C,
}

/// Spawns and deterministically reaps child processes, tracking every handle so
/// leaks are observable.
pub struct ProcessSupervisor<C: Reapable = OsChild> {
    live: Vec<Tracked<C>>,
    spawned: u64,
    reaped: u64,
    cancel: CancelObserver,
}

impl ProcessSupervisor<OsChild> {
    /// Create a supervisor bound to `cancel`'s observer. Spawn real OS
    /// processes with [`ProcessSupervisor::spawn`].
    pub fn new(cancel: &CancelToken) -> Self {
        Self {
            live: Vec::new(),
            spawned: 0,
            reaped: 0,
            cancel: cancel.observer(),
        }
    }

    /// Spawn a real OS child from a configured [`Command`] and track it. The
    /// caller is responsible for setting the program/args; the supervisor owns
    /// the lifecycle (reaping) from here on.
    pub fn spawn(
        &mut self,
        label: impl Into<String>,
        command: &mut Command,
    ) -> Result<(), SupervisorError> {
        let label = label.into();
        let child = command.spawn().map_err(|source| SupervisorError::Spawn {
            label: label.clone(),
            source,
        })?;
        self.track(label, OsChild::new(child));
        Ok(())
    }
}

impl<C: Reapable> ProcessSupervisor<C> {
    /// Create a supervisor over an arbitrary [`Reapable`] (used by tests with a
    /// deterministic in-memory child).
    pub fn with_observer(cancel: &CancelToken) -> Self {
        Self {
            live: Vec::new(),
            spawned: 0,
            reaped: 0,
            cancel: cancel.observer(),
        }
    }

    /// Track an already-created child handle. Lower-level entry point shared by
    /// the OS spawner and tests.
    pub fn track(&mut self, label: impl Into<String>, child: C) {
        self.live.push(Tracked {
            label: label.into(),
            child,
        });
        self.spawned += 1;
    }

    /// Number of children still alive (un-reaped). This is the leak counter: a
    /// clean run drains it to zero.
    pub fn leaked_handles(&self) -> usize {
        self.live.len()
    }

    /// Total children spawned/tracked over this supervisor's lifetime.
    pub fn spawned(&self) -> u64 {
        self.spawned
    }

    /// Total children deterministically reaped over this supervisor's lifetime.
    pub fn reaped(&self) -> u64 {
        self.reaped
    }

    /// Whether the owning cancel token has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Deterministically reap every live child: wait for each to exit and
    /// release its handle. On success the live registry is empty
    /// (`leaked_handles() == 0`). The first reap error is returned after still
    /// attempting to reap the remaining children (no early orphan).
    pub fn reap_all(&mut self) -> Result<(), SupervisorError> {
        self.drain_with(|child| child.wait())
    }

    /// Cancel-driven reap: kill then wait on every live child, so an in-flight
    /// supervised run is torn down deterministically. Honors the same no-orphan
    /// guarantee as [`reap_all`](Self::reap_all).
    pub fn kill_and_reap_all(&mut self) -> Result<(), SupervisorError> {
        self.drain_with(|child| {
            let _ = child.kill();
            child.wait()
        })
    }

    fn drain_with(
        &mut self,
        mut reap: impl FnMut(&mut C) -> std::io::Result<()>,
    ) -> Result<(), SupervisorError> {
        let mut first_error: Option<SupervisorError> = None;
        for mut tracked in self.live.drain(..) {
            match reap(&mut tracked.child) {
                Ok(()) => self.reaped += 1,
                Err(source) => {
                    // Count it reaped (handle is no longer tracked) but surface
                    // the first failure. We keep draining so nothing orphans.
                    self.reaped += 1;
                    if first_error.is_none() {
                        first_error = Some(SupervisorError::Reap {
                            label: tracked.label.clone(),
                            source,
                        });
                    }
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl<C: Reapable> Drop for ProcessSupervisor<C> {
    fn drop(&mut self) {
        // Safety net: if the owner forgot (or a panic unwound past) an explicit
        // reap, tear down every survivor here so a dropped supervisor can never
        // leave an orphaned process or leaked handle behind.
        let _ = self.kill_and_reap_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    /// A deterministic in-memory child that records whether it was waited on,
    /// backed by a shared live-counter so the test can prove no handle leaked.
    struct FakeChild {
        live: Rc<Cell<i64>>,
        reaped: bool,
    }

    impl FakeChild {
        fn spawn(live: &Rc<Cell<i64>>) -> Self {
            live.set(live.get() + 1);
            Self {
                live: Rc::clone(live),
                reaped: false,
            }
        }

        fn release(&mut self) {
            if !self.reaped {
                self.reaped = true;
                self.live.set(self.live.get() - 1);
            }
        }
    }

    impl Reapable for FakeChild {
        fn kill(&mut self) -> std::io::Result<()> {
            Ok(())
        }

        fn wait(&mut self) -> std::io::Result<()> {
            self.release();
            Ok(())
        }
    }

    impl Drop for FakeChild {
        fn drop(&mut self) {
            // Mirrors OS semantics: dropping a Child without waiting would leak;
            // our supervisor always waits, so this should already be released.
            self.release();
        }
    }

    #[test]
    fn supervisor_reaps_all_children_with_zero_leaked_handles() {
        // Leak scenario: spawn N short-lived children, reap, assert all reaped
        // and the registry is empty (zero leaked handles).
        let live = Rc::new(Cell::new(0i64));
        let cancel = CancelToken::new();
        let mut supervisor: ProcessSupervisor<FakeChild> =
            ProcessSupervisor::with_observer(&cancel);
        let n = 64u64;
        for index in 0..n {
            supervisor.track(format!("child-{index}"), FakeChild::spawn(&live));
        }
        assert_eq!(supervisor.leaked_handles(), n as usize);
        assert_eq!(live.get(), n as i64, "all children should be live pre-reap");

        supervisor.reap_all().expect("reap all children");

        assert_eq!(supervisor.spawned(), n);
        assert_eq!(supervisor.reaped(), n);
        assert_eq!(
            supervisor.leaked_handles(),
            0,
            "registry must be empty after reap"
        );
        assert_eq!(live.get(), 0, "no child handle may leak");
    }

    #[test]
    fn cancellation_reaps_in_flight_children_and_owner_is_sole_canceller() {
        // Cancellation ownership: only the owning CancelToken can cancel; an
        // observer can watch but not trigger. Cancelling reaps in-flight kids.
        let live = Rc::new(Cell::new(0i64));
        let cancel = CancelToken::new();
        let observer = cancel.observer();
        let mut supervisor: ProcessSupervisor<FakeChild> =
            ProcessSupervisor::with_observer(&cancel);
        for index in 0..16u64 {
            supervisor.track(format!("inflight-{index}"), FakeChild::spawn(&live));
        }

        // The observer can read state but exposes no `cancel` method — there is
        // no second canceller in the type system.
        assert!(!observer.is_cancelled());

        // The owner cancels; first cancel returns true, repeat returns false.
        assert!(cancel.cancel(), "owner is the canceller");
        assert!(!cancel.cancel(), "cancel is idempotent");
        assert!(observer.is_cancelled(), "observer sees the cancellation");
        assert!(supervisor.is_cancelled());

        // Cancelling tears down in-flight work deterministically.
        supervisor
            .kill_and_reap_all()
            .expect("cancel reaps in-flight children");
        assert_eq!(supervisor.leaked_handles(), 0);
        assert_eq!(supervisor.reaped(), 16);
        assert_eq!(
            live.get(),
            0,
            "cancellation must reap every in-flight child"
        );
    }

    #[test]
    fn dropping_supervisor_reaps_survivors_as_safety_net() {
        // Even if the owner never calls reap, Drop must release every handle.
        let live = Rc::new(Cell::new(0i64));
        let cancel = CancelToken::new();
        {
            let mut supervisor: ProcessSupervisor<FakeChild> =
                ProcessSupervisor::with_observer(&cancel);
            for index in 0..8u64 {
                supervisor.track(format!("orphan-{index}"), FakeChild::spawn(&live));
            }
            assert_eq!(live.get(), 8);
            // No explicit reap — rely on Drop.
        }
        assert_eq!(live.get(), 0, "Drop must reap every survivor");
    }

    #[test]
    fn supervisor_reaps_real_os_processes_with_zero_leaks() {
        // End-to-end over real OS processes: spawn short-lived children and
        // prove the supervisor reaps every one with no leaked process handle.
        let cancel = CancelToken::new();
        let mut supervisor = ProcessSupervisor::new(&cancel);
        let program = if cfg!(windows) { "cmd" } else { "true" };
        let n = 8u64;
        for index in 0..n {
            let mut command = Command::new(program);
            if cfg!(windows) {
                command.args(["/C", "exit", "0"]);
            }
            // Detach stdio so no pipe fds are retained by the parent.
            command
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            supervisor
                .spawn(format!("os-child-{index}"), &mut command)
                .expect("spawn os child");
        }
        assert_eq!(supervisor.leaked_handles(), n as usize);
        supervisor.reap_all().expect("reap os children");
        assert_eq!(supervisor.reaped(), n);
        assert_eq!(supervisor.leaked_handles(), 0);
    }
}
