use super::*;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use legion_proton_workload::{cpu_work, frame_rate, frame_stage_ticks, verify_presentation_load};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::*,
    },
    core::w,
};

static CLOSING: AtomicBool = AtomicBool::new(false);
static PALETTE: AtomicU64 = AtomicU64::new(0);
static KEY_EVENTS: AtomicU64 = AtomicU64::new(0);
static POINTER_EVENTS: AtomicU64 = AtomicU64::new(0);
static INPUT_QPC: AtomicU64 = AtomicU64::new(0);
static LATENCY_MODE: AtomicBool = AtomicBool::new(false);
static LATENCY_DELAY_MS: AtomicU64 = AtomicU64::new(0);
const STYLE: WINDOW_STYLE = WS_POPUP;

/// # Safety Called by Win32 with a live window and message arguments valid for DefWindowProcW.
unsafe extern "system" fn procedure(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match message {
        WM_CLOSE => {
            CLOSING.store(true, Ordering::Relaxed);
            return LRESULT(0); // Defer destruction until all original GPU fences complete.
        }
        WM_KEYDOWN if wp.0 == 0x20 && lp.0 & (1 << 30) == 0 => {
            if LATENCY_MODE.load(Ordering::Relaxed) {
                if let Ok(time) = counter() {
                    INPUT_QPC.store(time, Ordering::Relaxed);
                }
                // Diagnostic-only positive control: a known source delay must
                // appear in the independent host capture measurement.
                let delay = LATENCY_DELAY_MS.load(Ordering::Relaxed) as u32;
                if delay != 0 {
                    // SAFETY: bounded delay on this owned diagnostic GUI thread.
                    unsafe {
                        Sleep(delay);
                    }
                }
                PALETTE.fetch_add(1, Ordering::Relaxed);
            } else {
                PALETTE.fetch_xor(128, Ordering::Relaxed);
            }
            KEY_EVENTS.fetch_add(1, Ordering::Relaxed);
        }
        WM_KEYDOWN if wp.0 == 0x1b => {
            CLOSING.store(true, Ordering::Relaxed);
        }
        WM_MOUSEMOVE => {
            POINTER_EVENTS.fetch_add(1, Ordering::Relaxed);
        }
        _ => {}
    }
    // SAFETY: this callback forwards its original valid window/message arguments.
    unsafe { DefWindowProcW(hwnd, message, wp, lp) }
}

