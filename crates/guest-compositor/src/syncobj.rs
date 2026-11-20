//! Timeline readiness and release ownership for surface commits.
use anyhow::{Context, Result, ensure};
use rustix::event::{PollFd, PollFlags, poll};
use smithay::{
    reexports::wayland_server::{Resource, protocol::wl_surface::WlSurface},
    wayland::{
        compositor::{Barrier, add_blocker, with_states},
        drm_syncobj::{DrmSyncPoint, DrmSyncobjCachedState},
    },
};
use std::{
    os::fd::{AsFd, BorrowedFd, OwnedFd},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

static COMMITS: AtomicU64 = AtomicU64::new(0);
static WATCHED: AtomicU64 = AtomicU64::new(0);
static RESOLVED: AtomicU64 = AtomicU64::new(0);
static RELEASED: AtomicU64 = AtomicU64::new(0);

pub(crate) struct Release(DrmSyncPoint);
impl Release {
    pub fn signal(self) -> Result<()> {
        self.0.signal().context("signal source release timeline")?;
        RELEASED.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}
pub(crate) fn report() -> serde_json::Value {
    serde_json::json!({"commits":COMMITS.load(Ordering::Relaxed),"released":RELEASED.load(Ordering::Relaxed),"watched":WATCHED.load(Ordering::Relaxed),"resolved":RESOLVED.load(Ordering::Relaxed)})
}
pub(crate) fn validate_releases() -> Result<()> {
    ensure!(
        COMMITS.load(Ordering::Relaxed) == RELEASED.load(Ordering::Relaxed),
        "surface timeline releases remain outstanding"
    );
    Ok(())
}
pub(crate) fn take_release(
    states: &smithay::wayland::compositor::SurfaceData,
) -> Result<Option<Release>> {
    let (acquire, release) = {
        let mut cached = states.cached_state.get::<DrmSyncobjCachedState>();
        let state = cached.current();
        (state.acquire_point.take(), state.release_point.take())
    };
    match (acquire, release) {
        (None, None) => Ok(None),
        (Some(acquire), Some(release)) => {
            acquire
                .wait(0)
                .context("surface commit preceded acquire completion")?;
            COMMITS.fetch_add(1, Ordering::Relaxed);
            Ok(Some(Release(release)))
        }
        _ => anyhow::bail!("incomplete surface synchronization points"),
    }
}

pub(crate) struct Watch {
    pub surface: WlSurface,
    event: Arc<OwnedFd>,
    barrier: Barrier,
}
impl Watch {
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.event.as_fd()
    }
    pub fn pending(surface: &WlSurface) -> Result<Option<Self>> {
        let point = with_states(surface, |states| {
            states
                .cached_state
                .get::<DrmSyncobjCachedState>()
                .pending()
                .acquire_point
                .clone()
        });
        let Some(point) = point else {
            return Ok(None);
        };
        let watch = Self {
            surface: surface.clone(),
            event: point.eventfd().context("watch acquire timeline")?,
            barrier: Barrier::new(false),
        };
        if watch.ready()? {
            return Ok(None);
        }
        add_blocker(surface, watch.barrier.clone());
        WATCHED.fetch_add(1, Ordering::Relaxed);
        Ok(Some(watch))
    }
    pub fn ready(&self) -> Result<bool> {
        if !self.surface.is_alive() {
            return Ok(true);
        }
        event_ready(self.event.as_ref())
    }
    pub fn release(&self) {
        self.barrier.signal();
        RESOLVED.fetch_add(1, Ordering::Relaxed);
    }
}

fn event_ready(fd: &impl std::os::fd::AsFd) -> Result<bool> {
    let mut events = [PollFd::new(fd, PollFlags::IN)];
    poll(
        &mut events,
        Some(&rustix::time::Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        }),
    )?;
    ensure!(
        !events[0]
            .revents()
            .intersects(PollFlags::ERR | PollFlags::HUP | PollFlags::NVAL),
        "acquire timeline watch failed"
    );
    Ok(events[0].revents().contains(PollFlags::IN))
}

