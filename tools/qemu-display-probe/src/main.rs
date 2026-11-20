use anyhow::{Context, Result, bail, ensure};
use async_channel::{Receiver, Sender};
use qemu_display::{
    Console, ConsoleListenerHandler, Cursor, MouseButton, MouseSet, Scanout, ScanoutDMABUF, Update,
    UpdateDMABUF,
};
use std::{
    ffi::CString,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::Path,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Default)]
#[repr(C)]
struct Input {
    kind: i32,
    a: i32,
    b: i32,
}

#[derive(Default)]
#[repr(C)]
struct PresentationStats {
    submitted: u64,
    presented: u64,
    discarded: u64,
    abandoned: u64,
    errors: u64,
    clock_id: u64,
}

unsafe extern "C" {
    fn viewer_open(title: *const libc::c_char, fullscreen: i32, gpu_priority: i32) -> i32;
    fn viewer_gpu_priority() -> i32;
    fn viewer_gpu_timing_open() -> i32;
    fn viewer_early_swap_open() -> i32;
    fn stage_profile_open(path: *const libc::c_char) -> i32;
    fn stage_profile_finish() -> i32;
    fn viewer_fence_wait(microseconds: u32) -> i32;
    fn viewer_cpu_ns() -> u64;
    fn viewer_import(
        fds: *const i32,
        offsets: *const u32,
        strides: *const u32,
        planes: u32,
        width: u32,
        height: u32,
        backing_width: u32,
        backing_height: u32,
        x: u32,
        y: u32,
        fourcc: u32,
        modifier: u64,
        y0_top: i32,
    ) -> i32;
    fn viewer_draw(frame_id: u64, fail_after_swap: i32) -> i32;
    fn viewer_presentation_open(path: *const libc::c_char, delay_ms: u32) -> i32;
    fn presentation_finish(out: *mut PresentationStats) -> i32;
    fn viewer_event(event: *mut Input) -> i32;
    fn frame_tags_open() -> i32;
    fn viewer_close();
}

#[derive(Debug)]
enum Render {
    Import(ScanoutDMABUF),
    Draw,
}
#[derive(Debug)]
struct Request {
    render: Render,
    received: Instant,
    reply: Sender<()>,
}

#[derive(Debug, Clone)]
struct Handler {
    sender: Sender<Request>,
    stopped: Arc<AtomicBool>,
}
impl Handler {
    async fn render(&self, render: Render) {
        let (reply, receive) = async_channel::bounded(1);
        let request = Request {
            render,
            received: Instant::now(),
            reply,
        };
        if self.sender.send(request).await.is_err() || receive.recv().await.is_err() {
            // This upstream handler API cannot return a D-Bus error. Disconnect
            // rather than allowing its wrapper to acknowledge a failed read.
            eprintln!("display renderer stopped; refusing successful update acknowledgement");
            std::process::exit(1);
        }
    }
}

#[async_trait::async_trait]
impl ConsoleListenerHandler for Handler {
    async fn scanout(&mut self, _: Scanout) {} // Boot placeholder only, never a game-frame fallback.
    async fn update(&mut self, _: Update) {}
    async fn scanout_dmabuf(&mut self, source: ScanoutDMABUF) {
        self.render(Render::Import(source)).await;
    }
    async fn update_dmabuf(&mut self, _: UpdateDMABUF) {
        self.render(Render::Draw).await;
    }
    async fn disable(&mut self) {}
    async fn mouse_set(&mut self, _: MouseSet) {}
    async fn cursor_define(&mut self, _: Cursor) {}
    fn disconnected(&mut self) {
        self.stopped.store(true, Ordering::Release);
    }
    fn interfaces(&self) -> Vec<String> {
        vec!["org.legion.Display1.Listener.ReadCompletion".into()]
    }
}