struct Window(HWND);
impl Window {
    fn new(width: u32, height: u32) -> Result<Self> {
        // SAFETY: null requests this executable's live module handle.
        let instance = unsafe { GetModuleHandleW(None) }.map_err(|e| e.to_string())?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(procedure),
            hInstance: instance.into(),
            lpszClassName: w!("LegionOwnedD3D12"),
            ..Default::default()
        };
        // SAFETY: static class name/callback and initialized descriptor remain valid for the entire single-window fixture's process lifetime.
        if unsafe { RegisterClassW(&class) } == 0 {
            return Err("register owned window class".into());
        }
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        // SAFETY: owned rectangle computes the outer size for the requested client-area resolution using this fixture's exact window style.
        unsafe { AdjustWindowRectEx(&mut rect, STYLE, false, WINDOW_EX_STYLE::default()) }
            .map_err(|e| e.to_string())?;
        // SAFETY: static strings, live module, no parent/menu/user pointer.
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class.lpszClassName,
                w!("Legion D3D12 — Space changes palette"),
                STYLE | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                rect.right - rect.left,
                rect.bottom - rect.top,
                None,
                None,
                Some(instance.into()),
                None,
            )
        }
        .map_err(|e| e.to_string())?;
        Ok(Self(hwnd))
    }
    fn size(&self) -> Result<(u32, u32)> {
        let mut rect = RECT::default();
        // SAFETY: live owned window and writable initialized output.
        unsafe { GetClientRect(self.0, &mut rect) }.map_err(|e| e.to_string())?;
        Ok((
            (rect.right - rect.left).max(0) as u32,
            (rect.bottom - rect.top).max(0) as u32,
        ))
    }
    fn resize(&self, width: u32, height: u32) -> Result<()> {
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        // SAFETY: owned initialized rectangle and this fixture's fixed window style.
        unsafe { AdjustWindowRectEx(&mut rect, STYLE, false, WINDOW_EX_STYLE::default()) }
            .map_err(|e| e.to_string())?;
        // SAFETY: only the owned window is resized; no global positioning or focus change.
        unsafe {
            SetWindowPos(
                self.0,
                None,
                0,
                0,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
        }
        .map_err(|e| e.to_string())
    }
    fn messages(&self) {
        let mut message = MSG::default();
        // SAFETY: messages belong to this fixture's GUI thread.
        unsafe {
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
    fn settle(&self, width: u32, height: u32) -> Result<()> {
        // Desktop compositors can configure an initial placement before Wine applies the application's requested size.
        let mut stable = 0;
        for attempt in 0..2000 {
            self.messages();
            if self.size()? == (width, height) {
                stable += 1;
                if stable == 100 {
                    return Ok(());
                }
            } else {
                stable = 0;
                if attempt % 100 == 0 {
                    self.resize(width, height)?;
                }
            }
            if CLOSING.load(Ordering::Relaxed) {
                return Err("owned window closed during setup".into());
            }
            // SAFETY: yield only the owned GUI thread while processing its configure events.
            unsafe {
                Sleep(1);
            }
        }
        Err(format!(
            "desktop did not accept requested client size {width}x{height}; observed {:?}",
            self.size()?
        ))
    }
}
impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: the unique owned window outlives renderer/fence cleanup.
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

struct Frame {
    target: ID3D12Resource,
    allocator: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
    rtv: D3D12_CPU_DESCRIPTOR_HANDLE,
    fence: u64,
}
struct DrawObservation {
    image_hash: Option<u64>,
    clocks: [u64; 7],
    slot_blocked: bool,
    present_count: u32,
    fence: u64,
}
struct Readback {
    resource: ID3D12Resource,
    footprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT,
    len: usize,
}
impl Readback {
    fn new(gpu: &Gpu, target: &ID3D12Resource) -> Result<Self> {
        let mut footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
        let mut total = 0;
        // SAFETY: live texture descriptor and initialized synchronous footprint outputs.
        unsafe {
            gpu.device.GetCopyableFootprints(
                &target.GetDesc(),
                0,
                1,
                0,
                Some(&mut footprint),
                None,
                None,
                Some(&mut total),
            );
        }
        let resource = resource(
            &gpu.device,
            D3D12_HEAP_TYPE_READBACK,
            &D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Width: total,
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                ..Default::default()
            },
            D3D12_RESOURCE_STATE_COPY_DEST,
        )?;
        Ok(Self {
            resource,
            footprint,
            len: usize::try_from(total).map_err(|_| "readback size")?,
        })
    }
    fn record(&self, list: &ID3D12GraphicsCommandList, target: &ID3D12Resource) {
        let mut dst = D3D12_TEXTURE_COPY_LOCATION {
            pResource: ManuallyDrop::new(Some(self.resource.clone())),
            Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                PlacedFootprint: self.footprint,
            },
        };
        let mut src = D3D12_TEXTURE_COPY_LOCATION {
            pResource: ManuallyDrop::new(Some(target.clone())),
            Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                SubresourceIndex: 0,
            },
        };
        // SAFETY: caller transitions source to COPY_SOURCE; destination is a sufficiently sized READBACK buffer in COPY_DEST.
        unsafe {
            list.CopyTextureRegion(&dst, 0, 0, 0, &src, None);
            ManuallyDrop::drop(&mut dst.pResource);
            ManuallyDrop::drop(&mut src.pResource);
        }
    }
    fn verify(&self, width: u32, height: u32, seed: u32, iterations: u32) -> Result<u64> {
        let mut pointer = ptr::null_mut();
        // SAFETY: caller has waited for the original queue fence covering this copy.
        unsafe {
            com(
                self.resource.Map(
                    0,
                    Some(&D3D12_RANGE {
                        Begin: 0,
                        End: self.len,
                    }),
                    Some(&mut pointer),
                ),
                "map presented image",
            )?;
            if pointer.is_null() {
                return Err("null presented image mapping".into());
            }
            let result = verify_presentation_load(
                core::slice::from_raw_parts(pointer.cast::<u8>(), self.len),
                self.footprint.Footprint.RowPitch as usize,
                width,
                height,
                seed,
                iterations,
            );
            self.resource
                .Unmap(0, Some(&D3D12_RANGE { Begin: 0, End: 0 }));
            result.ok_or_else(|| "presented image CPU oracle mismatch".into())
        }
    }
}

