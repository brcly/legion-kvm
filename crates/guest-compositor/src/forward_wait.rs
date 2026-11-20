//! Opt-in event waits for the forwarding loops; GPU fences remain polled.
use std::{os::fd::BorrowedFd, time::Instant};

use anyhow::{Result, ensure};
use rustix::event::{PollFd, PollFlags, poll};
use serde_json::{Value, json};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Spin,
    Events,
    Hybrid,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Spin => "spin",
            Self::Events => "events",
            Self::Hybrid => "hybrid",
        }
    }
}

pub(crate) struct ForwardWait {
    mode: Mode,
    yield_calls: u64,
    calls: u64,
    timeouts: u64,
    elapsed_ns: u128,
    cpu_start_ns: u64,
    idle_retry_us: u64,
}

fn process_cpu_ns() -> Result<u64> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes one initialized, live timespec and retains no pointer.
    let status = unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut time) };
    ensure!(
        status == 0,
        "read compositor CPU clock: {}",
        std::io::Error::last_os_error()
    );
    Ok(u64::try_from(time.tv_sec)? * 1_000_000_000 + u64::try_from(time.tv_nsec)?)
}

impl ForwardWait {
    pub fn new(config: &Value) -> Result<Self> {
        let mode = config
            .get("forward_wait")
            .map_or(Some("spin"), Value::as_str);
        let mode = match mode {
            Some("spin") => Mode::Spin,
            Some("events") => Mode::Events,
            Some("hybrid") => Mode::Hybrid,
            _ => anyhow::bail!("unknown forwarding wait mode"),
        };
        let idle_retry_us = config
            .get("forward_idle_retry_us")
            .map_or(Some(1000), Value::as_u64)
            .filter(|value| (100..=1000).contains(value))
            .ok_or_else(|| anyhow::anyhow!("idle retry must be 100..=1000 microseconds"))?;
        Ok(Self {
            mode,
            yield_calls: 0,
            calls: 0,
            timeouts: 0,
            elapsed_ns: 0,
            cpu_start_ns: process_cpu_ns()?,
            idle_retry_us,
        })
    }

    pub fn wait<const N: usize>(
        &mut self,
        descriptors: [BorrowedFd<'_>; N],
        gpu_pending: bool,
    ) -> Result<()> {
        if self.mode == Mode::Spin || (self.mode == Mode::Hybrid && gpu_pending) {
            // Pending acquisition already has real GPU work in flight.
            self.yield_calls += 1;
            std::thread::yield_now();
            return Ok(());
        }
        ensure!(
            !descriptors.is_empty(),
            "forwarding wait has no descriptors"
        );
        let mut fds = descriptors.map(|fd| PollFd::from_borrowed_fd(fd, PollFlags::IN));
        self.poll_descriptors(&mut fds, gpu_pending)
    }

    pub fn wait_with_acquires<'fd, const N: usize>(
        &mut self,
        descriptors: [BorrowedFd<'fd>; N],
        acquires: impl Iterator<Item = BorrowedFd<'fd>>,
    ) -> Result<usize> {
        let mut fds: Vec<_> = descriptors
            .into_iter()
            .chain(acquires)
            .map(|fd| PollFd::from_borrowed_fd(fd, PollFlags::IN))
            .collect();
        self.poll_descriptors(&mut fds, false)?;
        Ok(fds[N..]
            .iter()
            .filter(|fd| fd.revents().contains(PollFlags::IN))
            .count())
    }

    fn poll_descriptors(&mut self, fds: &mut [PollFd<'_>], gpu_pending: bool) -> Result<()> {
        ensure!(!fds.is_empty(), "forwarding wait has no descriptors");
        // GL acquisition fences have no pollable FD in the current path.
        let timeout = rustix::time::Timespec {
            tv_sec: 0,
            tv_nsec: if gpu_pending {
                100_000
            } else {
                self.idle_retry_us as i64 * 1000
            },
        };
        let started = Instant::now();
        match poll(fds, Some(&timeout)) {
            Ok(ready) => {
                self.timeouts += u64::from(ready == 0);
                ensure!(
                    fds.iter().all(|fd| !fd
                        .revents()
                        .intersects(PollFlags::ERR | PollFlags::HUP | PollFlags::NVAL)),
                    "forwarding wait descriptor disconnected"
                );
            }
            Err(rustix::io::Errno::INTR) => {}
            Err(error) => return Err(error.into()),
        }
        self.calls += 1;
        self.elapsed_ns += started.elapsed().as_nanos();
        Ok(())
    }

    pub fn report(&self) -> Result<Value> {
        Ok(
            json!({"mode":self.mode.name(),"yield_calls":self.yield_calls,
            "wait_calls":self.calls,"timeouts":self.timeouts,
            "wait_seconds":self.elapsed_ns as f64 / 1e9,
            "process_cpu_seconds":process_cpu_ns()?.saturating_sub(self.cpu_start_ns) as f64 / 1e9,
            "gpu_retry_us":if self.mode == Mode::Events {100} else {0},"idle_retry_us":self.idle_retry_us}),
        )
    }
}

#[cfg(test)]
#[path = "../tests/unit/forward_wait.rs"]
mod tests;