fn qnum(scancode: i32) -> Option<u32> {
    const LETTERS: [u32; 26] = [
        0x1e, 0x30, 0x2e, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18,
        0x19, 0x10, 0x13, 0x1f, 0x14, 0x16, 0x2f, 0x11, 0x2d, 0x15, 0x2c,
    ];
    Some(match scancode {
        4..=29 => LETTERS[(scancode - 4) as usize],
        30..=39 => (scancode - 28) as u32,
        40 => 0x1c,
        41 => 1,
        42 => 0xe,
        43 => 0xf,
        44 => 0x39,
        45 => 0xc,
        46 => 0xd,
        47 => 0x1a,
        48 => 0x1b,
        49 => 0x2b,
        51 => 0x27,
        52 => 0x28,
        53 => 0x29,
        54 => 0x33,
        55 => 0x34,
        56 => 0x35,
        57 => 0x3a,
        58..=67 => (scancode + 1) as u32,
        68 => 0x57,
        69 => 0x58,
        73 => 0xd2,
        74 => 0xc7,
        75 => 0xc9,
        76 => 0xd3,
        77 => 0xcf,
        78 => 0xd1,
        79 => 0xcd,
        80 => 0xcb,
        81 => 0xd0,
        82 => 0xc8,
        224 => 0x1d,
        225 => 0x2a,
        226 => 0x38,
        227 => 0xdb,
        228 => 0x9d,
        229 => 0x36,
        230 => 0xb8,
        231 => 0xdc,
        _ => return None,
    })
}

fn select_mouse(path: &Path, absolute: bool) -> Result<()> {
    // Only the private QMP socket supplied by the owned-run harness is used.
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    ensure!(
        serde_json::from_str::<serde_json::Value>(&line)?
            .get("QMP")
            .is_some(),
        "invalid owned QMP greeting"
    );
    fn call(
        stream: &mut UnixStream,
        reader: &mut BufReader<UnixStream>,
        command: serde_json::Value,
    ) -> Result<serde_json::Value> {
        serde_json::to_writer(&mut *stream, &command)?;
        stream.write_all(b"\n")?;
        loop {
            let mut line = String::new();
            ensure!(reader.read_line(&mut line)? > 0, "owned QMP disconnected");
            let reply: serde_json::Value = serde_json::from_str(&line)?;
            if reply.get("event").is_some() {
                continue;
            }
            ensure!(reply.get("error").is_none(), "owned QMP error: {reply}");
            return Ok(reply["return"].clone());
        }
    }
    call(
        &mut stream,
        &mut reader,
        serde_json::json!({"execute":"qmp_capabilities"}),
    )?;
    let mice = call(
        &mut stream,
        &mut reader,
        serde_json::json!({"execute":"query-mice"}),
    )?;
    let index = mice
        .as_array()
        .context("invalid owned mouse list")?
        .iter()
        .find(|mouse| {
            mouse["absolute"].as_bool() == Some(absolute)
                && mouse["name"]
                    .as_str()
                    .is_some_and(|name| name.contains("virtio") || name.contains("Virtio"))
        })
        .and_then(|mouse| mouse["index"].as_u64())
        .filter(|index| *index < 1024)
        .context("owned VM lacks the requested pointing device")?;
    call(
        &mut stream,
        &mut reader,
        serde_json::json!({"execute":"human-monitor-command","arguments":{"command-line":format!("mouse_set {index}")}}),
    )?;
    Ok(())
}

