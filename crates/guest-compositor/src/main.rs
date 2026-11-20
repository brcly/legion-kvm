mod acquire;
mod acquire_gpu;
mod animation;
mod cursor;
mod evdev;
mod exit_marker;
mod forward;
mod forward_wait;
mod frame_pacing;
mod frame_timing;
mod frame_transport;
mod input_events;
mod kms;
mod kms_formats;
mod kms_plane;
mod native_acquire;
mod native_amd;
mod offscreen;
mod state;
#[cfg(test)]
#[path = "../tests/unit/surface.rs"]
mod surface_tests;
mod syncobj;
mod vm_forward;
mod xwayland;

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{SocketAddr, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use legion_display_channel::{Channel, FrameKind, FrameMeta};
use serde_json::json;
use smithay::{
    backend::{
        allocator::{Buffer, Fourcc},
        egl::{EGLContext, EGLDevice, EGLDisplay},
        renderer::{ExportMem, ImportDma, Renderer, gles::GlesRenderer},
    },
    reexports::wayland_server::{Display, ListeningSocket},
    utils::Rectangle,
    wayland::dmabuf::get_dmabuf,
};

struct OwnedClient(Child);
impl Drop for OwnedClient {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    if let Err(error) = run() {
        println!("{}", json!({"passed":false,"error":format!("{error:#}")}));
        std::process::exit(1);
    }
}

fn channel() -> Result<Channel> {
    for entry in fs::read_dir("/sys/bus/pci/devices")? {
        let path = entry?.path();
        if fs::read_to_string(path.join("vendor"))?.trim() != "0x1af4"
            || fs::read_to_string(path.join("device"))?.trim() != "0x1110"
        {
            continue;
        }
        fs::write(path.join("enable"), b"1")?;
        let resource = fs::read_to_string(path.join("resource"))?;
        let values = resource
            .lines()
            .nth(2)
            .context("no BAR2")?
            .split_whitespace()
            .map(|n| u64::from_str_radix(n.trim_start_matches("0x"), 16))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ensure!(
            values.len() == 3 && values[0] != 0,
            "unassigned display BAR"
        );
        let len = usize::try_from(
            values[1]
                .checked_sub(values[0])
                .and_then(|n| n.checked_add(1))
                .context("invalid BAR extent")?,
        )?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path.join("resource2"))?;
        let pixels = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path.join("resource2_wc"))?;
        // SAFETY: these descriptors expose this VM's same owned ivshmem BAR.
        return unsafe { Channel::attach_bar_wc(file, pixels, len) };
    }
    anyhow::bail!("no display ivshmem BAR")
}

fn renderer() -> Result<(GlesRenderer, PathBuf)> {
    let selected = std::env::var_os("LEGION_RENDER_NODE").map(PathBuf::from);
    let device = EGLDevice::enumerate()?
        .find(|device| {
            device.render_device_path().is_ok_and(|path| {
                path.starts_with("/dev/dri") && selected.as_ref().is_none_or(|node| *node == path)
            })
        })
        .context("no hardware EGL render device")?;
    let render_node = device.render_device_path()?;
    eprintln!("EGL device: {}", render_node.display());
    // SAFETY: the owned EGLDevice is retained by EGLDisplay and used only on this thread.
    let display = unsafe { EGLDisplay::new(device)? };
    let context = EGLContext::new(&display)?;
    // SAFETY: the newly created context is owned by this single-threaded compositor and has never been made current in another thread.
    Ok((unsafe { GlesRenderer::new(context)? }, render_node))
}