struct Renderer<'a> {
    gpu: &'a mut Gpu,
    chain: IDXGISwapChain3,
    frames: Vec<Frame>,
    heap: ID3D12DescriptorHeap,
    root: ID3D12RootSignature,
    pipelines: Vec<ID3D12PipelineState>,
    width: u32,
    height: u32,
    fragment_iterations: u32,
}
impl<'a> Renderer<'a> {
    fn new(gpu: &'a mut Gpu, window: &Window, fragment_iterations: u32) -> Result<Self> {
        let (width, height) = window.size()?;
        let (root, one) = pipeline::create_present_pipeline(&gpu.device, false)?;
        let (_, two) = pipeline::create_present_pipeline(&gpu.device, true)?;
        // SAFETY: swapchain uses the owned HWND and this device's DIRECT queue.
        let (chain, heap) = unsafe {
            let factory: IDXGIFactory2 = com(
                CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS::default()),
                "presentation factory",
            )?;
            let chain: IDXGISwapChain3 = com(
                factory.CreateSwapChainForHwnd(
                    &gpu.queue,
                    window.0,
                    &DXGI_SWAP_CHAIN_DESC1 {
                        Width: width,
                        Height: height,
                        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                        BufferCount: 3,
                        Scaling: DXGI_SCALING_STRETCH,
                        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                        AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                        ..Default::default()
                    },
                    None,
                    None,
                ),
                "create presentation swapchain",
            )?
            .cast()
            .map_err(|e| e.to_string())?;
            let _ = factory.MakeWindowAssociation(window.0, DXGI_MWA_NO_ALT_ENTER);
            let heap = com(
                gpu.device
                    .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                        Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
                        NumDescriptors: 3,
                        ..Default::default()
                    }),
                "presentation RTV heap",
            )?;
            (chain, heap)
        };
        let mut result = Self {
            gpu,
            chain,
            frames: Vec::with_capacity(3),
            heap,
            root,
            pipelines: vec![one, two],
            width,
            height,
            fragment_iterations,
        };
        result.buffers()?;
        Ok(result)
    }
    fn buffers(&mut self) -> Result<()> {
        // SAFETY: all three buffers come from this device/chain; descriptor offsets stay inside its three-entry RTV heap.
        unsafe {
            let base = self.heap.GetCPUDescriptorHandleForHeapStart();
            let step = self
                .gpu
                .device
                .GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV)
                as usize;
            for index in 0..3 {
                let target: ID3D12Resource =
                    com(self.chain.GetBuffer(index), "swapchain backbuffer")?;
                let allocator = com(
                    self.gpu
                        .device
                        .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT),
                    "presentation allocator",
                )?;
                let list: ID3D12GraphicsCommandList = com(
                    self.gpu.device.CreateCommandList(
                        0,
                        D3D12_COMMAND_LIST_TYPE_DIRECT,
                        &allocator,
                        None,
                    ),
                    "presentation list",
                )?;
                com(list.Close(), "close presentation list")?;
                let rtv = D3D12_CPU_DESCRIPTOR_HANDLE {
                    ptr: base.ptr + index as usize * step,
                };
                self.gpu.device.CreateRenderTargetView(&target, None, rtv);
                self.frames.push(Frame {
                    target,
                    allocator,
                    list,
                    rtv,
                    fence: 0,
                });
            }
        }
        Ok(())
    }
    fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        self.gpu.wait()?;
        self.frames.clear(); // Releases direct/command-list references before ResizeBuffers.
        com(
            // SAFETY: every original fence completed; no old backbuffer/list references remain.
            unsafe {
                self.chain.ResizeBuffers(
                    3,
                    width,
                    height,
                    DXGI_FORMAT_R8G8B8A8_UNORM,
                    DXGI_SWAP_CHAIN_FLAG::default(),
                )
            },
            "resize swapchain",
        )?;
        self.width = width;
        self.height = height;
        self.buffers()
    }
    fn draw(
        &mut self,
        seed: u32,
        diagnostic: bool,
        probe: bool,
        feedback: bool,
    ) -> Result<DrawObservation> {
        let stamp = || if probe { counter() } else { Ok(0) };
        let mut clocks = [0; 7];
        clocks[0] = stamp()?;
        // SAFETY: live owned chain; returned index selects one of its three buffers.
        let index = unsafe { self.chain.GetCurrentBackBufferIndex() } as usize;
        let frame = self.frames.get(index).ok_or("invalid current backbuffer")?;
        let waited = self.gpu.wait_for(frame.fence)?;
        clocks[1] = stamp()?;
        let readback = diagnostic
            .then(|| Readback::new(self.gpu, &frame.target))
            .transpose()?;
        let next_serial = self
            .gpu
            .serial
            .checked_add(1)
            .ok_or("presentation fence overflow")?;
        // SAFETY: original fence above protects allocator/list reuse.
        unsafe {
            com(frame.allocator.Reset(), "reset presentation allocator")?;
            com(
                frame.list.Reset(&frame.allocator, None),
                "reset presentation list",
            )?;
        }
        clocks[2] = stamp()?;
        // SAFETY: the same completed original fence protects this command list; all borrowed descriptors and resources remain owned through its fence.
        let command: ID3D12CommandList = unsafe {
            transition(
                &frame.list,
                &frame.target,
                D3D12_RESOURCE_STATE_PRESENT,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
            );
            frame.list.SetGraphicsRootSignature(&self.root);
            frame.list.SetGraphicsRoot32BitConstants(
                0,
                3,
                [seed, self.width, self.fragment_iterations].as_ptr().cast(),
                0,
            );
            frame
                .list
                .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            frame.list.RSSetViewports(&[D3D12_VIEWPORT {
                Width: self.width as f32,
                Height: self.height as f32,
                MaxDepth: 1.0,
                ..Default::default()
            }]);
            frame
                .list
                .OMSetRenderTargets(1, Some(&frame.rtv), false, None);
            for (pso, left, right) in [
                (&self.pipelines[0], 0, self.width / 2),
                (&self.pipelines[1], self.width / 2, self.width),
            ] {
                frame.list.SetPipelineState(pso);
                frame.list.RSSetScissorRects(&[RECT {
                    left: left as i32,
                    top: 0,
                    right: right as i32,
                    bottom: self.height as i32,
                }]);
                frame.list.DrawInstanced(3, 1, 0, 0);
            }
            if let Some(copy) = &readback {
                transition(
                    &frame.list,
                    &frame.target,
                    D3D12_RESOURCE_STATE_RENDER_TARGET,
                    D3D12_RESOURCE_STATE_COPY_SOURCE,
                );
                copy.record(&frame.list, &frame.target);
                transition(
                    &frame.list,
                    &frame.target,
                    D3D12_RESOURCE_STATE_COPY_SOURCE,
                    D3D12_RESOURCE_STATE_PRESENT,
                );
            } else {
                transition(
                    &frame.list,
                    &frame.target,
                    D3D12_RESOURCE_STATE_RENDER_TARGET,
                    D3D12_RESOURCE_STATE_PRESENT,
                );
            }
            com(frame.list.Close(), "close presentation commands")?;
            frame.list.cast().map_err(|e| e.to_string())?
        };
        clocks[3] = stamp()?;
        // SAFETY: closed list and all referenced allocations remain owned by the renderer.
        unsafe {
            self.gpu.queue.ExecuteCommandLists(&[Some(command)]);
        }
        self.gpu.serial = next_serial;
        self.frames[index].fence = self.gpu.serial; // Retain in-flight ownership before fallible calls.
        let executed_at = stamp();
        // SAFETY: current buffer was rendered and returned to PRESENT.
        let presented = unsafe { self.chain.Present(0, DXGI_PRESENT::default()) };
        let presented_at = stamp();
        // SAFETY: live queue/fence; covers original work even if Present failed.
        let signalled = unsafe { self.gpu.queue.Signal(&self.gpu.fence, self.gpu.serial) };
        let signalled_at = stamp();
        if let Err(error) = signalled {
            if let Some(copy) = readback {
                core::mem::forget(copy);
            }
            return Err(format!("presentation original fence signal: {error}"));
        }
        if presented.is_err() {
            if self.gpu.wait().is_err()
                && let Some(copy) = readback
            {
                core::mem::forget(copy);
            }
            return Err(format!("Present: HRESULT {:#010x}", presented.0));
        }
        let image_hash = if let Some(copy) = readback {
            if let Err(error) = self.gpu.wait() {
                core::mem::forget(copy);
                return Err(error);
            }
            Some(copy.verify(self.width, self.height, seed, self.fragment_iterations)?)
        } else {
            None
        };
        clocks[4] = executed_at?;
        clocks[5] = presented_at?;
        clocks[6] = signalled_at?;
        let present_count = if feedback {
            // SAFETY: live owned swapchain; this call returns its submitted frame ID.
            unsafe { self.chain.GetLastPresentCount() }.map_err(|e| e.to_string())?
        } else {
            0
        };
        Ok(DrawObservation {
            present_count,
            fence: self.gpu.serial,
            image_hash,
            clocks,
            slot_blocked: waited[1] != 0 || waited[2] != 0,
        })
    }
}
impl Drop for Renderer<'_> {
    fn drop(&mut self) {
        if self.gpu.wait().is_err() {
            // Preserve potentially referenced resources until this failing owned process exits, rather than freeing in-flight allocations.
            core::mem::forget(core::mem::take(&mut self.frames));
            core::mem::forget(core::mem::take(&mut self.pipelines));
            core::mem::forget(self.root.clone());
            core::mem::forget(self.heap.clone());
        }
    }
}