async fn input_loop(console: &Console, events: Receiver<Input>, qmp: &Path) -> Result<()> {
    let mut held = [false; 256];
    let mut forwarded = 0_u64;
    let mut relative_forwarded = 0_u64;
    let mut absolute = true;
    select_mouse(qmp, true)?;
    loop {
        let event = futures_lite::future::race(async { events.recv().await.ok() }, async {
            async_io::Timer::after(Duration::from_millis(500)).await;
            Some(Input::default())
        })
        .await;
        let Some(event) = event else {
            break;
        };
        if event.kind == 0 {
            // A property read can be served from zbus's cache after QEMU exits.
            // Probe the actual owned peer so shutdown cannot strand a window.
            if console
                .proxy
                .inner()
                .connection()
                .call_method(
                    Some("org.qemu"),
                    "/org/qemu/Display1",
                    Some("org.freedesktop.DBus.Peer"),
                    "Ping",
                    &(),
                )
                .await
                .is_err()
            {
                break;
            }
            continue;
        }
        match event.kind {
            2 => {
                if let Some(key) = qnum(event.a) {
                    held[key as usize] = event.b != 0;
                    if event.b != 0 {
                        console.keyboard.press(key).await?;
                    } else {
                        console.keyboard.release(key).await?;
                    }
                    forwarded += 1;
                }
            }
            3 if absolute => {
                let result = console
                    .mouse
                    .set_abs_position(event.a.max(0) as u32, event.b.max(0) as u32)
                    .await;
                if result.is_err() {
                    // Guest driver activation can reorder pointing devices
                    // after startup. Repair once, without a QMP hop per event.
                    select_mouse(qmp, true)?;
                    console
                        .mouse
                        .set_abs_position(event.a.max(0) as u32, event.b.max(0) as u32)
                        .await?;
                }
                forwarded += 1;
            }
            4 if !absolute => {
                if console.mouse.rel_motion(event.a, event.b).await.is_err() {
                    select_mouse(qmp, false)?;
                    console.mouse.rel_motion(event.a, event.b).await?;
                }
                forwarded += 1;
                relative_forwarded += 1;
            }
            5 => {
                let button = match event.a {
                    1 => MouseButton::Left,
                    2 => MouseButton::Middle,
                    3 => MouseButton::Right,
                    4 => MouseButton::Side,
                    5 => MouseButton::Extra,
                    _ => continue,
                };
                if event.b != 0 {
                    console.mouse.press(button).await?;
                } else {
                    console.mouse.release(button).await?;
                }
                forwarded += 1;
            }
            6 => {
                for _ in 0..event.a.unsigned_abs().min(32) {
                    let button = if event.a > 0 {
                        MouseButton::WheelUp
                    } else {
                        MouseButton::WheelDown
                    };
                    console.mouse.press(button).await?;
                    console.mouse.release(button).await?;
                    forwarded += 1;
                }
            }
            7 => {
                if !absolute {
                    select_mouse(qmp, true)?;
                    absolute = true;
                }
                for (key, down) in held.iter_mut().enumerate() {
                    if *down {
                        console.keyboard.release(key as u32).await?;
                        *down = false;
                    }
                }
            }
            8 => {
                absolute = event.a == 0;
                select_mouse(qmp, absolute)?;
            }
            _ => {}
        }
    }
    eprintln!("viewer input: forwarded={forwarded} relative_forwarded={relative_forwarded}");
    Ok(())
}

fn percentile(values: &mut [f64], fraction: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    Some(values[((values.len() - 1) as f64 * fraction) as usize])
}