fn run() -> Result<()> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() == 2 && args[0] == "--native-amdgpu" {
        return native_amd::run(Path::new(&args[1]));
    }
    if (args.is_empty() || (args.len() == 1 && args[0] == "--proton-forward"))
        && Path::new("/frames/kms-presentation.json").is_file()
    {
        return kms::run();
    }
    if args.len() == 1 && args[0] == "--offscreen-server" {
        return offscreen::run(false);
    }
    if args.len() == 1 && args[0] == "--offscreen-protocol-server" {
        return offscreen::run(true);
    }
    if args.len() == 1 && args[0] == "--proton-forward" {
        let config = serde_json::from_slice(&fs::read("/frames/proton-forward-config.json")?)?;
        return vm_forward::run(&config);
    }
    if args.len() == 1 && args[0] == "--proton-native-forward" {
        let config = serde_json::from_slice(&fs::read("/frames/proton-forward-config.json")?)?;
        return forward::run_proton(&config);
    }
    if args.first().is_some_and(|arg| arg == "--native-forward") {
        return forward::run(&args[1..]);
    }
    if args.is_empty() && Path::new("/frames/presentation-config.json").is_file() {
        let config: serde_json::Value =
            serde_json::from_slice(&fs::read("/frames/presentation-config.json")?)?;
        if config["gpu_forward"] == true {
            return vm_forward::run(&config);
        }
    }
    let native = args.first().is_some_and(|a| a == "--native");
    let (channel, frames_dir, scratch, userspace) = if native {
        ensure!(
            args.len() == 5,
            "expected --native <channel> <frames> <userspace> <scratch>"
        );
        let file = OpenOptions::new().read(true).write(true).open(&args[1])?;
        // SAFETY: the explicit native-control mode receives the launcher's private size-sealed memfd.
        (
            unsafe { Channel::attach_host(file)? },
            PathBuf::from(&args[2]),
            PathBuf::from(&args[4]),
            PathBuf::from(&args[3]),
        )
    } else {
        ensure!(args.is_empty(), "unknown compositor arguments");
        ensure!(
            Path::new("/modules/load-order").is_file() && Path::new("/probe-display").is_file(),
            "requires the prepared Legion guest initramfs"
        );
        (
            channel()?,
            PathBuf::from("/frames"),
            PathBuf::from("/tmp"),
            PathBuf::from("/usr"),
        )
    };
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(frames_dir.join("presentation-config.json"))?)?;
    ensure!(config["schema"] == 1, "unknown presentation configuration");
    let uncapped = config["uncapped"]
        .as_bool()
        .context("missing uncapped option")?;
    let (width, height) = channel.dimensions();
    let mut producer = channel.producer()?;
    let mut display = Display::<state::State>::new()?;
    let mut dh = display.handle();
    let (renderer, render_path) = renderer().context("initialize hardware compositor renderer")?;
    let render_node = fs::metadata(&render_path)?.rdev();
    let mut state = state::State::new(&dh, renderer, render_node, width, height)?;
    let listener = ListeningSocket::bind("legion-app-0")?;
    let client_log_path = scratch.join("vkcube.log");
    let client_log = File::create(&client_log_path)?;
    let mut client_command = if native {
        let mut command = Command::new(userspace.join("lib/x86_64-linux-gnu/ld-linux-x86-64.so.2"));
        command
            .arg("--library-path")
            .arg(std::env::join_paths([
                userspace.join("lib"),
                userspace.join("lib/x86_64-linux-gnu"),
            ])?)
            .arg(userspace.join("bin/vkcube"));
        command
    } else {
        Command::new("/usr/bin/vkcube")
    };
    let metrics_dir = scratch.join("legion-benchmark");
    if uncapped {
        fs::create_dir(&metrics_dir)?;
        client_command.env("MANGOHUD", "1")
            .env("MANGOHUD_CONFIG", format!("no_display,control=legion-benchmark-%p,log_duration=10,log_interval=0,output_folder={},fps_limit=0,cpu_stats=0,gpu_stats=0",metrics_dir.display()));
    }
    let mut client = OwnedClient(
        client_command
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
                if uncapped { "1" } else { "2" },
            ])
            .env("WAYLAND_DISPLAY", "legion-app-0")
            .stdin(Stdio::null())
            .stdout(client_log.try_clone()?)
            .stderr(client_log)
            .spawn()
            .context("start unmodified vkcube")?,
    );
    let start = Instant::now();
    let mut frames = 0_u64;
    let mut dropped = 0_u64;
    let mut stages = [0_u128; 3];
    let mut clients = Vec::new();
    let interval = Duration::from_nanos(1_000_000_000 / 60);
    let mut next = Instant::now();
    let mut logger_control = None;
    while !channel.stopped() && start.elapsed() < Duration::from_secs(60) {
        while let Some(stream) = listener.accept()? {
            clients.push(dh.insert_client(stream, Arc::new(state::ClientState::default()))?);
        }
        display.dispatch_clients(&mut state)?;
        if let Some(error) = state.error.take() {
            anyhow::bail!("Wayland import: {error}");
        }
        state.input(channel.input(), u32::try_from(start.elapsed().as_millis())?);
        if uncapped && logger_control.is_none() && start.elapsed() > Duration::from_secs(2) {
            let address =
                SocketAddr::from_abstract_name(format!("legion-benchmark-{}", client.0.id()))?;
            if let Ok(mut socket) = UnixStream::connect_addr(&address) {
                socket.write_all(b":logging=1;")?;
                logger_control = Some(socket);
            }
            ensure!(
                logger_control.is_some() || start.elapsed() < Duration::from_secs(8),
                "benchmark logger did not become ready"
            );
        }
        if uncapped || Instant::now() >= next {
            if let Some(buffer) = state.pending.take() {
                let begin = Instant::now();
                let dma =
                    get_dmabuf(&buffer).context("application supplied a non-DMA-BUF frame")?;
                ensure!(
                    dma.size().w == i32::try_from(width)? && dma.size().h == i32::try_from(height)?,
                    "unexpected application buffer size: {:?}",
                    dma.size()
                );
                let renderer = state
                    .renderer
                    .as_mut()
                    .context("missing readback renderer")?;
                let texture = renderer
                    .import_dmabuf(dma, None)
                    .context("import application DMA-BUF")?;
                let imported = Instant::now();
                let mapping = renderer
                    .copy_texture(&texture, Rectangle::from_size(dma.size()), Fourcc::Abgr8888)
                    .context("read back application texture")?;
                let copied = Instant::now();
                let bytes = renderer
                    .map_texture(&mapping)
                    .context("map application readback")?;
                let mapped = Instant::now();
                stages[0] += imported.duration_since(begin).as_nanos();
                stages[1] += copied.duration_since(imported).as_nanos();
                stages[2] += mapped.duration_since(copied).as_nanos();
                frames += 1;
                if !producer.publish(
                    bytes,
                    FrameMeta {
                        kind: FrameKind::Application,
                        number: frames,
                        input: state.last_input,
                        rgba: [0; 4],
                        gpu_ns: u64::try_from(begin.elapsed().as_nanos())?,
                        copy_ns: 0,
                    },
                )? {
                    dropped += 1;
                }
                if frames == 1 {
                    fs::write(frames_dir.join("application-first.rgba"), bytes)?;
                }
                buffer.release();
                drop(mapping);
                drop(texture);
                // No render() call runs in this import/readback-only compositor.
                renderer.cleanup_texture_cache()?;
            }
            state.send_callbacks(u32::try_from(start.elapsed().as_millis())?);
            if !uncapped {
                next += interval;
                if Instant::now().saturating_duration_since(next) > interval {
                    next = Instant::now();
                }
            }
        }
        display.flush_clients()?;
        if let Some(status) = client.0.try_wait()? {
            anyhow::bail!(
                "vkcube exited early: {status}; {}",
                fs::read_to_string(&client_log_path)?
            );
        }
        if uncapped {
            std::thread::yield_now();
        } else {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    ensure!(frames > 0, "application presented no frames");
    state.close_clients();
    display.flush_clients()?;
    let shutdown = Instant::now();
    while client.0.try_wait()?.is_none() && shutdown.elapsed() < Duration::from_secs(1) {
        display.dispatch_clients(&mut state)?;
        display.flush_clients()?;
        std::thread::sleep(Duration::from_millis(5));
    }
    if uncapped {
        ensure!(logger_control.is_some(), "benchmark logger was not started");
        for entry in fs::read_dir(&metrics_dir)? {
            let entry = entry?;
            ensure!(
                entry.metadata()?.len() <= 64 * 1024 * 1024,
                "benchmark log exceeds 64 MiB"
            );
            fs::copy(entry.path(), frames_dir.join(entry.file_name()))?;
        }
    }
    println!(
        "{}",
        json!({"passed":true,"native_control":native,"render_node":render_path,"application":userspace.join("bin/vkcube"),"unmodified_application":true,"uncapped":uncapped,"present_mode":if uncapped {"mailbox"} else {"fifo"},"frames_rendered":frames,"frames_dropped":dropped,"readback_stages_mean_ms":stages.map(|n|n as f64/frames as f64/1_000_000.0),"dmabuf_imports":state.imports,"keyboard_events":state.keyboard_events,"pointer_events":state.pointer_events,"last_input_generation":state.last_input.generation(),"stopped_by_host":channel.stopped(),"zero_copy":false,"client_log":fs::read_to_string(&client_log_path)?})
    );
    Ok(())
}