pub(crate) fn verify_device(device: &smithay::backend::drm::DrmDeviceFd) -> Result<()> {
    use drm::control::Device;
    struct Syncobj<'a>(
        &'a smithay::backend::drm::DrmDeviceFd,
        drm::control::syncobj::Handle,
    );
    impl Drop for Syncobj<'_> {
        fn drop(&mut self) {
            let _ = self.0.destroy_syncobj(self.1);
        }
    }
    let producer = Syncobj(
        device,
        device
            .create_syncobj(false)
            .context("create qualification syncobj")?,
    );
    let exported = device
        .syncobj_to_fd(producer.1, false)
        .context("export qualification timeline")?;
    use smithay::{
        backend::drm::DrmDeviceFd,
        utils::{DevPath, DeviceFd},
    };
    use std::os::fd::AsFd;
    let path = device
        .dev_path()
        .context("timeline device path is unavailable")?;
    let peer_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    let peer = DrmDeviceFd::new(DeviceFd::from(OwnedFd::from(peer_file)));
    let imported = Syncobj(
        &peer,
        peer.fd_to_syncobj(exported.as_fd(), false)
            .context("import qualification timeline")?,
    );
    let event = rustix::event::eventfd(
        0,
        rustix::event::EventfdFlags::CLOEXEC | rustix::event::EventfdFlags::NONBLOCK,
    )?;
    peer.syncobj_eventfd(imported.1, 7, event.as_fd(), false)
        .context("watch qualification timeline")?;
    let ready = || -> Result<bool> {
        let mut fds = [PollFd::new(&event, PollFlags::IN)];
        poll(
            &mut fds,
            Some(&rustix::time::Timespec {
                tv_sec: 0,
                tv_nsec: 0,
            }),
        )?;
        Ok(fds[0].revents().contains(PollFlags::IN))
    };
    ensure!(!ready()?, "timeline event signaled before submission");
    device
        .syncobj_timeline_signal(&[producer.1], &[7])
        .context("signal qualification timeline")?;
    ensure!(ready()?, "timeline event did not follow signal");
    peer.syncobj_timeline_wait(&[imported.1], &[7], 0, false, false, false)
        .context("wait qualification timeline")?;
    drop(producer);
    drop(exported);
    let binary = Syncobj(
        &peer,
        peer.create_syncobj(false)
            .context("create qualification binary syncobj")?,
    );
    peer.syncobj_timeline_transfer(imported.1, binary.1, 7, 0)
        .context("transfer qualification timeline point")?;
    let file = peer
        .syncobj_to_fd(binary.1, true)
        .context("export qualification sync-file")?;
    let roundtrip = Syncobj(
        device,
        device
            .create_syncobj(false)
            .context("create qualification syncobj")?,
    );
    use std::os::fd::AsRawFd;
    let mut input = [0_u8; 16];
    input[..4].copy_from_slice(&u32::from(roundtrip.1).to_le_bytes());
    input[4..8].copy_from_slice(&1_u32.to_le_bytes());
    input[8..12].copy_from_slice(&file.as_raw_fd().to_le_bytes());
    // SAFETY: this scalar-only DRM ioctl imports the retained sync-file into our allocated syncobj.
    let status = unsafe {
        libc::ioctl(
            device.as_raw_fd(),
            0xc01064c2_u32 as libc::Ioctl,
            input.as_mut_ptr(),
        )
    };
    ensure!(
        status == 0,
        "import sync-file into timeline: {}",
        std::io::Error::last_os_error()
    );
    device
        .syncobj_wait(&[roundtrip.1], 0, true, false)
        .context("wait qualification sync-file")?;
    Ok(())
}

#[cfg(test)]
#[path = "../tests/unit/syncobj.rs"]
mod tests;