fn run() -> Result<()> {
    let mut address = None;
    let mut title = "Legion D-Bus research".to_owned();
    let mut report = None;
    let mut qmp = None;
    let mut fullscreen = false;
    let mut gpu_priority = 0_i32;
    let mut gpu_timing = false;
    let mut early_swap = false;
    let mut inject = false;
    let mut fail_after = None;
    let mut presentation_report: Option<PathBuf> = None;
    let mut stage_profile: Option<PathBuf> = None;
    let mut fence_wait_us = 1000_u32;
    let mut presentation_delay_ms = 0_u32;
    let mut frame_tags = false;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--address" => address = arguments.next(),
            "--title" => title = arguments.next().context("missing title")?,
            "--report" => report = arguments.next().map(PathBuf::from),
            "--qmp" => qmp = arguments.next().map(PathBuf::from),
            "--fullscreen" => fullscreen = true,
            "--gpu-timing" => gpu_timing = true,
            "--fence-wait-us" => {
                fence_wait_us = arguments
                    .next()
                    .context("missing fence wait timeout")?
                    .parse()?;
            }
            "--stage-profile" => {
                stage_profile = Some(
                    arguments
                        .next()
                        .context("missing stage profile path")?
                        .into(),
                );
            }
            "--early-swap" => early_swap = true,
            "--gpu-priority" => {
                gpu_priority = match arguments.next().as_deref() {
                    Some("default") => 0,
                    Some("medium") => 0x3102,
                    Some("high") => 0x3101,
                    _ => bail!("GPU priority must be default, medium or high"),
                }
            }
            "--frame-tags" => frame_tags = true,
            "--inject-input" => inject = true,
            "--presentation-report" => {
                presentation_report = Some(
                    arguments
                        .next()
                        .context("missing presentation report")?
                        .into(),
                );
            }
            "--presentation-delay-ms" => {
                presentation_delay_ms = arguments
                    .next()
                    .context("missing presentation delay")?
                    .parse()?;
            }
            "--fail-read-after" => {
                fail_after = Some(
                    arguments
                        .next()
                        .context("missing failure frame")?
                        .parse::<u64>()?,
                )
            }
            _ => bail!("unknown argument {argument}"),
        }
    }
    let address = address.context("--address is required")?;
    let report = report.context("--report is required")?;
    let qmp = qmp.context("--qmp is required")?;
    let title = CString::new(title)?;
    ensure!(
        // SAFETY: this sets a bounded CPU wait parameter before the owner
        // thread renders. It does not acknowledge or access a source image.
        unsafe { viewer_fence_wait(fence_wait_us) } == 0,
        "fence wait timeout must be 50..1000 microseconds"
    );
    ensure!(
        stage_profile.is_none() || !early_swap,
        "stage profiling requires original swap ordering"
    );
    ensure!(
        presentation_delay_ms <= 100
            && (presentation_delay_ms == 0 || presentation_report.is_some()),
        "presentation delay requires its opt-in report and at most 100 ms"
    );
    ensure!(
        // SAFETY: C receives a valid terminated title. All viewer calls stay
        // on this thread, which owns the SDL window and current GL context.
        unsafe { viewer_open(title.as_ptr(), i32::from(fullscreen), gpu_priority) } == 0,
        "initialize DMA-BUF viewer"
    );
    if let Some(path) = &presentation_report {
        use std::os::unix::ffi::OsStrExt;
        let path = CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: terminated path is consumed synchronously; SDL and its native
        // display remain owned by this thread until all feedback is destroyed.
        if unsafe { viewer_presentation_open(path.as_ptr(), presentation_delay_ms) } != 0 {
            // SAFETY: initialization failed on the same context-owning thread.
            unsafe {
                viewer_close();
            }
            bail!("host presentation protocol/clock initialization failed");
        }
    }
    if let Some(path) = &stage_profile {
        use std::os::unix::ffi::OsStrExt;
        let path = CString::new(path.as_os_str().as_bytes())?;
        ensure!(
            // SAFETY: the terminated path is consumed synchronously on the owner
            // thread. C owns its bounded allocation and output file.
            unsafe { stage_profile_open(path.as_ptr()) } == 0,
            "open viewer stage profile"
        );
    }
    if early_swap {
        ensure!(
            presentation_report.is_some() && presentation_delay_ms == 0,
            "early swap requires presentation reporting without a delay control"
        );
        ensure!(
            // SAFETY: this owner thread holds the live SDL window and native display.
            unsafe { viewer_early_swap_open() } == 0,
            "early swap requires Wayland presentation"
        );
    }
    if gpu_timing {
        ensure!(
            // SAFETY: this owner thread retains a live current GL context.
            // Query resources remain owned until viewer_close after rendering.
            unsafe { viewer_gpu_timing_open() } == 0,
            "initialize GPU timer queries"
        );
    }
    if frame_tags {
        ensure!(
            presentation_report.is_some(),
            "frame tags require presentation feedback"
        );
        // SAFETY: the SDL owner thread has a live current GL context. All tag
        // resources stay owned until viewer_close on this same thread.
        if unsafe { frame_tags_open() } != 0 {
            // SAFETY: initialization failed on the GL context owner thread.
            unsafe {
                viewer_close();
            }
            bail!("initialize GPU frame tag extraction");
        }
    }
    let stopped = Arc::new(AtomicBool::new(false));
    let (render_tx, render_rx) = async_channel::bounded::<Request>(1);
    let (input_tx, input_rx) = async_channel::bounded::<Input>(256);
    let handler = Handler {
        sender: render_tx,
        stopped: stopped.clone(),
    };
    let network_stopped = stopped.clone();
    let network = std::thread::spawn(move || {
        let result = futures_lite::future::block_on(async {
            let deadline = Instant::now() + Duration::from_secs(15);
            let connection = loop {
                match qemu_display::zbus::connection::Builder::address(address.as_str())
                    .build()
                    .await
                {
                    Ok(connection) => break connection,
                    Err(error) if Instant::now() < deadline => {
                        eprintln!("waiting for owned display: {error}");
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    Err(error) => return Err(error.into()),
                }
            };
            let console = loop {
                if let Ok(console) = Console::new(&connection, 0).await
                    && console.label().await.is_ok()
                {
                    break console;
                }
                ensure!(
                    Instant::now() < deadline,
                    "owned QEMU console did not appear"
                );
                std::thread::sleep(Duration::from_millis(100));
            };
            console.register_listener(handler).await?;
            eprintln!("viewer: registered owned console");
            let result = input_loop(&console, input_rx, &qmp).await;
            console.unregister_listener();
            result
        });
        network_stopped.store(true, Ordering::Release);
        result
    });
    let mut imports = 0_u64;
    let mut frames = 0_u64;
    let mut waits = Vec::new();
    let mut draws = Vec::new();
    let mut failed = None;
    let started = Instant::now();
    // SAFETY: C reads this process's CPU accounting into its own local struct.
    let cpu_start = unsafe { viewer_cpu_ns() };
    let deadline = started + Duration::from_secs(240);
    let mut injected = false;
    let mut pending_input = None;
    let mut relative_phase = false;
    while !stopped.load(Ordering::Acquire) && Instant::now() < deadline {
        if let Ok(request) = render_rx.try_recv() {
            let wait = request.received.elapsed().as_secs_f64() * 1e6;
            let start = Instant::now();
            let (result, draw) = match request.render {
                Render::Import(source) => {
                    // SAFETY: fixed arrays remain alive across this synchronous
                    // import; EGL retains the backing memory independently.
                    let result = unsafe {
                        viewer_import(
                            source.fd.as_ptr(),
                            source.offset.as_ptr(),
                            source.stride.as_ptr(),
                            source.num_planes,
                            source.width,
                            source.height,
                            source.width,
                            source.height,
                            0,
                            0,
                            source.fourcc,
                            source.modifier,
                            i32::from(source.y0_top),
                        )
                    };
                    imports += 1;
                    (result, false)
                }
                // SAFETY: the GL context and all imported images are owned by
                // this thread. Success means its real read fence completed.
                Render::Draw => {
                    if !early_swap && fail_after.is_some_and(|limit| frames >= limit) {
                        eprintln!("LEGION_VIEWER_INJECTED_READ_FAILURE successful_reply=0");
                        // Terminate without returning through upstream's void
                        // handler wrapper, which would otherwise send success.
                        std::process::exit(1);
                    }
                    // SAFETY: this remains the context-owning thread; success
                    // is returned only after the real GPU read fence completes.
                    (
                        unsafe {
                            viewer_draw(
                                frames + 1,
                                i32::from(
                                    early_swap && fail_after.is_some_and(|limit| frames >= limit),
                                ),
                            )
                        },
                        true,
                    )
                }
            };
            if result != 0 {
                failed = Some("DMA-BUF import/draw/fence failed");
                break;
            }
            if draw {
                frames += 1;
                if draws.len() < 100_000 {
                    waits.push(wait);
                    draws.push(start.elapsed().as_secs_f64() * 1e6);
                }
            }
            // Only successful imports or completed GPU reads get a reply.
            request
                .reply
                .try_send(())
                .context("display reply receiver disappeared")?;
            if inject && !injected && frames > 3 && started.elapsed() > Duration::from_secs(6) {
                injected = true;
                input_tx.try_send(Input {
                    kind: 2,
                    a: 4,
                    b: 1,
                })?;
                input_tx.try_send(Input {
                    kind: 3,
                    a: 400,
                    b: 300,
                })?;
                input_tx.try_send(Input {
                    kind: 8,
                    a: 1,
                    b: 0,
                })?;
                pending_input = Some(Instant::now() + Duration::from_millis(50));
            }
        }
        if pending_input.is_some_and(|time| Instant::now() >= time) {
            input_tx.try_send(Input {
                kind: 4,
                a: 12,
                b: -7,
            })?;
            if !relative_phase {
                relative_phase = true;
                pending_input = Some(Instant::now() + Duration::from_millis(50));
                continue;
            }
            input_tx.try_send(Input {
                kind: 8,
                a: 0,
                b: 0,
            })?;
            input_tx.try_send(Input {
                kind: 2,
                a: 4,
                b: 0,
            })?;
            input_tx.try_send(Input {
                kind: 3,
                a: 500,
                b: 350,
            })?;
            pending_input = None;
        }
        let mut event = Input::default();
        // SAFETY: C initializes the writable event structure synchronously.
        while unsafe { viewer_event(&mut event) } != 0 {
            if event.kind == 1 {
                stopped.store(true, Ordering::Release);
                break;
            }
            input_tx
                .try_send(event)
                .context("input queue full; refusing silent input loss")?;
            event = Input::default();
        }
        std::thread::sleep(Duration::from_micros(100));
    }
    input_tx.close();
    render_rx.close();
    let network_result = network
        .join()
        .map_err(|_| anyhow::anyhow!("display connection thread panicked"))?;
    let elapsed_seconds = started.elapsed().as_secs_f64();
    // SAFETY: rendering stopped; C reads process-local CPU accounting only.
    let cpu_end = unsafe { viewer_cpu_ns() };
    let process_cpu_seconds = if cpu_start != u64::MAX && cpu_end != u64::MAX {
        cpu_end.checked_sub(cpu_start).map(|ns| ns as f64 / 1e9)
    } else {
        None
    };
    // SAFETY: rendering stopped on this owner thread; its current EGL context
    // and display remain live until viewer_close below.
    let assigned_priority = unsafe { viewer_gpu_priority() };
    if gpu_priority != 0 && assigned_priority != gpu_priority {
        failed = Some("viewer GPU priority changed or could not be queried");
    }
    let mut presentation = PresentationStats::default();
    if stage_profile.is_some() {
        // SAFETY: rendering has stopped on the owner thread. C serializes only
        // its initialized bounded records before viewer_close frees storage.
        if unsafe { stage_profile_finish() } != 0 {
            failed = Some("viewer stage profile could not be saved");
        }
    }
    if presentation_report.is_some() {
        // SAFETY: rendering has stopped, the SDL display is still live, and the
        // writable fixed-layout result outlives synchronous callback draining.
        if unsafe { presentation_finish(&mut presentation) } != 0 {
            failed = Some("host presentation feedback incomplete or invalid");
        }
    }
    // SAFETY: all rendering has stopped, and this is the context-owning thread.
    unsafe {
        viewer_close();
    }
    let passed = failed.is_none() && frames > 0 && network_result.is_ok();
    let statistics = serde_json::json!({"schema":1,"passed":passed,"imports":imports,"gpu_completed_frames":frames,
        "transport":"local-dmabuf","read_completion":"real-gl-fence-before-reply","cpu_frame_copies":0,"early_swap":early_swap,"gpu_timing_enabled":gpu_timing,"gpu_priority_requested":gpu_priority,"gpu_priority_assigned":assigned_priority,"tag_readback_bytes_per_frame":if frame_tags {8} else {0},
        "input_test_requested":inject,"input_test_injected":injected,"error":failed,"network_error":network_result.err().map(|e|e.to_string()),
        "stage_profile":stage_profile,
        "fence_wait_us":fence_wait_us,"process_cpu_seconds":process_cpu_seconds,"render_loop_elapsed_seconds":elapsed_seconds,
        "handler_to_render_us":{"p50":percentile(&mut waits,0.5),"p95":percentile(&mut waits,0.95)},
        "draw_fence_swap_us":{"p50":percentile(&mut draws,0.5),"p95":percentile(&mut draws,0.95)},
        "host_presentation":presentation_report.as_ref().map(|path|serde_json::json!({
            "report":path,"submitted":presentation.submitted,"presented":presentation.presented,
            "discarded":presentation.discarded,"abandoned":presentation.abandoned,"errors":presentation.errors,
            "clock_id":presentation.clock_id,"post_gpu_delay_ms":presentation_delay_ms,
            "scope":"viewer update tokens to host compositor feedback; source input/frame identity not linked"})),
        "scope":"CPU-observed viewer spans; optional compositor feedback is not input-to-photon"});
    std::fs::write(report, serde_json::to_vec_pretty(&statistics)?)?;
    ensure!(passed, "viewer did not complete successfully: {statistics}");
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("legion D-Bus viewer: {error:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn physical_game_keys_and_modifiers() {
        assert_eq!(qnum(4), Some(0x1e));
        assert_eq!(qnum(20), Some(0x10));
        assert_eq!(qnum(26), Some(0x11));
        assert_eq!(qnum(227), Some(0xdb));
        assert_eq!(qnum(228), Some(0x9d));
        assert_eq!(qnum(-1), None);
    }

    #[test]
    fn selects_owned_pointing_device_and_skips_qmp_events() {
        use std::os::unix::net::UnixListener;
        let path =
            std::env::temp_dir().join(format!("legion-qmp-test-{}.sock", std::process::id()));
        let listener = UnixListener::bind(&path).expect("test listener");
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("owned client");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("timeout");
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            stream.write_all(b"{\"QMP\":{}}\n").expect("greeting");
            for expected in ["qmp_capabilities", "query-mice", "human-monitor-command"] {
                let mut line = String::new();
                reader.read_line(&mut line).expect("request");
                let request: serde_json::Value = serde_json::from_str(&line).expect("JSON");
                assert_eq!(request["execute"], expected);
                stream
                    .write_all(b"{\"event\":\"RESUME\"}\n")
                    .expect("event");
                if expected == "query-mice" {
                    stream.write_all(b"{\"return\":[{\"name\":\"QEMU PS/2 Mouse\",\"absolute\":false,\"index\":0},{\"name\":\"QEMU Virtio Tablet\",\"absolute\":true,\"index\":1},{\"name\":\"QEMU Virtio Mouse\",\"absolute\":false,\"index\":3}]}\n").expect("devices");
                } else {
                    if expected == "human-monitor-command" {
                        assert_eq!(request["arguments"]["command-line"], "mouse_set 3");
                    }
                    stream.write_all(b"{\"return\":{}}\n").expect("reply");
                }
            }
        });
        let result = select_mouse(&path, false);
        worker.join().expect("test peer");
        std::fs::remove_file(&path).expect("remove owned socket");
        result.expect("relative pointer selection");
    }
}
