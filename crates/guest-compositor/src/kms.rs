use crate::{
    OwnedClient,
    forward_wait::ForwardWait,
    state::{ClientState, State},
};
use anyhow::{Context, Result, ensure};
use drm::{
    Device,
    buffer::{self, DrmFourcc, DrmModifier, PlanarBuffer},
    control::{self, Device as ControlDevice, connector, crtc, framebuffer},
};
use serde_json::json;
use smithay::{
    backend::allocator::{Buffer, Format, Fourcc, Modifier, dmabuf::Dmabuf},
    reexports::wayland_server::{
        Display, ListeningSocket, Resource, protocol::wl_buffer::WlBuffer,
    },
    wayland::dmabuf::get_dmabuf,
};
use std::{
    fs::{self, File, OpenOptions},
    os::{
        fd::{AsFd, BorrowedFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

struct Card(File);
impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl Device for Card {}
impl ControlDevice for Card {}

struct Image {
    dma: Dmabuf,
    fb: framebuffer::Handle,
    gem: buffer::Handle,
}
struct Description {
    gem: buffer::Handle,
    format: DrmFourcc,
    size: (u32, u32),
    stride: u32,
    offset: u32,
    modifier: Option<DrmModifier>,
}
impl PlanarBuffer for Description {
    fn size(&self) -> (u32, u32) {
        self.size
    }
    fn format(&self) -> DrmFourcc {
        self.format
    }
    // Legacy ADDFB2 accepts linear storage without the modifier flag.
    fn modifier(&self) -> Option<DrmModifier> {
        self.modifier
    }
    fn pitches(&self) -> [u32; 4] {
        [self.stride, 0, 0, 0]
    }
    fn offsets(&self) -> [u32; 4] {
        [self.offset, 0, 0, 0]
    }
    fn handles(&self) -> [Option<buffer::Handle>; 4] {
        [Some(self.gem), None, None, None]
    }
}

struct Scanout {
    card: Card,
    crtc: crtc::Handle,
    connector: connector::Handle,
    mode: control::Mode,
    images: Vec<Image>,
    handles: Vec<buffer::Handle>,
    current: Option<ScanoutBuffer>,
    queued: Option<ScanoutBuffer>,
    frames: u64,
    cursor: Option<crate::cursor::Cursor>,
    formats: Vec<Format>,
    native_nvidia: bool,
    plane: Option<crate::kms_plane::Plane>,
    source_size: Option<(u32, u32)>,
    source_sizes: Vec<((u32, u32), u64)>,
    source_transitions: u64,
    import_evictions: u64,
    validate_scaling: bool,
    scaling_checks: u64,
    unrecorded_source_sizes: u64,
}

struct ScanoutBuffer {
    fb: framebuffer::Handle,
    source: (u32, u32),
    buffer: WlBuffer,
    release: Option<crate::syncobj::Release>,
}

impl ScanoutBuffer {
    fn release(self) -> Result<()> {
        if let Some(release) = self.release {
            release.signal()?;
        }
        if self.buffer.is_alive() {
            self.buffer.release();
        }
        Ok(())
    }
}

impl Drop for Scanout {
    fn drop(&mut self) {
        if let Some(cursor) = self.cursor.take() {
            cursor.destroy(&self.card, self.crtc);
        }
        // Detach scanout before releasing client storage and destroying FBs.
        let _ = self.card.set_crtc(self.crtc, None, (0, 0), &[], None);
        for buffer in self.queued.take().into_iter().chain(self.current.take()) {
            // Shutdown can flush a KMS event without a host GPU acknowledgement.
            // Leave its explicit point unsignalled; never claim successful reuse.
            if buffer.buffer.is_alive() {
                buffer.buffer.release();
            }
        }
        for image in &self.images {
            let _ = self.card.destroy_framebuffer(image.fb);
        }
        for handle in &self.handles {
            let _ = self.card.close_buffer(*handle);
        }
    }
}

impl Scanout {
    fn open(requested: Option<(u16, u16)>, driver: &str) -> Result<Self> {
        for entry in fs::read_dir("/dev/dri")? {
            let path = entry?.path();
            if !path
                .file_name()
                .is_some_and(|name| name.as_encoded_bytes().starts_with(b"card"))
            {
                continue;
            }
            let card = Card(
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&path)?,
            );
            if card.get_driver()?.name().as_encoded_bytes() != driver.as_bytes() {
                continue;
            }
            card.acquire_master_lock()
                .context("acquire guest virtual display")?;
            let resources = card.resource_handles()?;
            for connector in resources.connectors() {
                let info = card.get_connector(*connector, true)?;
                if info.state() != connector::State::Connected {
                    continue;
                }
                let mode = *info
                    .modes()
                    .iter()
                    .find(|mode| requested.is_none_or(|size| mode.size() == size))
                    .context("virtual display does not offer the requested mode")?;
                let encoder = info
                    .encoders()
                    .first()
                    .context("virtual display has no encoder")?;
                let encoder = card.get_encoder(*encoder)?;
                let crtc = *resources
                    .filter_crtcs(encoder.possible_crtcs())
                    .first()
                    .context("virtual display has no CRTC")?;
                let mut primary_plane = None;
                let (cursor, formats) = if driver == "nvidia-drm" {
                    card.set_client_capability(drm::ClientCapability::Atomic, true)?;
                    card.set_client_capability(drm::ClientCapability::UniversalPlanes, true)?;
                    let mut formats = Vec::new();
                    for plane in card.plane_handles()? {
                        let info = card.get_plane(plane)?;
                        if !resources
                            .filter_crtcs(info.possible_crtcs())
                            .contains(&crtc)
                        {
                            continue;
                        }
                        let props = card.get_properties(plane)?;
                        let primary = props.iter().any(|(handle, value)| {
                            card.get_property(*handle)
                                .is_ok_and(|info| info.name().to_bytes() == b"type" && *value == 1)
                        });
                        if !primary {
                            continue;
                        }
                        ensure!(primary_plane.is_none(), "multiple virtual primary planes");
                        primary_plane = Some(crate::kms_plane::Plane::new(
                            &card, plane, crtc, *connector, mode,
                        )?);
                        for (handle, value) in props.iter() {
                            let info = card.get_property(*handle)?;
                            if info.name().to_bytes() == b"IN_FORMATS" {
                                let blob =
                                    card.get_property_blob(u64::from(u32::try_from(*value)?))?;
                                formats.extend(crate::kms_formats::decode(&blob)?);
                            }
                        }
                    }
                    ensure!(
                        !formats.is_empty(),
                        "NVIDIA virtual head advertises no RGB scanout formats"
                    );
                    // The experimental NVIDIA head does not yet support Legion's cursor ioctls.
                    (None, formats)
                } else {
                    (
                        Some(crate::cursor::Cursor::new(&card)?),
                        [
                            Fourcc::Xrgb8888,
                            Fourcc::Argb8888,
                            Fourcc::Xbgr8888,
                            Fourcc::Abgr8888,
                        ]
                        .map(|code| Format {
                            code,
                            modifier: Modifier::Linear,
                        })
                        .to_vec(),
                    )
                };
                return Ok(Self {
                    card,
                    crtc,
                    connector: *connector,
                    mode,
                    images: Vec::new(),
                    handles: Vec::new(),
                    current: None,
                    queued: None,
                    frames: 0,
                    cursor,
                    formats,
                    native_nvidia: driver == "nvidia-drm",
                    plane: primary_plane,
                    source_size: None,
                    source_sizes: Vec::new(),
                    source_transitions: 0,
                    import_evictions: 0,
                    validate_scaling: false,
                    scaling_checks: 0,
                    unrecorded_source_sizes: 0,
                });
            }
        }
        anyhow::bail!("no connected guest {driver} display")
    }

    fn import(&mut self, dma: &Dmabuf) -> Result<framebuffer::Handle> {
        if let Some(image) = self.images.iter().find(|image| image.dma == *dma) {
            return Ok(image.fb);
        }
        if self.images.len() >= 32 {
            // A live Wayland buffer may be re-imported later. Only the current
            // and queued KMS FBs must remain cached until their read completes.
            let index = self
                .images
                .iter()
                .position(|image| {
                    !self
                        .current
                        .as_ref()
                        .is_some_and(|buffer| buffer.fb == image.fb)
                        && !self
                            .queued
                            .as_ref()
                            .is_some_and(|buffer| buffer.fb == image.fb)
                })
                .context("all cached scanout images are still in use")?;
            let image = &self.images[index];
            self.card.destroy_framebuffer(image.fb)?;
            let image = self.images.remove(index);
            if !self.images.iter().any(|other| other.gem == image.gem) {
                self.card.close_buffer(image.gem)?;
                self.handles.retain(|handle| *handle != image.gem);
            }
            self.import_evictions += 1;
        }
        let (width, height) = self.mode.size();
        ensure!(
            (64..=8192).contains(&dma.size().w) && (64..=8192).contains(&dma.size().h),
            "scanout image dimensions exceed supported bounds"
        );
        ensure!(
            self.plane.is_some()
                || (dma.size().w == i32::from(width) && dma.size().h == i32::from(height)),
            "scanout image {:?} does not match the display mode {width}x{height}",
            dma.size()
        );
        ensure!(
            dma.num_planes() == 1
                && self.formats.contains(&dma.format())
                && (self.native_nvidia
                    || matches!(dma.format().code, Fourcc::Xrgb8888 | Fourcc::Xbgr8888))
                && !dma.y_inverted(),
            "scanout requires a non-inverted, single-plane RGB8888 layout advertised by the display"
        );
        let gem = self
            .card
            .prime_fd_to_buffer(dma.handles().next().context("missing DMA-BUF plane")?)?;
        // PRIME import may return an already-owned GEM handle for another FD.
        if !self.handles.contains(&gem) {
            self.handles.push(gem);
        }
        let description = Description {
            gem,
            format: dma.format().code,
            size: (u32::try_from(dma.size().w)?, u32::try_from(dma.size().h)?),
            stride: dma.strides().next().context("missing stride")?,
            offset: dma.offsets().next().context("missing offset")?,
            modifier: (dma.format().modifier != Modifier::Linear).then_some(dma.format().modifier),
        };
        let fb = self
            .card
            .add_planar_framebuffer(
                &description,
                if description.modifier.is_some() {
                    control::FbCmd2Flags::MODIFIERS
                } else {
                    control::FbCmd2Flags::empty()
                },
            )
            .context("create guest scanout framebuffer with its negotiated layout")?;
        self.images.push(Image {
            dma: dma.clone(),
            fb,
            gem,
        });
        Ok(fb)
    }

    fn submit(&mut self, buffer: WlBuffer, release: Option<crate::syncobj::Release>) -> Result<()> {
        ensure!(self.queued.is_none(), "scanout page flip is already queued");
        let dma = get_dmabuf(&buffer).context("non-DMA-BUF scanout image")?;
        let source = (u32::try_from(dma.size().w)?, u32::try_from(dma.size().h)?);
        let fb = self.import(dma)?;
        let mode_size = self.mode.size();
        let transition = self.source_size != Some(source);
        if let Some(plane) = &self.plane
            && transition
            && (self.current.is_some()
                || source != (u32::from(mode_size.0), u32::from(mode_size.1)))
        {
            self.scaling_checks += plane.commit(
                &self.card,
                fb,
                source,
                self.current.is_none(),
                self.validate_scaling && self.scaling_checks == 0,
            )?;
            self.queued = Some(ScanoutBuffer {
                fb,
                source,
                buffer,
                release,
            });
        } else if self.current.is_none() {
            self.card.set_crtc(
                self.crtc,
                Some(fb),
                (0, 0),
                &[self.connector],
                Some(self.mode),
            )?;
            self.current = Some(ScanoutBuffer {
                fb,
                source,
                buffer,
                release,
            });
            self.frames += 1;
        } else {
            self.card
                .page_flip(self.crtc, fb, control::PageFlipFlags::EVENT, None)?;
            self.queued = Some(ScanoutBuffer {
                fb,
                source,
                buffer,
                release,
            });
        }
        if transition && self.source_size.is_some() {
            self.source_transitions += 1;
        }
        self.source_size = Some(source);
        if let Some((_, count)) = self
            .source_sizes
            .iter_mut()
            .find(|(size, _)| *size == source)
        {
            *count += 1;
        } else {
            if self.source_sizes.len() < 32 {
                self.source_sizes.push((source, 1));
            } else {
                self.unrecorded_source_sizes += 1;
            }
        }
        Ok(())
    }

    fn complete(&mut self) -> Result<bool> {
        let events = match self.card.receive_events() {
            Ok(events) => events,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        let mut completed = false;
        for event in events {
            if matches!(event, control::Event::PageFlip(_)) {
                let next = self
                    .queued
                    .take()
                    .context("unexpected guest page-flip completion")?;
                if let Some(previous) = self.current.replace(next) {
                    previous.release()?;
                }
                self.frames += 1;
                completed = true;
            }
        }
        Ok(completed)
    }
}

pub(crate) fn run() -> Result<()> {
    ensure!(
        std::path::Path::new("/modules/load-order").is_file(),
        "KMS fixture requires prepared guest"
    );
    let settings: serde_json::Value =
        serde_json::from_slice(&fs::read("/frames/kms-presentation.json")?)?;
    let external = settings["client"] == "external";
    let application = external && settings["application"] == true;
    let event_wait = match settings
        .get("kms_wait")
        .map_or(Some("events"), serde_json::Value::as_str)
    {
        Some("sleep") => false,
        Some("events") => true,
        _ => anyhow::bail!("unsupported KMS wait mode"),
    };
    let mut wait = ForwardWait::new(&json!({"forward_wait":"events"}))?;
    let kms_refresh = match settings
        .get("kms_refresh")
        .map_or(Some("legacy"), serde_json::Value::as_str)
    {
        Some("legacy") => false,
        Some("kms") => true,
        _ => anyhow::bail!("unsupported KMS refresh policy"),
    };
    let seconds = settings["seconds"].as_u64().unwrap_or(15);
    ensure!(
        external || (5..=30).contains(&seconds),
        "invalid graphics check duration"
    );
    ensure!(
        settings["schema"] == 1 && (external || settings["client"] == "cube"),
        "unsupported KMS presentation fixture configuration"
    );
    // SDL may publish its tiled window size as the preferred EDID mode.
    // An explicitly sized fixture must still use its requested render mode.
    let requested = settings["width"]
        .as_u64()
        .zip(settings["height"].as_u64())
        .map(|(w, h)| Ok::<_, anyhow::Error>((u16::try_from(w)?, u16::try_from(h)?)))
        .transpose()?;
    let driver = settings["drm_driver"].as_str().unwrap_or("virtio_gpu");
    ensure!(
        matches!(driver, "virtio_gpu" | "nvidia-drm"),
        "unsupported virtual display driver"
    );
    let mut scanout = Scanout::open(requested, driver)?;
    scanout.validate_scaling = settings["validate_primary_scaling"] == true;
    let (width, height) = scanout.mode.size();
    let mut display = Display::<State>::new()?;
    let mut dh = display.handle();
    // Only the virtual KMS device's supported scanout layout is advertised.
    let formats = scanout.formats.clone();
    let mut state = State::forwarding(
        &dh,
        formats,
        scanout.card.0.metadata()?.rdev(),
        u32::from(width),
        u32::from(height),
    )?;
    let explicit_sync = settings["explicit_sync"].as_bool().unwrap_or(false);
    let acquire_event_wait = settings["kms_acquire_wake"] == true;
    ensure!(
        !acquire_event_wait || (explicit_sync && event_wait),
        "acquire event waits require explicit sync and KMS event mode"
    );
    let mut acquire_event_wait_calls = 0_u64;
    let mut acquire_event_wakeups = 0_u64;
    if explicit_sync {
        ensure!(
            driver == "nvidia-drm",
            "explicit KMS synchronization requires the research NVIDIA backend"
        );
        let strict =
            fs::read_to_string("/sys/module/nvkvm_guest/parameters/strict_scanout_release")
                .context("read negotiated host scanout completion capability")?;
        ensure!(
            strict.trim() == "Y",
            "host GPU scanout completion was not negotiated"
        );
        ensure!(
            state.enable_explicit_sync(&dh, scanout.card.0.try_clone()?)?,
            "guest display cannot support explicit timeline synchronization"
        );
    }
    let mut refresh_mhz = if kms_refresh {
        scanout
            .mode
            .vrefresh()
            .checked_mul(1000)
            .context("KMS refresh overflow")?
    } else {
        60_000
    };
    if kms_refresh {
        state.set_output_refresh(refresh_mhz)?;
    }
    state.gpu_only = true;
    state.diagnose_surfaces = settings["diagnostic_shm"] == true;
    let listener = ListeningSocket::bind(if external {
        "legion-proton-0"
    } else {
        "legion-kms-0"
    })?;
    let client_log = File::create("/frames/kms-client.log")?;
    let mut client = if external {
        None
    } else {
        Some(OwnedClient(
            Command::new("/usr/bin/vkcube")
                .args([
                    "--wsi",
                    "wayland",
                    "--width",
                    &width.to_string(),
                    "--height",
                    &height.to_string(),
                    "--c",
                    "1000000",
                    "--present_mode",
                    "1",
                ])
                .env("WAYLAND_DISPLAY", "legion-kms-0")
                .env(
                    "LEGION_CUBE_SECONDS",
                    settings["seconds"].as_u64().unwrap_or(15).to_string(),
                )
                .env("LEGION_CUBE_REPORT", "/frames/cube-result.json")
                .env("VK_LOADER_LAYERS_DISABLE", "~implicit~")
                .envs((settings["trace_protocol"] == true).then_some(("WAYLAND_DEBUG", "client")))
                .stdin(Stdio::null())
                .stdout(client_log.try_clone()?)
                .stderr(client_log)
                .spawn()?,
        ))
    };
    let start = Instant::now();
    let mut diagnostic_at = Instant::now();
    let mut input = crate::evdev::Input::open()?;
    if input.relative_supported() {
        state.enable_relative_input(&dh);
    }
    let mut input_at = Instant::now();
    let mut exit_check_at = Instant::now();
    let mut refresh_check_at = Instant::now();
    let mut clients = Vec::new();
    loop {
        ensure!(
            application
                || start.elapsed() < Duration::from_secs(if external { 60 } else { seconds + 10 }),
            "KMS presentation fixture timed out"
        );
        while let Some(stream) = listener.accept()? {
            clients.push(dh.insert_client(stream, Arc::new(ClientState::default()))?);
        }
        display.dispatch_clients(&mut state)?;
        if kms_refresh
            && scanout.native_nvidia
            && refresh_check_at.elapsed() >= Duration::from_secs(1)
        {
            refresh_check_at = Instant::now();
            // The virtual head accepts host refresh feedback after activation.
            // Its timer changes without a CRTC modeset. Re-read its advertised
            // modes so Wine sees the same rate as the virtual vblank.
            let connector = scanout.card.get_connector(scanout.connector, true)?;
            if let Some(mode) = connector
                .modes()
                .iter()
                .find(|mode| mode.size() == (width, height))
            {
                let next = mode
                    .vrefresh()
                    .checked_mul(1000)
                    .context("KMS refresh overflow")?;
                if next != refresh_mhz {
                    state.set_output_refresh(next)?;
                    refresh_mhz = next;
                }
            }
        }
        if input_at.elapsed() >= Duration::from_millis(1) {
            input_at = Instant::now();
            let now = u64::try_from(start.elapsed().as_micros())?;
            let (input_width, input_height) = scanout
                .current
                .as_ref()
                .map(|buffer| buffer.source)
                .unwrap_or((u32::from(width), u32::from(height)));
            state.ordered_input_batch(input.poll(input_width, input_height, now)?, now);
        }
        if explicit_sync {
            state.poll_explicit_sync(&dh)?;
        }
        if let Some(error) = state.error.take() {
            anyhow::bail!("KMS source Wayland: {error}");
        }
        // Imported images are bounded by the KMS cache. Drop dead Wayland
        // resource notifications; active FB ownership is tracked independently.
        state.destroyed_buffers.clear();
        for (dma, notifier) in std::mem::take(&mut state.pending_imports) {
            match scanout.import(&dma) {
                Ok(_) => {
                    let _ = notifier.successful::<State>();
                    state.imports += 1;
                }
                Err(error) => {
                    notifier.failed();
                    return Err(error);
                }
            }
        }
        if scanout.complete()? {
            state.send_callbacks(u32::try_from(start.elapsed().as_millis())?);
        }
        if scanout.queued.is_none()
            && let Some(buffer) = state.pending.take()
        {
            let release = state.acquire_pending_release();
            scanout.submit(buffer, release)?;
            if scanout.frames == 1 {
                fs::write(
                    "/frames/kms-first-frame.json",
                    b"{\"schema\":1,\"presented\":true}\n",
                )?;
            }
        }
        state.send_callbacks(u32::try_from(start.elapsed().as_millis())?);
        if scanout.current.is_some()
            && let Some(cursor) = &mut scanout.cursor
        {
            cursor.update(
                &scanout.card,
                scanout.crtc,
                &state,
                u32::try_from(start.elapsed().as_millis())?,
            )?;
        }
        display.flush_clients()?;
        if state.diagnose_surfaces && diagnostic_at.elapsed() >= Duration::from_secs(1) {
            diagnostic_at = Instant::now();
            fs::write(
                "/frames/kms-diagnostics.json.tmp",
                serde_json::to_vec(&json!({
                    "schema":1,"surfaces":state.surface_diagnostics(),
                    "completed_scanouts":scanout.frames,"imports":state.imports,
                    "source_size":scanout.source_size,"source_transitions":scanout.source_transitions,
                    "source_commits":state.source_commits,"shm_skipped":state.shm_skipped,
                    "input":state.input_diagnostics()
                    ,"keyboard_events":state.keyboard_events,"pointer_events":state.pointer_events,"evdev":input.diagnostics()
                }))?,
            )?;
            fs::rename(
                "/frames/kms-diagnostics.json.tmp",
                "/frames/kms-diagnostics.json",
            )?;
        }
        if let Some(status) = client
            .as_mut()
            .map(|child| child.0.try_wait())
            .transpose()?
            .flatten()
        {
            ensure!(
                status.success(),
                "KMS Vulkan client failed: {}",
                fs::read_to_string("/frames/kms-client.log")?
            );
            break;
        }
        if external && (!event_wait || exit_check_at.elapsed() >= Duration::from_millis(50)) {
            exit_check_at = Instant::now();
            // This marker lives on virtiofs. Avoid a cross-VM metadata request
            // on every loop iteration; owned-client shutdown can wait 50 ms.
            if std::path::Path::new("/frames/proton-client-exited").is_file() {
                break;
            }
        }
        if !external && start.elapsed() >= Duration::from_secs(seconds) {
            break;
        }
        if event_wait {
            // DRM flip completions and Wayland commits both wake this poll.
            // The bounded timeout also services evdev and the owned-client
            // exit marker, whose current interfaces have no shared wake FD.
            let descriptors = [display.as_fd(), listener.as_fd(), scanout.card.as_fd()];
            if acquire_event_wait && state.pending_sync_acquires() > 0 {
                acquire_event_wait_calls += 1;
                acquire_event_wakeups += u64::try_from(
                    wait.wait_with_acquires(descriptors, state.acquire_watch_fds())?,
                )?;
            } else {
                wait.wait(descriptors, false)?;
            }
        } else {
            std::thread::sleep(Duration::from_micros(200));
        }
    }
    ensure!(
        application || scanout.frames > 30,
        "KMS fixture displayed too few frames"
    );
    let report = json!({"schema":1,"passed":true,"transport":if driver == "nvidia-drm" { "nvkvm_kms_dmabuf" } else { "virtio_gpu_kms_linear_dmabuf" },
        "completed_scanouts":scanout.frames,"imports":state.imports,"width":width,"height":height,
        "primary_scaling":scanout.plane.is_some(),"source_transitions":scanout.source_transitions,
        "scaling_invalid_geometry_checks":scanout.scaling_checks,
        "import_cache_entries":scanout.images.len(),"import_evictions":scanout.import_evictions,
        "unrecorded_source_sizes":scanout.unrecorded_source_sizes,
        "source_sizes":scanout.source_sizes.iter().map(|(size, count)| json!({"width":size.0,"height":size.1,"submissions":count})).collect::<Vec<_>>(),
        "kms_wait":if event_wait {"events"} else {"sleep"},"wait":wait.report()?,
        "exit_marker_poll_ms":if event_wait {50} else {0},
        "refresh_mhz":refresh_mhz,
        "kms_refresh_policy":if kms_refresh {"kms"} else {"legacy"},
        "explicit_sync":explicit_sync,"synchronization":crate::syncobj::report(),
        "acquire_event_wait":acquire_event_wait,"acquire_event_wait_calls":acquire_event_wait_calls,"acquire_event_wakeups":acquire_event_wakeups,
        "elapsed_seconds":start.elapsed().as_secs_f64(),"steady_state_pixel_readbacks":0,
        "desktop_capture_verified":false});
    fs::write(
        "/frames/kms-result.json",
        serde_json::to_vec_pretty(&report)?,
    )?;
    if external {
        let mut forward = report.clone();
        forward["full_frame_cpu_transport"] = false.into();
        fs::write(
            "/frames/proton-forward-result.json",
            serde_json::to_vec_pretty(&forward)?,
        )?;
    }
    // Keep the final scanout owned while the desktop harness captures it.
    std::thread::sleep(Duration::from_secs(2));
    println!("{report}");
    Ok(())
}