struct Feedback {
    output: Handle,
    frequency: u64,
    ready: bool,
    sequence: u32,
    responses: u32,
    pending: Option<(u32, u64, u64, u64)>, // first present ID, original fence, input QPC, submission QPC
}
impl Feedback {
    fn new(frequency: u64) -> Result<Self> {
        Ok(Self {
            output: open("Z:\\frames\\runtime-feedback.log", true)?,
            frequency,
            ready: false,
            sequence: 0,
            responses: 0,
            pending: None,
        })
    }
    fn emit(&self, line: &str) -> Result<()> {
        write_handle(self.output.0, line.as_bytes())?;
        // SAFETY: this owner retains its valid writable file handle. Flush makes
        // the small record visible through virtiofs; no GPU work/readback is added.
        if unsafe { FlushFileBuffers(self.output.0) } == 0 {
            return Err("flush runtime feedback".into());
        }
        Ok(())
    }
    fn observe(
        &mut self,
        renderer: &Renderer<'_>,
        sequence: u32,
        draw: &DrawObservation,
    ) -> Result<()> {
        if sequence != self.sequence {
            if sequence != self.sequence + 1 || self.pending.is_some() {
                return Err("runtime feedback input overtook pending frame".into());
            }
            self.sequence = sequence;
            self.pending = Some((
                draw.present_count,
                draw.fence,
                INPUT_QPC.load(Ordering::Relaxed),
                counter()?,
            ));
        }
        let mut stats = DXGI_FRAME_STATISTICS::default();
        // SAFETY: initialized writable result and live owned swapchain.
        let result = unsafe { renderer.chain.GetFrameStatistics(&mut stats) };
        if let Err(error) = result {
            if error.code() == DXGI_ERROR_FRAME_STATISTICS_DISJOINT {
                return Ok(());
            }
            return Err(format!("runtime frame statistics unavailable: {error}"));
        }
        if stats.PresentCount == 0 {
            return Ok(());
        }
        let time =
            u64::try_from(stats.SyncQPCTime).map_err(|_| "negative runtime feedback clock")?;
        let polled = counter()?;
        if time == 0 || time > polled {
            return Err("runtime feedback clock is invalid".into());
        }
        if !self.ready {
            self.emit(&format!("LEGION_FEEDBACK_READY {}\n", self.frequency))?;
            self.ready = true;
        }
        if let Some((target, fence, input, submitted)) = self.pending {
            // SAFETY: this reads the original queue fence, never a replacement completion.
            let completed = unsafe { renderer.gpu.fence.GetCompletedValue() };
            let polled = counter()?;
            if completed == u64::MAX {
                return Err("original GPU fence reported device removal".into());
            }
            if stats.PresentCount >= target && completed >= fence {
                if time < input || submitted < input || polled < submitted {
                    return Err("runtime frame feedback predates its input".into());
                }
                self.emit(&format!(
                    "LEGION_FEEDBACK {} {} {} {} {} {} {} {} {}\n",
                    self.sequence,
                    target,
                    stats.PresentCount,
                    input,
                    submitted,
                    time,
                    polled,
                    fence,
                    completed
                ))?;
                self.responses += 1;
                self.pending = None;
            }
        }
        Ok(())
    }
}

pub(super) fn run(config: &Config, frequency: u64) -> Result<()> {
    let settings = config
        .presentation
        .as_ref()
        .ok_or("missing presentation configuration")?;
    LATENCY_MODE.store(settings.latency_probe, Ordering::Relaxed);
    LATENCY_DELAY_MS.store(u64::from(settings.latency_delay_ms), Ordering::Relaxed);
    let window = Window::new(config.width, config.height)?;
    window.settle(config.width, config.height)?;
    let mut gpu = Gpu::new(config, frequency)?;
    let original_hash = gpu.verify()?;
    let mut renderer = Renderer::new(&mut gpu, &window, settings.fragment_iterations)?;
    if settings.latency_probe {
        renderer.fragment_iterations |= 1 << 31;
    }
    if settings.frame_tags {
        renderer.fragment_iterations |= 1 << 30;
    }
    let cpu_expected = (settings.cpu_rounds != 0).then(|| cpu_work(settings.cpu_rounds));
    let mut cpu_frames_verified = 0_u64;
    let mut checks = Vec::new();
    let mut samples = Vec::with_capacity(1_000_000);
    let mut diagnostic_frames = Vec::new();
    let mut tagged_frames = Vec::new();
    let mut stage_samples =
        Vec::<[u64; 8]>::with_capacity(if settings.stage_probe { 1_000_000 } else { 0 });
    let mut blocked_slots = 0_u64;
    let mut resizes = 0;
    let mut resize_stage = 0;
    let mut final_check = false;
    let mut feedback = settings
        .runtime_feedback
        .then(|| Feedback::new(frequency))
        .transpose()?;
    let start = counter()?;
    let mut before = start;
    loop {
        window.messages();
        let closing = CLOSING.load(Ordering::Relaxed);
        let now = counter()?;
        let elapsed = now - start;
        if elapsed >= frequency * u64::from(settings.seconds) && !closing {
            break;
        }
        if settings.resize {
            let phases = u64::from(settings.resize_cycles) * 2;
            let phase =
                (elapsed * (phases + 1) / (frequency * u64::from(settings.seconds))).min(phases);
            if phase > resize_stage {
                window.resize(
                    if phase % 2 == 1 {
                        config.width * 3 / 4
                    } else {
                        config.width
                    },
                    if phase % 2 == 1 {
                        config.height * 3 / 4
                    } else {
                        config.height
                    },
                )?;
                resize_stage = phase;
            }
        }
        let (width, height) = window.size()?;
        if width == 0 || height == 0 {
            if closing {
                break;
            }
            // SAFETY: the owned GUI thread yields while minimized; no GPU resource is reused.
            unsafe {
                Sleep(10);
            }
            continue;
        }
        if width > 3840 || height > 2160 {
            return Err("presentation window exceeds bounded dimensions".into());
        }
        let mut diagnostic = samples.is_empty() || closing;
        if (width, height) != (renderer.width, renderer.height) {
            renderer.resize(width, height)?;
            resizes += 1;
            diagnostic = true;
        }
        if !final_check && elapsed >= frequency * u64::from(settings.seconds - 3) {
            diagnostic = true;
            final_check = true;
        }
        let sequence = if settings.latency_probe {
            PALETTE.load(Ordering::Relaxed) as u32
        } else {
            (elapsed * 60 / frequency) as u32 ^ PALETTE.load(Ordering::Relaxed) as u32
        };
        let source_frame = if settings.frame_tags {
            let id = u16::try_from(samples.len() + 1).map_err(|_| "source frame tag exhausted")?;
            if sequence > u32::from(u16::MAX) {
                return Err("input generation tag exhausted".into());
            }
            u32::from(id)
        } else {
            0
        };
        let seed = sequence | (source_frame << 16);
        if let Some(expected) = cpu_expected {
            if cpu_work(settings.cpu_rounds) != expected {
                return Err("presentation CPU checksum mismatch".into());
            }
            cpu_frames_verified += 1;
        }
        if diagnostic && let Some(feedback) = &feedback {
            feedback.emit(&format!(
                "LEGION_FEEDBACK_READBACK {} {}\n",
                sequence,
                counter()?
            ))?;
        }
        let observed = renderer.draw(
            seed,
            diagnostic,
            settings.stage_probe,
            settings.runtime_feedback,
        )?;
        if diagnostic {
            diagnostic_frames.push(samples.len());
        }
        if let Some(feedback) = &mut feedback {
            feedback.observe(&renderer, sequence, &observed)?;
        }
        if settings.frame_tags {
            tagged_frames.push([
                source_frame as u64,
                u64::from(sequence),
                u64::from(observed.present_count),
                observed.fence,
            ]);
        }
        if let Some(hash) = observed.image_hash {
            checks.push(serde_json::json!({"width":width,"height":height,"seed":seed,"hash":hash,"pixels":u64::from(width)*u64::from(height)}));
        }
        let after = counter()?;
        if settings.stage_probe && !diagnostic {
            let [
                draw,
                waited,
                reset,
                recorded,
                executed,
                presented,
                signalled,
            ] = observed.clocks;
            stage_samples.push(
                frame_stage_ticks([
                    before, draw, waited, reset, recorded, executed, presented, signalled, after,
                ])
                .ok_or("source frame stage clock regressed")?,
            );
            blocked_slots += u64::from(observed.slot_blocked);
        }
        if samples.len() == samples.capacity() {
            return Err("presentation sample cap exceeded".into());
        }
        samples.push(after - before);
        before = after;
        if closing {
            break;
        }
    }
    renderer.gpu.wait()?;
    drop(renderer);
    let after = gpu.verify()?;
    if samples.is_empty() || checks.len() < 2 || original_hash != after {
        return Err("missing presentation GPU validation".into());
    }
    let frames = samples.len();
    let raw = serde_json::json!({
        "schema":1,"frequency":frequency,"ticks":samples,"diagnostic_frames":diagnostic_frames,
        "warmup_ms":config.warmup_ms
    });
    write_handle(
        open("Z:\\frames\\frame-times.json", true)?.0,
        &serde_json::to_vec(&raw).map_err(|error| error.to_string())?,
    )?;
    let timing = timing_summary(&mut samples, frequency).ok_or("no presented frames")?;
    let fps = frame_rate(&samples, frequency).ok_or("invalid source frame rate")?;
    let mut stage_report = serde_json::Map::new();
    for (index, name) in [
        "messages_and_setup",
        "slot_wait",
        "allocator_list_reset",
        "record_and_close",
        "execute",
        "present",
        "signal",
        "frame_tail",
    ]
    .into_iter()
    .enumerate()
    {
        if !stage_samples.is_empty() {
            let mut ticks: Vec<u64> = stage_samples.iter().map(|sample| sample[index]).collect();
            stage_report.insert(
                name.into(),
                serde_json::to_value(
                    timing_summary(&mut ticks, frequency).ok_or("missing source frame stages")?,
                )
                .map_err(|e| e.to_string())?,
            );
        }
    }
    let xcr0 = if core::arch::x86_64::__cpuid(1).ecx & (3 << 26) == 3 << 26 {
        // SAFETY: CPUID confirms XSAVE/OSXSAVE before executing XGETBV(0).
        unsafe { core::arch::x86_64::_xgetbv(0) }
    } else {
        0
    };
    let report = serde_json::json!({"schema":1,"passed":true,"gpu_verified":true,"workload":"windows_d3d12_presentation_v1",
        "adapter":gpu.adapter_name,"vendor_id":gpu.vendor_id,"device_id":gpu.device_id,"xcr0":xcr0,"config":config,
        "frames":frames,"elapsed_ms":(before-start) as f64*1000.0/frequency as f64,"frame_cadence":timing,"source_render_fps":fps,
        "source_frame_stages":stage_report,"profiled_frames":stage_samples.len(),"profiled_slot_waits":blocked_slots,"diagnostic_frames_excluded_from_stage_profile":checks.len(),
        "image_checks":checks,"pipeline_changes_per_frame":2,"draws_per_frame":2,"swapchain_buffers":3,
        "fragment_iterations":settings.fragment_iterations,"frame_cpu_rounds":settings.cpu_rounds,
        "latency_probe":settings.latency_probe,"runtime_feedback":settings.runtime_feedback,"frame_tags":settings.frame_tags,
        "tagged_frames":tagged_frames,
        "runtime_feedback_samples":feedback.as_ref().map_or(0, |feedback|feedback.responses),"latency_delay_ms":settings.latency_delay_ms,
        "cpu_frames_verified":cpu_frames_verified,"expected_cpu_checksum":cpu_expected,
        "resize_count":resizes,"space_events":KEY_EVENTS.load(Ordering::Relaxed),"pointer_events":POINTER_EVENTS.load(Ordering::Relaxed),
        "close_requested":CLOSING.load(Ordering::Relaxed),"original_queue_drained":true});
    write_handle(
        open("Z:\\frames\\workload-result.json", true)?.0,
        &serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
    )?;
    log("LEGION_PROTON_PRESENTATION_PASSED");
    Ok(())
}
