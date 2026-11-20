# nvkvm-pv investigation

Research branch: `research/nvkvm-pv`, based on Legion `79a6ec0`.
Investigation date: 2026-11-19. The packaged launcher still uses Venus; this
branch is developing driver forwarding as the performance-first direction.

The prototype now presents verified Vulkan and stock-Proton D3D12 output in a
real host window through direct DMA-BUF scanout. AMDGPU DRM native context also
passes offscreen rendering, visible cube and stock-Proton D3D12 presentation
checks on the integrated GPU with Venus disabled, including
AMD-render/NVIDIA-display operation.
The owned UE5 D3D12 benchmark now completes on NVIDIA with stock Proton.
Broader game compatibility, cursor behavior and sustained frame times remain
qualification work.

The physical-desktop follow-up on November 20 changes the latency conclusion:
early EGL swap improved the headless diagnostic but **increased** response time
on Hyprland. Keep the original submission order. A one-frame VKD3D queue lowers
the reactive fixture's response time, with a substantial UE5 throughput cost;
it also remains opt-in. Detailed results and measurement limits follow below.

## Sources and design

The checkout is pinned to
[`39972874d22f5ba19e39827488b089e80a0c17f4`](https://github.com/reindertpelsma/nvkvm-pv/tree/39972874d22f5ba19e39827488b089e80a0c17f4).
The research builder verifies Legion's existing QEMU 11.1.2 source archive and
applies the upstream patch series with zero fuzz. Both the guest module and
the embedded host stub come from that pinned checkout. No nvkvm host module
is needed: the new kernel module runs inside the disposable guest.

Venus serializes Vulkan commands to a host Vulkan implementation. nvkvm-pv
instead gives the guest unmodified NVIDIA userspace, forwards driver control
calls through its own virtio device and isolated host processes, and maps GPU
memory/doorbells into the VM. GPU work can therefore avoid a Vulkan protocol
round trip. This is a plausible way to reduce submission overhead; it also
brings NVIDIA's private driver ABI back into Legion's compatibility surface.

The public [parity report](https://github.com/reindertpelsma/nvkvm-pv/blob/39972874d22f5ba19e39827488b089e80a0c17f4/docs/reference/parity.md)
contains strong sustained compute results. Those are not Proton game FPS
measurements. Upstream also records a higher kernel-launch latency and
workloads with less favorable results.

The [guest library instructions](https://github.com/reindertpelsma/nvkvm-pv/blob/39972874d22f5ba19e39827488b089e80a0c17f4/docs/howto/stage-guest-libraries.md)
require NVIDIA userspace matching the host driver. The project measures and
selects versioned ABI profiles; it does not eliminate driver version coupling.
Our research bundle collects the local driver into an ignored build directory.

## Local qualification

Host: Ryzen 7 9800X3D, RTX 4070 Ti SUPER (`10de:2705`), CachyOS kernel
7.3.4-1-cachyos, NVIDIA 620.70.06. Guest: Legion Linux 6.18.62, 4 GiB RAM.
The smoke/Proton tests initially used 4 vCPUs; the transport comparison uses
16 vCPUs. No host driver, installed Proton or game prefix is modified.

Build and correctness evidence is under `build/nvkvm-pv-repro/`:

| Check | Evidence / result |
| --- | --- |
| Upstream unit checks | `unit-check.log`: 17 suites, all 9 isolate cases pass |
| QEMU integration | `qemu-build.log`: builds; device list contains both custom devices |
| Guest module | `guest-build.log`: builds against Legion's existing kernel |
| NVIDIA structure probes | `abi.log`: measured 620.70.06 fields match the newest 610 ABI profile |
| Vulkan enumeration/rendering | `smoke-01/`: correct proprietary NVIDIA device and rendering checksum |
| Stock GE-Proton11-11, D3D11/DXVK | `d3d11-stock-02/`: fixture passes, before/after image hashes agree |
| Stock GE-Proton11-11, D3D12/VKD3D | `d3d12-stock-01/`: fixture passes, queue depths 1–3 and mixed CPU/GPU work |
| Restricted CPU policy, D3D11/DXVK | `d3d11-policy-01/`: fixture passes with XCR0 = 7 |
| Restricted CPU policy, D3D12/VKD3D | `d3d12-policy-01/`: fixture passes with XCR0 = 7 |

The initial smoke and Proton records exposed the full host CPU feature set.
Do not treat their throughput as an exact match for the production CPU policy.
The portable init and restricted-CPU qualification are described below.

## Performance method

`tools/benchmark_nvkvm.py` interleaves native Linux, the packaged Venus runtime
and nvkvm-pv. It uses the identical owned Linux Vulkan executable and JSON
configuration, 1920×1080 offscreen targets, three frames in flight, one second
of warmup and three seconds of sampling per case, and three repetitions.
The cases use 1, 64 and 256 integer shader iterations. Pixel checksums must
agree across all transports; the selected hardware must also agree.

These are synthetic completed-frame rates. They exclude WSI, scanout,
compositor copies, game logic, texture streaming, pipeline diversity and input.
The native/VM result includes differences in userspace libraries and scheduling;
it is not a measurement of protocol cost alone. Host GPU clocks are not locked.
The small sample count and desktop activity limit the precision of small gains.

`comparison-04/report.json` uses Legion's restricted CPU feature policy for
both guest transports and the portable EGL/GBM helper for NVIDIA. All 27
case results pass and have matching rendering checksums. Ratios divide each
transport's median rate by the native median:

| Workload | Native median synthetic frames/s | Venus / native | nvkvm / native |
| --- | ---: | ---: | ---: |
| Light 1080p, 1 shader iteration | 49,009 | 62.1% | 82.2% |
| 1080p, 64 shader iterations | 14,541 | 77.8% | 89.6% |
| 1080p, 256 shader iterations | 4,342 | 85.9% | 90.7% |

The samples vary materially: the native light test ranges from 43,201 to
49,055 frames/s, Venus from 30,344 to 35,263, and nvkvm from 39,792 to 46,065.
Later repetitions are generally faster across transports. Without recorded
clock telemetry this experiment cannot attribute that drift or establish a
precise overhead percentage. It supports further graphics testing, not a game
FPS promise. `comparison-01/report.json` preserves the earlier full-feature-CPU
exploration; `comparison-02` and `comparison-03` preserve the portability
failures encountered before the portable init/helper correction.

## Driver and environment issues found

The 620.70.06 structure sizes are compatible with upstream's latest profile,
but its NVKMS command numbering is not covered. The backend rejects
`cmdType=16 size=152` during the graphics probes. Reading NVIDIA's exact
620.70.06 `NvKmsIoctlCommand` enum identifies 16 as `REGISTER_SURFACE`, 17 as
`UNREGISTER_SURFACE`, 18 as `GRANT_SURFACE`, and 59/60/61 as the vblank semaphore
commands. Upstream only marks the newer command-number regime known through
610, whose surface registration is numbered 17. Evidence:
`nvkms-api-620.h` and `nvkms-620-enum.json`.

The tracked patch in `tools/nvkvm-patches/` adds the measured 620.70 family
explicitly. The tests verify its surface and vblank commands and reject the
neighboring sharing/fliplock commands and unknown branches. The expanded
allowlist suite passes 769 assertions; the complete upstream suite still
passes. The original `build/nvkvm-pv-repro/source` remains unmodified; the new
builder applies and fingerprints local patches in a separate source overlay.
Do not widen this to other unmeasured driver families.

The host's static glibc and EGL/GBM helper contain unconditional AVX-512
instructions. A first attempt to use Legion's normal XSTATE/AVX-512 restrictions
therefore produced SIGILL. The trace in `trace-01/vulkaninfo.stderr` locates the
second fault in `libnvidia-egl-gbm.so.1.1.3`, at offset `0x4558`
(`vinserti64x2`). This is an environment packaging issue, not evidence that the
NVIDIA forwarding protocol needs AVX-512. The research tools now compile init
against portable musl, prefer Legion's bundled generic libraries over host
dependencies, and stage the independent Debian EGL/GBM helper from the existing
build image. Host packages remain untouched.

Upstream's current presentation code probes the host UI renderer and selects
zero-copy for NVIDIA. Cross-vendor hybrid display with NVIDIA block-linear
buffers can fall back to CPU readback. Older upstream notes describing an
unconditional readback default are stale. The same-NVIDIA zero-copy path has
now been exercised; cross-vendor display is not qualified here.

## Visible NVIDIA presentation

Artifacts for the patched prototype are in `build/nvkvm-pv-display/`.
The upstream GLES scanout harness still fails at `eglCreateImageKHR` with
`EGL_BAD_PARAMETER`; enabling surface registration alone does not fix that
allocation/import path. The separate GBM harness completes 180 flips, but its
CPU mapping fails, leaving uninitialized storage. Those flips are plumbing
evidence only, not correct-pixel evidence.

Legion's compositor now has an explicitly selected `nvidia-drm` research path.
It reads the primary plane's actual `IN_FORMATS` blob, advertises those layouts
to the Wayland client, PRIME-imports the client's rendered buffer and preserves
its real modifier in ADDFB2. This presents the rendered buffer directly,
avoiding EGL re-import into a guest compositor framebuffer. The portable blob
decoder checks all ranges and rejects truncated or invalid references. The
default virtio-GPU path keeps its existing linear layout policy.

| Visible check | Result |
| --- | --- |
| `cube-window-01` | 558 completed scanouts in 10 seconds; 3 imported buffers; rotating Vulkan cube captured in the exact host Wayland window |
| `proton-window-01` | Stock GE-Proton11-11, D3D12/VKD3D presentation fixture: 892 rendered frames in 15.01 seconds, image checks pass, XCR0 = 7 |
| `proton-window-02` | Repeated after the shared guest/environment changes: 893 completed scanouts; client and image checks pass |
| `cube-window-02` | Repeated with final probe metadata: visible pixels and exact-window capture verified |
| Host presentation | QEMU reports `GL zero-copy`, with NVIDIA as the host UI renderer |
| Guest transport | `nvkvm_kms_dmabuf`, zero steady-state pixel readbacks |
| Rust checks | 36 compositor tests pass, including malformed format-blob tests; Clippy passes with warnings denied |
| Existing Venus scanout | `venus-regression-01`: the new compositor passes its default linear virtio-GPU cube path, with 4,584 scanouts in 5 seconds |

The initial roughly 60 Hz limit came from QEMU's SDL resize event omitting the
host display's refresh rate. The local QEMU patch reports the actual output
rate, including display changes, in the mHz expected by QEMU. On this 239.964 Hz
monitor, `build/nvkvm-pv-perf/cube-refresh-01` completes 2,232 scanouts in ten
seconds, compared with 557 before the fix. This cube rate is a presentation
check, not a game performance comparison. Headless research can select an
explicit bounded fallback using `--kms-hz`.

Host captures were inspected alongside frame counters and client image checks.
The research compositor does not enable a hardware cursor on the NVIDIA head.
Cursor, relative mouse, host shortcuts and arbitrary window sizes need explicit
qualification before replacing normal game launches. The local SDL input patch
preserves host shortcuts in desktop mode and releases modifiers on focus loss;
upstream's explicit Ctrl-Alt-G grab remains available. This policy is not yet
an end-to-end shortcut/cursor qualification.
The driver also logs unsupported RM controls/classes in the D3D12 run; a passing
fixture does not certify every driver feature.

## UE5 and sparse memory

The owned UE5 benchmark initially failed in VKD3D's sparse bind path with
`VK_ERROR_DEVICE_LOST`. An isolated Vulkan probe reproduced this on its first
`vkQueueBindSparse`, while passing natively. The driver sends the inline
`NV00FE_CTRL_CMD_SUBMIT_OPERATIONS` structure: 229,392 bytes on measured NVIDIA
620.70.06, exceeding the upstream guest's 64 KiB control slot. The command was
also absent from the host allowlist.

The local patches add a 256 KiB slot, allow this specific control with exact
structure-size, operation-count and operation-type checks, and route it away
from the small fast ring. The 256 slots consume 64 MiB rather than 16 MiB.
Each end of the isolate's SEQPACKET socket gets a bounded send buffer: the
host's default 212,992-byte limit otherwise rejects the payload and leaves the
peer waiting for data after its header. No host driver or kernel setting is
changed. Malformed structures and unknown operation types remain rejected.

UE5 also needs `NV00FE_CTRL_CMD_RESIZE_QUEUE`, a four-byte inline count. This is
separately allowed with a maximum of 65,536 entries and exact size checks; the
benchmark requests 16,384 then 32,768. Both controls receive host validation,
including when the fast ring is available. These checks describe the measured
620.70.06 layout, not automatic qualification of other driver versions.

`build/nvkvm-pv-perf/sparse-04` passes four bind/rebind rounds, GPU fills, copies
and complete readback checks using the owned `tools/nvkvm_sparse_probe.c`.
The builder compiles this portable fixture and records its source/binary hashes.
All 17 upstream unit suites and nine isolate cases pass after the changes,
including added malformed sparse-control tests.

`ue5-03` then completes with stock GE-Proton11-11, D3D12, 1920×1080, VSync off,
100% screen percentage and Legion's XCR0=7 CPU policy. Its score is 84.79 FPS;
the earlier native control scored 88.01 FPS (96.3%). The VM reports the real
NVIDIA GPU and its exact host window shows the animated scene; presentation
uses the zero-copy path. This initial comparison is exploratory: the original
native control did not isolate NVIDIA's global shader cache. The paired runner
now provides separate fresh Proton, VKD3D and NVIDIA cache directories for both
modes. Native CPU policy remains unrestricted, as for normal host play.

The repeat with private caches is `build/nvkvm-pv-perf/ue5-pair-02`:

| Mode | Application-reported FPS |
| --- | ---: |
| Native, stock GE-Proton11-11 | 92.15 |
| nvkvm-pv, same Proton and scene | 84.29 |

This is **91.47% of native**, or an 8.53% reduction. The previous paired run,
before isolating the native driver cache, scored 92.69/84.91 FPS. The three
completed VM runs span 84.29–84.91 FPS. Use the 91.5% private-cache comparison
for the current result, rather than presenting the first 96.3% observation as
established performance. These runs do not compare against Venus.

The final `input-sparse-01` run also passes the rebuilt sparse fixture, rotating
cube, exact host-window capture and injected A press/release plus absolute
pointer movement. Its diagnostics report two keyboard events and an absolute
pointer event. The final build repeats all 36 compositor tests and the complete
upstream unit suite. Kernel logs are collected directly by the owned guest init,
without depending on a `dmesg` executable in the slim guest image.

`tools/benchmark_nvkvm_ue5.py` records arguments, executable identity, scores and
per-run artifacts. It alternates ordering when multiple pairs are requested.
GPU clocks are not locked. Inspection of the application-provided CSV shows
that its `Time` column is a sequential frame index, and the arithmetic mean of
its rounded instantaneous-FPS values matches the displayed score. Inverting
those values estimates frame periods; these are not GPU timestamps or an
independent wall-clock trace. `tools/ue5_frame_log.py` now records the score,
estimated trace duration, FPS from logged periods, and estimated frame-period
percentiles separately. The original 8.53% figure describes the displayed
score gap. For example, the private-cache native trace yields approximately
90.17 FPS from logged periods, versus 80.10 for `opt-baseline-02`, with estimated
p99 periods of 15.37 and 28.97 ms respectively. The stutter gap is therefore
larger than the displayed score alone suggests.

## Performance experiments after the first UE5 result

Artifacts are in `build/nvkvm-pv-perf/opt-*`; the generated summary is
`optimization-study-01.json`. Runs use the same stock Proton, owned application,
1920×1080 settings, fresh private caches and Legion CPU restrictions unless
explicitly identified as a control. Timed UE5 trials run sequentially.
GPU clocks/load and owned QEMU thread CPU times are recorded; clock locking
was not used. Host presentation averages about 0.25 ms per frame with no drops
or failures reported by the SDL path.

| Experiment | Application score (FPS) |
| --- | ---: |
| Historical polling baseline, `opt-baseline-02` | 84.58 |
| Historical polling recheck, `opt-baseline-final-01` | 84.31 |
| 1000 Hz virtual cadence | 84.67 |
| Host-matching 8-core/16-thread topology | 84.51 |
| Matching topology plus vCPU pinning | 83.60 |
| Event-driven compositor and throttled exit-marker checks | 84.15 |
| Command-ring idle budget 20,000 rather than 2,000 iterations | 84.96 |
| Unmasked host CPU features, diagnostic only | 84.65 |
| Direct Wayland broker, first run | 85.50 |
| Direct Wayland broker, repeat | 84.52 |
| SDL host window fullscreen | 83.27 |
| Normalized KMS/Wayland refresh reporting | 83.15 |

The broker's apparent first-run gain did not repeat: 84.52 versus the final
polling baseline's 84.31 is only 0.25%, within observed run variation. These results do not
establish a material FPS improvement. The event-driven
variant does reduce guest compositor CPU time from about 9.7–10.0 seconds to
2.27 seconds per run (roughly 77%), and combined QEMU/vCPU CPU use drops from
about 701% to 673%, where 100% is one logical CPU. This is a CPU-efficiency
result, not an FPS gain. It passes NVIDIA and AMD visible rendering and injected
input checks. Event-driven waits are now the default for KMS presentation on
both native backends, including configurations that omit `kms_wait`. The
research runner and paired benchmark also default to events. Use
`--kms-wait sleep` (or `"kms_wait":"sleep"` in a KMS configuration) to reproduce
the historical polling baseline. Event mode checks the virtiofs exit marker
every 50 ms and services input with a bounded 1 ms timeout. This change applies
to the KMS loop; the packaged Venus launcher's separate forwarding loop keeps
its existing policy. The 77% figure describes compositor CPU time.

The optional direct Wayland path builds upstream's display broker and uses
`-display nvkvm-broker` instead of SDL. The broker accepts the guest's native
DMA-BUF modifier, avoiding QEMU's intermediate GL import/blit. Clipboard is
explicitly off, the socket is inside the run's private directory, and
`--present-mode=native` refuses a CPU-copy fallback. The cube, exact-window
capture, input injection, broker protocol/authentication/clipboard/lifecycle
tests pass. The host compositor still reports composition (`COPY` in its
presentation feedback), so this is not physical direct scanout. The tiled
broker also preserves aspect ratio while SDL stretches its window; this is
another reason not to attribute a roughly 1% score difference solely to the
relay. Fullscreen comparisons must match the host presentation geometry.

A synchronization mismatch was also found: the native control uses NTSYNC,
whereas the lightweight research guest uses FSYNC because its minimal init
did not load the packaged NTSYNC module. The normal Legion runtime already
packages it. `--ntsync` now loads that matching module and verifies activation
in both prefix and application logs. The first run completed at 82.36, with
activation in `prefix.log`; its original validator checked only `proton.log`
and consequently marked the report failed. This validation bug is corrected.
That observation does not justify enabling/disabling a synchronization backend
globally. It also means these end-to-end score gaps must not be called pure
NVIDIA forwarding overhead.

The virtual head's software timer followed host refresh, but its standard DMT
modes and Wayland output still advertised 60 Hz. An opt-in correction now scales
the virtual mode clocks to the actual cadence and updates Wayland output modes;
a real Wayland protocol test verifies 240,000 mHz reaches the client. The corrected
NVIDIA cube reports 240 Hz and passes visible/input checks. It did not improve
UE5, so mode normalization is off by default and selected only by
`--kms-refresh kms`. Display cadence, buffer-release backpressure, CPU policy,
topology and synchronization choices remain unchanged.

Hardware `perf` attachment to the owned QEMU was rejected by the host's access
controls, as was callchain sampling. Launching the owned QEMU as a child of
`perf record -e cycles:uH` does allow flat userspace sampling. The research
runner exposes that diagnostic through `--profile-host`; its samples exclude
guest execution and host kernel execution, and include startup unless the
time interval is filtered. No host permissions or security settings were changed.

### Submission and poll registration study

The next study keeps the event-driven KMS default and uses stock GE-Proton
11-11 with FSYNC on both sides. `benchmark_nvkvm_ue5.py --fsync` disables native
NTSYNC for this control and checks which backend actually activated. This is
a measurement control, not a change to the packaged launcher's sync policy.

Short shader traces separate GPU timestamps from CPU submission and fence-slot
waits. GPU execution is approximately 59–60 microseconds on both paths, while
the guest's median record-and-submit stage is about 22 microseconds versus
approximately 4 microseconds in the earlier native trace. Those observations
identify a CPU-side lead; they do not establish the cause of the UE5 FPS gap.

`--request-stats` adds opt-in QEMU request counts. The short shader diagnostic
made about 70,000 poll-arm requests. The eight-vCPU UE5 trial made 561,600,
alongside 14,705 forwarded ioctls. The owned Linux workload's diagnostic preload
also confirms that most poll calls are blocking waits. Request frequency alone
does not prove that these waits limit the rendering thread.

Two separately selectable experiments investigate registration:

- `--poll-arm async` queues guest work holding a file reference until the arm
  finishes. Its first UE5 score, 84.56, does not improve the 85.06 synchronous
  observation. The default remains synchronous.
- `--poll-transport enqueue` keeps the guest syscall path but removes the
  helper's registration-only acknowledgement. QEMU sends a bounded,
  nonblocking SEQPACKET command; the ordinary GPU-completion notification still
  wakes the guest. Ownership checks, one-shot registration, command ordering,
  ioctl results and GPU fence waits remain in place. Backpressure returns an
  error so the guest can reset its arm flag. This is opt-in research.

The enqueue implementation passes all 17 SDK suites and ten isolate cases,
including no-ACK command sequencing, reserved-flag sanitization, a stopped
helper/socket backpressure case, and requests against dead isolates. The
disposable guest also passes GPU known-answer validation, 400 concurrent
poll/close cycles and non-forced module unload. A stock D3D12 swapchain run
passes exact-window capture, keyboard/pointer input and NTSYNC activation.

The unload stress test found an existing KMS lifetime defect on both synchronous
and asynchronous paths: DRM managed cleanup runs in reverse registration order,
so allocating the embedded display objects after mode-config registration freed
their storage too early. The local patch allocates that storage first, retaining
it through mode-object cleanup. The failing and passing guest logs are retained.

The same study rules out two simple changes for this workload: staging its
1.5 GiB application in guest tmpfs scored 83.95, and reducing the VM to eight
vCPUs scored 82.03. These are individual exploratory observations, not general
claims about filesystem or CPU scaling. Neither becomes a default.

Raw results are under `build/nvkvm-pv-perf/submit-study-01` and
`build/nvkvm-pv-perf/ue5-submit-pair-01`. The synchronous 85.06 observation has
a complete, numerically consistent saved frame log, but its original harness
failed while waiting for the result file to stop changing. The benchmark may
rewrite its results after finishing; the runner now reads a complete snapshot
whose reconstructed mean matches the score instead. A separate truncated-CSV
race affected the first native runner; its saved 93.69 result was recovered and
checked against the complete frame log. Keep those harness failures visible
when interpreting the raw reports. Subsequent comparisons use the corrected
snapshot collector.

For deeper diagnostics, `--queue-profile` uses the stock Proton build's
[VKD3D queue timeline](https://github.com/HansKristian-Work/vkd3d-proton/blob/master/libs/vkd3d/queue_timeline.c).
The guest writes its trace into tmpfs and copies it out after the application
stops. These instrumented runs are excluded from FPS conclusions; the timeline
records CPU submission and observed completion, not direct GPU execution time.
The raw stream is retained as `queue-profile.raw.json`; a separate normalized
JSON array contains its complete records because forced application shutdown
can leave the original stream without its closing bracket. The report records
any incomplete records discarded during normalization.

The repeated enqueue score was 85.07 against a fresh synchronous control of
84.70. Together with the first 85.78 result, this is too small and variable to
establish a material FPS improvement. The registration policy stays opt-in.
The D3D12 queue-profile qualification delivered 77,955 notifications with zero
event-buffer drops. A full aligned-RAM UE5 run delivered 598,425 with zero
drops, so notification loss does not explain those trials' FPS gap.

Host memory inspection exposed another comparison mismatch: the research
runner's ordinary memfd mapping began 4 KiB before a 2 MiB boundary, with zero
`ShmemPmdMapped` despite its hugepage hint. `src/memory.rs` in the packaged
launcher already uses an aligned, size-sealed, shared memfd through QEMU's
file backend. The research runner now matches that design by default, retaining
the parent-owned descriptor until QEMU exits. `--ram-layout memfd` reproduces
the earlier allocation. This correction changes the research harness, not the
packaged launcher's existing memory policy.

That initial alignment experiment retained the system THP policy: its D3D12
qualification shows 745,472 KiB of PMD-mapped shared memory versus zero previously
and passes rendering, input and NTSYNC. The packaged launcher separately disables
THP in the owned QEMU process (`src/qemu_policy.rs`), so the research runner now
also defaults to `--host-thp off`, using the same internal QEMU exec stage.
`--host-thp system` reproduces the earlier study. No host-wide memory settings
are changed. NVIDIA D3D12/NTSYNC/input and AMD cube/input pass with both
production memory defaults (`production-memory-nv-01`, `production-memory-amd-01`). The first aligned
UE5 observation was 85.32, so alignment alone has not established an FPS win.
Its guest-captured frame summary is complete (116.033 seconds reconstructed),
but the old host collector reread an original CSV truncated during shutdown
and marked that report failed. Both collectors now preserve the validated
score and CSV bytes as `BenchmarkResults.snapshot.txt` and
`BenchmarkResultsPerFrame.snapshot.csv` before stopping the application; they
never use the rewriting originals as the authoritative result afterward.

### Presentation synchronization experiment

A matched, instrumented FSYNC UE5 comparison (`ue5-queue-profile-pair-01`)
finds median VKD3D `CALLBACK` spans of 303.2 microseconds natively and 5,294.3
microseconds in the guest, excluding the first 20 seconds. The callback region
includes swapchain acquire/present work; these are CPU-observed spans, not GPU
timestamps. Median low-level submission overhead is 4.0 versus 8.7 microseconds.
Instrumented FPS scores are excluded from performance conclusions.

The native NVIDIA driver exposes SYNC_FD fence/semaphore feature flags 3/3;
the guest previously exposes 0/0. Upstream deliberately clears `supports_sync_fd`
because a host kernel sync-file descriptor cannot be returned as a guest fd.
The opt-in `--sync-file-bridge` prototype adds actual host-to-guest fences rather
than merely changing that capability flag:

- A negotiated, graphics-only device feature defaults off. The host exports a
  fence from an owned render file and passes its native sync file over SCM_RIGHTS.
  QEMU validates it and keeps it as a typed, session-owned handle.
- A guest DMA fence monitors that handle's actual kernel status. Notifications
  wake the monitor; only a terminal native fence status signals completion.
  Transport failures cannot signal successful completion. Independent exports
  have distinct timeline contexts and retain their session/module references.
- The guest implements SEMSURF fence export/attach and the newer NVIDIA syncobj
  point export/get-syncfd operations. Guest syncobj IDs stay in the guest.
  Implicit KMS acquire waits use the normal DRM reservation-fence helper.

The owned `nvkvm_sync_file_probe.c` submits work behind a CPU-signalled timeline
gate. Both native and guest produce a genuinely pending sync file, return from
export without waiting for the gate, remain unreadable before completion, and
report status 1 after completion. Guest Vulkan cube and stock D3D12/NTSYNC runs
also pass visible capture and injected input (`build/nvkvm-pv-syncfd/pending-fence-03`,
`proton-syncfd-01`). All 17 SDK unit suites, 11 isolate cases and 37 compositor
tests pass. The first two prototype runs failed because the guest response-type
range rejected the new request; the explicit correction is patch 0015, and the
failed logs remain available.

At this stage this was an export/presentation experiment, not complete external-sync support.
Importing arbitrary pending guest fences back into the native driver, combined
fences, error propagation and close/removal stress still required qualification.
The private NVIDIA capability bit exposes both import and export, so the feature
must stay opt-in until those paths are implemented and tested. The packaged
runtime and AMD backend are unchanged. UE5 comparisons use the same source
and driver bundle, with production memory policy. The only functional source
change between the bridge-off control and corrected bridge-on trial affects
the opt-in fence monitor; the off path does not execute it.

The completed fresh-cache FSYNC comparison is recorded in
`build/nvkvm-pv-syncfd/fps-study.json`:

| UE5 D3D12, 1920×1080 | Application score (FPS) |
| --- | ---: |
| Native control | 91.53 |
| Bridge off | 84.21 |
| Bridge on, corrected wakeup | 83.33 |

The baseline is 92.0% of native. The corrected bridge is about 1.0% below the
off control, which does not establish a repeatable regression or any FPS gain.
Its p95/p99 reconstructed frame periods are 17.09/27.76 ms versus 17.87/29.17 ms
off; those small tail differences also need repeated trials. Scores are means of
instantaneous FPS, and these reconstructed periods are not GPU timestamps.

The initial bridge trial scored 62.27 because its monitor used an
uninterruptible wait with `wake_up_interruptible()`, so real notifications did
not wake it and completion fell back to a 10 ms timer. Patch 0016 matches the
wait state to the wakeup. The corrected pending-fence fixture observes the gate
signal in 0.112 ms, while retaining its pending-before-completion checks.
The runner refuses bridge builds missing the completion/wakeup corrections.

The corrected full UE5 diagnostic trace still shows a 5.457 ms median present
callback, versus 5.294 ms in the previous guest trace and 0.303 ms natively.
Median submission overhead is 9.2 microseconds, versus 8.7 guest/4.0 native
previously. `timeline-comparison.json` records counts, percentiles and cutoff;
`vkd3d_queue_profile.py --summary` reproduces these CPU-observed measurements.
The previous trace pair retained system THP while the new trace uses production
THP-off policy, so this comparison is diagnostic rather than an isolated
fencing-only experiment. Export support alone has not removed the present wait.
All instrumented FPS scores remain excluded from performance conclusions.

The pending import regression was then reproduced (`pending-import-03`). Importing
the exported SYNC_FD into a Vulkan binary semaphore and submitting a dependent
GPU wait both return success, but the dependent fence times out even though the
original sync file has reached status 1. The native version passes. The fixture
reports this failure and terminates only its owned test process rather than
hanging indefinitely in `vkDeviceWaitIdle`. It never declares the queued GPU
work complete. This expected failing interoperability test is separate from the
passing unit suites and export/presentation fixtures.

The next work identified here was to complete guest-to-host fence import and verify presentation
acquire/release ownership before enabling the Wayland explicit-sync protocol.
Simply advertising that protocol without delivering its release points can
strand buffers or permit premature reuse. The bridge stays disabled by default;
there is no new production FPS optimization to promote from this study.

### Completed import and asynchronous buffer release

The subsequent patches 0018–0024 implement and qualify that missing import and
ownership path. The earlier failed reports remain historical results. An
exported native sync file is now passed to the importing native render file
using a pinned descriptor and SCM_RIGHTS, checking both session/isolate owners.
Guest-created or merged fences register a completion callback; only successful
producer completion schedules the native semaphore update. Pending producers do
not occupy a waiting workqueue thread. Import cancellation retains the required
GEM/context/module references without retaining the closing file itself.

The owned fence fixture uses separate producer and consumer GPU queues. Both
native and guest keep the consumer pending until its producer completes. A
merged-fence test requires both producers to finish, including a check between
their two signals. An abandoned pending import also permits non-forced guest
module unload. These checks pass in `build/nvkvm-pv-import`, including
`pending-import-two-queues-01`, `pending-import-merged-01`, and
`import-lifetime-01`.

`--sync-file-bridge --kms-explicit-sync` additionally enables an experimental
Wayland timeline path in the direct KMS compositor. It requires the negotiated
strict-release guest feature. QEMU retires its old SDL GL texture and exports
an actual EGL native fence covering its final GPU reads. A bounded timer tracks
that fence and retries completed notifications when the guest event queue is
full. Failed fences never acknowledge safe reuse. Guest ownership distinguishes
queued use from host-held use, survives early acknowledgements, identifies
buffers by render-file handle and GEM, and never frees a strict host-owned
buffer through a timeout. Compositor teardown drops pending release points
unsignalled rather than declaring unverified GPU completion.

`build/nvkvm-pv-explicit/cube-explicit-02` and `proton-explicit-01` pass visible
rendering and keyboard/absolute-pointer checks. The latter also verifies stock
Proton D3D12 and NTSYNC. Thousands of real host-read completions occur with zero
fence errors, and request 34 confirms that ordinary presentation uses import.
The cube's scanout rate roughly doubles; this is a fixture result, not a game
FPS gain. The SDK's 19 unit suites / 12 isolate cases, 37 compositor tests and
Clippy with warnings denied pass. Unit checks cover pending/error fences,
notification backpressure, coalescing and queued/host-held ownership.

The counterbalanced six-run UE5 comparison is saved in
`build/nvkvm-pv-explicit/ue5-explicit-comparison-01/report.json`. All runs use
stock GE-Proton11-11, FSYNC, 1920×1080, fresh private caches and production
memory policy. The order is native/baseline/explicit, then reversed:

| UE5 D3D12 | Run 1 FPS | Run 2 FPS | Median FPS | Native percentage |
| --- | ---: | ---: | ---: | ---: |
| Native | 93.83 | 91.17 | 92.50 | 100.00% |
| nvkvm baseline | 83.38 | 82.83 | 83.11 | 89.84% |
| nvkvm explicit sync | 82.19 | 83.06 | 82.63 | 89.32% |

Explicit sync changes the median by −0.58%; there is no demonstrated FPS gain.
Both experimental features remain disabled by default. These are application
scores averaging instantaneous FPS; reconstructed frame periods are not GPU
timestamps. GPU clocks are unlocked and two samples per variant do not establish
small differences. Earlier measurements used different conditions and should
not be combined into this comparison.

The matching current-build diagnostic traces (`ue5-baseline-profile-01` and
`ue5-explicit-profile-01`) confirm that explicit sync reduces median VKD3D
presentation `CALLBACK` wall time from 5,390.5 to 417.65 microseconds (92.25%).
Its p95 falls from 9,432.72 to 992.2 microseconds. Median submission overhead
remains similar, 9.2 versus 9.6 microseconds. Both traces use FSYNC and production
memory settings, exclude their first 20 seconds, and pass rendering checks.
The explicit run records 8,886 imports and 8,889 real host-read completions,
with zero host fence errors. `timeline-comparison.json` preserves both summaries.
This removes a measurable presentation wait but does not prove an equivalent
critical-path or CPU-time saving: the separate uninstrumented FPS comparison
still shows no improvement. The remaining throughput limit needs further study.

A separate `--halt-poll-ns 0` trial scores 82.38 FPS
(`ue5-halt0-01`), again without a gain. The control applies KVM's per-VM capability
only to the owned research VM; the host-wide 200,000 ns policy remains unchanged.
It also stays opt-in. Instrumented timeline runs are excluded from these scores.

The new portable `tools/nvkvm_memory_probe.c` checks another possible source of
overhead: CPU access to mapped cached system memory and mapped device-local
memory. It uses identical volatile scalar loops on both sides, excludes the
initial write pass from timing, and verifies first/last writes through a real
GPU transfer. `build/nvkvm-pv-explicit/memory-comparison.json` records 32.01 versus
31.11 GB/s for cached system writes and 22.74 versus 22.41 GB/s for device-local
writes, native versus guest. Both GPU checks pass. This single diagnostic sample
does not show a large write-mapping/cache-policy penalty; it does not establish
bulk-copy throughput, game FPS or GPU memory bandwidth. The fixture's uncached
device-local CPU reads are slow on both sides, as expected for that mapping.

After the shared compositor changes, `amd-default-cube-01` again passes AMDGPU
native-context hardware rendering, actual host-window capture and injected
keyboard/absolute-pointer delivery, with experimental explicit sync disabled.

Final verification on the optional paths also passes:
`opt-final-proton-broker-01` uses stock Proton D3D12, the native-modifier broker,
NTSYNC (verified from its prefix log), corrected refresh reporting and injected
keyboard/absolute-pointer events. All image checks and exact-window capture
pass, with zero broker-rejected frames. `opt-final-cube-amd-01` repeats visible
AMD native-context rendering and input with event waits and KMS refresh
reporting. All 37 compositor tests, Clippy with warnings denied, and the 17
upstream unit suites / nine isolate cases pass. These checks establish optional
path correctness, not a new performance claim or a switch of the packaged
runtime. Event waits have since been selected as the KMS default for their CPU
efficiency; the other performance experiments remain optional.

Default-mode validation is retained in `default-events-proton-nv-01` and
`default-events-cube-amd-01`. Both verify visible output and keyboard/absolute
pointer delivery with event waits and the 50 ms exit-marker interval. The NVIDIA
Proton test deliberately omits `kms_wait` from its configuration to cover older
configuration files, and verifies NTSYNC activation as well. The explicit
polling fallback is exercised by `default-events-legacy-fallback-01`.

## External D-Bus viewer experiment

`tools/qemu-display-probe` is a separate host-only research frontend using
[qemu-display at revision 2592a231](https://gitlab.com/marcandre.lureau/qemu-display/-/tree/2592a2316e29e9187d29b4883570da2fb501b202).
It has its own locked Rust dependency graph and does not add dependencies to the
packaged launcher. See [its reproduction guide](../tools/qemu-display-probe/README.md).

The prototype uses the upstream Rust D-Bus bindings and a small SDL/EGL renderer.
It imports native single-plane DMA-BUFs, preserves modifiers, and draws into its
own host window without CPU frame copies. This includes a GPU presentation draw;
it is not physical direct scanout. Each update is acknowledged only after a real
GL fence covers the final source read. SDK patch 0025 and QEMU patch 0004 connect
that acknowledgement to strict guest buffer ownership through a private,
negotiated viewer contract. An ordinary D-Bus viewer does not imply this contract.
Only one external update is in flight. A failed import/read or disconnected
viewer cannot produce a successful reuse acknowledgement.

Each run owns a private D-Bus daemon, socket directory, viewer and QMP connection.
It does not publish its VM on the desktop session bus. Keyboard, pointer and
capture transitions use the owned VM only. The frontend sends ordinary input
through D-Bus; QMP is used only to select its Virtio tablet/mouse on capture
transitions or repair a driver-activation reorder. The selector explicitly
excludes the legacy PS/2 mouse: choosing the first relative device accepted
motion on the host but sent it to a device the guest was not consuming.

Qualification exposed and corrected three display lifecycle issues:

- QEMU must select the intended render node explicitly for D-Bus EGL startup.
- Releasing the old DMA-BUF must not disable a newly installed scanout. The
  listener now tracks the current buffer before deciding whether to disable it.
- D-Bus shader teardown must restore the render-node EGL context before GL
  deletion. The retained GDB trace proves the earlier shutdown assertion came
  from `qemu_gl_fini_shader` with no current context; QEMU patch 0005 fixes it.

The viewer also uses an actual peer ping for liveness. Reading a cached console
label had kept its window alive after QEMU exited. Initial failed runs
`cube-dbus-01` through `cube-dbus-03` and `cube-dbus-debug-01` are preserved.

`cube-dbus-04` passes native GPU rendering, real host-window capture, keyboard
and absolute input, and clean QEMU/viewer shutdown. Matching the virtual refresh
fallback to this monitor's approximately 240 Hz is essential: it completes
4,468 viewer GPU reads in ten seconds. Its median dispatch and draw/fence/swap
spans are 148.66 and 173.88 microseconds respectively. These CPU-observed spans
are not physical presentation or input-to-photon latency, and the completed
scanout count is not game FPS. The earlier implicit 60 Hz fallback substantially
limited the fixture, so it must not be mixed into a display comparison.

The stock GE-Proton11-11 D3D12/NTSYNC fixture also passes visible rendering and
input (`proton-dbus-01`). Relative-input qualification then exposed a shared
guest evdev gap: only absolute X/Y events were decoded. The decoder now detects
real relative-axis capability, accumulates X/Y through each synchronization
report, preserves motion/button ordering and discards partial movement on
`SYN_DROPPED`. The KMS compositor advertises relative input only when an actual
relative virtual device exists. `cube-relative-06` verifies two guest relative
events/four raw axis samples through the viewer in addition to keyboard and
absolute input. There are now 39 passing compositor tests and two host-viewer
tests, with Clippy warnings denied.

`read-failure-01` deliberately terminates the viewer before its tenth read
acknowledgement. This is an expected failed launch, not a normal passing run:
QEMU logs `external viewer read failed; source remains owned`, counts one host
error, and the runner removes its owned processes. Retaining the failed report
is part of the test evidence. It tests a disconnect, not a real injected GPU
hardware fault.

The final display comparison uses native, strict SDL, the existing implicit
Wayland broker and strict D-Bus, then reverses that order. It holds the game
settings, fullscreen geometry, refresh fallback, stock Proton, FSYNC, fresh
private caches and production memory policy fixed. It compares usable paths,
not transport overhead alone, because the broker has a different synchronization
path. The source/binary identities are preserved in
`build/nvkvm-pv-dbus/viewer-build.json` and the qualified viewer binary alongside it.
No display-default change follows merely from a passing fixture. Cursor-shape
forwarding, high-rate mice, VRR, hybrid display and physical latency remain
separate qualification work.

The final checks `proton-final-input-01` and `amd-final-input-01` pass with the
final shared decoder. The Proton test verifies stock D3D12, NTSYNC, two keyboard
events, two absolute events and two relative events through the D-Bus viewer.
The AMD regression verifies its existing native-context display and input path;
it does not qualify the new D-Bus viewer on AMD. The final forced-disconnect
test is `read-failure-final-01`. `qualification-report.json` indexes this evidence.

All eight UE5 trials completed successfully in
`build/nvkvm-pv-dbus/ue5-display-comparison-01/report.json`:

| Display path, UE5 D3D12 1920×1080 | Run 1 FPS | Run 2 FPS | Median FPS | Native percentage |
| --- | ---: | ---: | ---: | ---: |
| Native | 91.92 | 88.84 | 90.38 | 100.00% |
| Strict SDL | 81.49 | 82.87 | 82.18 | 90.93% |
| Direct Wayland broker, implicit sync | 84.91 | 84.77 | 84.84 | 93.87% |
| Strict D-Bus viewer | 82.99 | 83.32 | 83.16 | 92.01% |

The broker is fastest in this comparison. D-Bus is 1.19% above SDL and 1.99%
below the broker by median, but two samples and the native control's 3.35%
first-to-last drift do not establish a small D-Bus/SDL gain. The synchronization
paths also differ. These figures do not prove a transport-only overhead, a
universal backend ranking or a physical input-latency advantage. Display
defaults remain unchanged.

During the two D-Bus UE5 runs the viewer completes 9,234 and 9,187 GPU reads;
the host records 9,233 and 9,186 guest reuse acknowledgements with zero errors.
Median dispatch is approximately 147 microseconds, and median draw/fence/swap
is 757 and 750 microseconds (p95 approximately 1.24 ms). These remain internal
CPU-observed spans rather than input-to-screen measurements. The next latency
test should use an input-reactive visual fixture and equal instrumentation
across frontends before selecting a latency default.

## AMD without Venus

The selected AMD route is **AMDGPU DRM native context**, not Vulkan API
serialization. It runs RADV in the guest with `amdgpu-virtio=true`, using
virglrenderer's `amdgpu-experimental` DRM renderer and
`virtio-gpu-gl-pci,blob=on,drm_native_context=on,venus=off`.
[QEMU documents this upstream mode and its kernel/Mesa requirements](https://www.qemu.org/docs/master/system/devices/virtio/virtio-gpu.html).

The initial local smoke test `amd-native-04` passes Vulkan enumeration and the
owned rendering checksum on the integrated `1002:13c0` GPU. The guest reports
`DRIVER_ID_MESA_RADV`, not Venus. This first test uses the existing Mesa 25.0.7
native-context build and virglrenderer 1.3.0; it is not an AMD game or
performance qualification. `tools/build_amd_native_probe.py` builds a separate
Mesa 26.2.6 RADV driver and virglrenderer 1.3.0 with Venus disabled for ongoing
qualification.

The fresh pinned build also passes: `amd-native-26-01` verifies Vulkan rendering
with Mesa 26.2.6, and `amd-d3d12-01` runs the stock GE-Proton11-11 D3D12 fixture
through RADV native context. Its image checks, queue depths 1–3 and mixed
CPU/GPU cases pass with XCR0 = 7. These AMD tests remain offscreen. The host
rejects high/realtime-priority context probes without extra privileges; normal
priority rendering succeeds. No priority elevation is introduced.
`amd-d3d12-02` repeats the check using the driver build's own ICD manifest,
preserving its Vulkan 1.4 API declaration rather than imposing a separate cap;
all rendering checks pass again.

Mesa 26.2.6 requires newer libdrm AMDGPU headers than the Debian build image.
The builder verifies and extracts Legion's existing pinned libdrm 2.4.134
package into its private build prefix, without replacing the host or image
libraries. Component versions, checksums and build options are saved in
`build/amd-native/build.json`.

An inherited host `GBM_BACKEND=nvidia-drm` initially forced the wrong host GBM
plugin despite selecting the AMD render node. The probe now removes inherited
GBM/vendor overrides and explicitly selects Mesa/DRI for AMD. Guest dependencies
are collected in the portable build image, avoiding workstation AVX-512-only
libraries. The NVIDIA driver profile is not involved in AMD rendering.

The initial visible AMD cube completes KMS flips and receives injected keyboard
and absolute-pointer events, but its host window is black when the host UI is
forced to Mesa on this NVIDIA-driven display.
`amd-cube-window-04` exposed a research-validator false positive: QEMU's
post-client "Display output is not active" text is nonuniform. The probe now
captures only during active KMS presentation, rejects grayscale placeholder
text for these colored fixtures, and requires a real host-window image when a
window is requested. The earlier report's generic `passed` field must not be
used to claim working AMD display; its `desktop_capture_verified` was false.

Separating the host UI vendor from the guest rendering vendor, together with
the GLES/external-texture presentation path, fixes the visible output.
`amd-cube-window-06` selects NVIDIA for the host SDL/EGL window and RADV for the
guest AMD GPU. The real owned host window shows the rotating cube, hardware Vulkan
still identifies `1002:13c0` / `DRIVER_ID_MESA_RADV`, and both input-delivery checks
pass. This is AMDGPU native context with Venus disabled; NVIDIA is only the
host display/import driver in this AMD experiment. It does not establish that
every AMD-only desktop needs these display options. The research tool exposes
the independent selection through `--host-ui-vendor`.

`amd-proton-window-01` also passes with this configuration: stock GE-Proton11-11,
the owned D3D12 swapchain fixture, RADV on `1002:13c0`, XCR0=7, GPU image checks
and keyboard/absolute-pointer delivery. All 15 owned host-window captures are
distinct and show the moving colored fixture. This is presentation/correctness
qualification, not AMD game-FPS evidence. The cube run has ten distinct captures.
The controls remain useful: Mesa UI plus GLES/external (`amd-cube-window-05`)
and NVIDIA UI without GLES/external (`amd-cube-window-07`) both fail the visible
pixel check. Keep the tested combination together on this workstation.

## Controlled input response measurement

The next latency investigation now has a shared, independently observed D3D12
fixture, rather than relying on FPS as a latency proxy. The same executable runs
natively, through strict SDL, through the existing implicit Wayland broker, and
through the strict D-Bus viewer. A separate client injects Space through a
private Wayland virtual keyboard and identifies each resulting sequence barcode
in a completed compositor capture. Every response uses host CLOCK_MONOTONIC.
The private headless Sway output is NVIDIA GLES2, 1920×1080@240 Hz. See
[the measurement README](../tools/input-latency/README.md) for build/run commands,
validation, prerequisites and the exact measurement boundary.

`build/input-latency/comparison-01/report.json` contains the controls and two
reversed-order trials per mode, 40 responses per trial. All use stock
GE-Proton11-11-x86_64 and FSYNC. A deliberate 20 ms delay at the source increased
the median by **20.09 ms**; withholding input timed out with zero responses.
The GPU image oracles, original queue drain and exact Space event counts pass.

| Path | Responses | Median | 95th percentile |
| --- | ---: | ---: | ---: |
| Native | 80 | 4.80 ms | 6.52 ms |
| Strict SDL, original input dispatch | 80 | 26.17 ms | 40.97 ms |
| Implicit Wayland broker | 80 | 12.87 ms | 15.65 ms |
| Strict D-Bus viewer (one separate trial) | 40 | 14.26 ms | 16.94 ms |

SDL's normal GUI refresh services input on a 10/30 ms busy/idle cadence. New
QEMU research patch `0006-sdl-service-input-on-active-scanout.patch` optionally
services its existing event queue on active scanout work, before taking GL
state. A global dispatcher guard prevents expose/redraw recursion. It adds no
idle polling timer. The probe's `--sdl-frame-input` flag enables the environment
opt-in only for the owned SDL process; packaged defaults remain unchanged.

A matched four-trial sequence on the same patched QEMU, disabled/enabled/enabled/
disabled, confirms the improvement (`frame-input-comparison-01/report.json`):

| Strict SDL | Responses | Median | 95th percentile | Maximum |
| --- | ---: | ---: | ---: | ---: |
| Normal input dispatch | 80 | 25.66 ms | 41.75 ms | 44.86 ms |
| Frame-driven input | 80 | 13.99 ms | 17.57 ms | 20.73 ms |

That is a **45.5% median reduction** and **57.9% reduction at the 95th
percentile** in this test. Each run confirms all 40 keys and zero host read-fence
errors. Whole-VM host CPU use is similar: 1.218 versus 1.231 average cores over
boot, workload and shutdown; these counters do not isolate input processing or
steady state. Source rendering remains approximately 477 FPS in this deliberately
light fixture, independent of its 240 Hz captured output.

These are software dispatch-to-captured-response results, including approximately
4 ms of capture wait. Capture forces composition, and the headless compositor
is not the desktop/monitor. They do not establish physical input-to-photon
latency, direct scanout performance or latency in a GPU-bound game. Native and
VM CPU policies differ, and broker synchronization differs from strict SDL.
The remaining native/VM gap warrants tracing compositor/input/queue timing under
representative game load after this baseline; no monitor latency is inferred.

The ordinary animated D3D12 fixture also passes with frame-driven input,
including GPU image checks, original queue drain, keyboard/pointer delivery,
strict release and NTSYNC (`sdl-frame-normal-01`). A separate uninstrumented
UE5 regression smoke gives **74.04 FPS disabled / 78.35 FPS enabled**, both stock
Proton/FSYNC, D3D12 1920×1080 fullscreen, 16 vCPUs, 8 GiB, KMS 240 Hz, strict
SDL. Both report zero host fence errors. This is one pair, so the observed 5.8%
increase is not a confirmed FPS gain. The latency observer was absent; the
ordinary sparse presentation checks remained enabled. Results and reconstructed
frame-period distributions are in `build/input-latency/ue5-frame-input-smoke.json`.

The original resize regression exposed a research-compositor restriction:
1440×810 client buffers are rejected against its fixed 1920×1080 mode. Both
`sdl-frame-resize-01` (enabled) and `sdl-baseline-resize-01` (disabled) fail with
that same explicit size mismatch. The input optimization remains opt-in. The subsequent whole-image resize work
is described below; the failed runs remain as controls.

## Whole-image resize without a guest staging copy

The fixed-size failure is addressed on the NVIDIA research path by matching the
virtual primary-plane contract to the existing host viewer. New SDK patch
`0026-allow-whole-image-virtual-primary-scaling.patch` permits a whole framebuffer
to cover the whole virtual CRTC, with bounded scale ratios. The host viewer
already samples that full image into its window. Cropped source rectangles,
partial destination planes and out-of-buffer source rectangles are rejected;
this is not general overlay, panning or multiwindow composition support.

The compositor now imports the framebuffer at its actual dimensions and uses an
atomic primary-plane geometry update when its source size changes. Steady frames
keep the existing page-flip path. The old and new client buffers remain owned
through the actual flip and strict host GPU-read completion. No guest staging
surface or full-frame CPU copy is introduced. Absolute tablet coordinates follow
the current displayed source dimensions; raw relative motion remains unchanged.
AMDGPU native context retains its existing same-size scanout contract.

Repeated resizing also exposed a cache lifetime limit. Imported FBs previously
accumulated until a fatal 256-image cap. The cache now evicts unused imports at
32 entries while protecting both the current and queued FBs. Shared PRIME GEM
handles close only after their last cached FB is retired. Source-size diagnostic
history is bounded independently and cannot stop a game after many sizes.

The owned fixture adds bounded `resize_cycles` (1–48, default 1), preserving the
original two-transition test. Stress mode requests 96 transitions in 30 seconds.
Every actual resize verifies a full GPU image, and shutdown drains the original
queue. Diagnostic probe runs also perform a valid DRM TEST_ONLY request followed
by three invalid geometry requests; none of those invalid requests is applied.
Linux may reject invalid geometry with EINVAL, ERANGE or ENOSPC; the latter is a
DRM geometry validation result here, not a disk-space diagnosis. An initial test
incorrectly required EINVAL alone and was corrected; its failed report is kept.

```sh
python3 tools/probe_nvkvm.py --build build/nvkvm-pv-dbus \
  --proton /path/to/GE-Proton --fixture /path/to/current/workload.exe \
  --window --proton-window --fixture-resize --fixture-resize-cycles 48 \
  --input-check --ntsync --sdl-frame-input --sync-file-bridge \
  --kms-explicit-sync --cpus 16 --memory-mib 8192 --kms-hz 240 \
  --output build/resize-latency/stress-new
```

For automated qualification, use the private-session wrapper. It prevents real
desktop pointer events from overwriting the injected coordinate check and
captures only the single owned fullscreen output:

```sh
python3 tools/qualify_nvkvm_resize.py --build build/nvkvm-pv-dbus \
  --proton /path/to/GE-Proton --fixture /path/to/current/workload.exe \
  --output build/resize-latency/qualification-new
```

This runs normal resize, 96-transition stress, initial smaller-buffer startup
and D-Bus resize sequentially. It requires the same private Sway prerequisites
as the latency tool. The direct probe commands above use the desktop; their
coordinate checks can be disturbed by real desktop pointer events. One such
run (`resize-checked-02`) retained successful rendering/fence checks but correctly
failed the overall verdict because it could not validate the injected position.

Use a fresh output directory. Omit `--fixture-resize-cycles` for the ordinary
1920×1080 → 1440×810 → 1920×1080 check. `--fixture-start-scaled` replaces the
resize flags to start the fixture directly at 1440×810 against a 1920×1080
virtual mode. For D-Bus, replace `--sdl-frame-input` with `--host-display dbus`.
The resize validator requires GPU image checks at both sizes, presentation of
both sizes and correctly remapped absolute coordinates while smaller.

The isolated qualification in `build/resize-latency/qualification-report.json`
passes all four cases. The stress run completes **96 actual transitions**, **98
full GPU image checks** and **294 imports**, with **262 evictions** and **32
remaining cache entries**. The ordinary resize and D-Bus resize each complete
two transitions and validate the smaller-size pointer at approximately
(1080, 202.5) for normalized input (0.75, 0.25). Startup directly at 1440×810 also
passes. Every case rejects all three invalid geometries and reports zero host
read-fence errors. There are no steady-state compositor pixel readbacks.
These are correctness results, not a game compatibility or latency certification.
Independent captures show changing colored output: 15 distinct frames in each
ordinary/startup/D-Bus case, and 29 in the stress case. This freshness check is
separate from the fixture's full GPU image oracle; it is not a complete display
pixel oracle for every presented frame.

The AMD integrated-GPU same-size cube regression also passes with hardware RADV
rendering and keyboard/absolute-pointer delivery (`amd-regression-01`). A final
owned UE5 D3D12 fullscreen smoke passes at **83.93 FPS**, with zero host fence
errors (`ue5-regression-01`). That single run has no new matched native control,
so it demonstrates a working benchmark path, not an FPS improvement.

## Heavier shader load: measurement boundary

The latency tool now exposes `--fragment-iterations`; 16 remains the default.
The normal fixture remains capped at 8192, while explicit latency diagnostic
mode permits up to 65536. This higher diagnostic bound preserves the image
oracle and existing configuration bounds for ordinary runs.

A 512-iteration pilot still renders about 3286 FPS natively, versus about 4789
FPS in the original native light scene. It was insufficient to approximate a
GPU-heavy game frame rate. A subsequent 65536-iteration test runs near 70 source
FPS. Native, original SDL and frame-driven SDL each complete two trials / 80
responses, with SDL ordered off/on/on/off. All images, input counts, FSYNC
identities and original GPU queue drains pass. Results are retained in
`build/resize-latency/loaded-input-01/report.json`.

| Path | Source rendering FPS (median of runs) | Captured-response median | Capture wait median |
| --- | ---: | ---: | ---: |
| Native | 71.97 | 213.80 ms | 111.52 ms |
| Strict SDL, original input | 69.74 | 244.05 ms | 131.12 ms |
| Strict SDL, frame-driven input | 69.99 | 232.78 ms | 127.85 ms |

**This does not establish a heavy-load latency improvement.** Capture completion
itself now takes over 100 ms in many observations. The current endpoint includes
GPU/compositor/capture scheduling, and these trials cannot isolate game response
from the observation delay or its perturbation of the workload. Source FPS is
not displayed FPS. The long, dependent integer shader is also unlike an ordinary
game at the same average frame rate. Do not subtract the capture wait to invent
an input-to-photon estimate, or extrapolate the light-scene improvement to GPU
saturation. Keep frame-driven input opt-in.

The next measurement step should correlate frame identity with GPU completion
and presentation feedback, with captures used for correctness outside timed
intervals, and qualify physical input-to-photon separately. The heavy-load
results remain useful as a limit of this capture-based method.

```sh
python3 tools/measure_input_latency.py --mode sdl --sdl-frame-input \
  --fragment-iterations 65536 --proton /path/to/GE-Proton \
  --fixture /path/to/current/workload.exe \
  --output build/resize-latency/loaded-new
```

## Capture-free runtime frame feedback

The owned D3D12 fixture now supports opt-in `runtime_feedback` inside latency
mode, using stock Proton. It associates each Space sequence with the first
matching `GetLastPresentCount` frame ID and that draw's **original D3D12 queue
fence**. It emits a record only after `GetFrameStatistics` reports that ID or a
later one and the original fence has completed. One input is outstanding at a
time, so later frame IDs still carry the same sequence. This is the first
matched observation, not proof that the first responsive frame was displayed.

The `source_submitted_qpc` field is the CPU observation after Present and its
frame-ID query; it is not a GPU submission timestamp.

The host observer initially identifies barcode zero, then stops capturing. It
uses inotify to receive small, flushed fixture records and timestamps receipt
with host CLOCK_MONOTONIC. After the final response, it independently captures
and verifies the final barcode. It rejects stale IDs, missing/reordered input
sequences, invalid original-fence values, source-clock regressions, source
readbacks during timing, and any timed captures. Initial/final full GPU image
oracles and original queue drain remain required.

**The endpoint is runtime feedback, not host-window presentation.** Host dispatch
to record receipt includes source processing, polling, file flush, virtiofs and
host scheduling. Input-to-feedback and feedback-to-poll source durations use
only source QPC ticks and frequency. Host/guest absolute clocks are never
subtracted. DXGI statistics may be approximate; [DXVK's implementation warns of
this](https://github.com/doitsujin/dxvk/blob/master/src/dxgi/dxgi_swapchain.cpp).
The exact tested Proton binaries are recorded with the results; upstream source
is explanatory rather than a claim that GE contains that exact revision. These
records do not establish physical input-to-photon, nor that the host compositor
showed every matching frame. No pixel is read back during the timed phase.

```sh
python3 tools/measure_input_latency.py --mode sdl --runtime-feedback \
  --sdl-frame-input --fragment-iterations 65536 --proton /path/to/GE-Proton \
  --fixture /path/to/current/workload.exe \
  --output build/presentation-feedback/runtime-new
```

The controlled heavy-load comparison (`build/presentation-feedback/comparison-01`)
collects 80 responses per path with no timed captures. The source runtime portion
is stable at approximately 42 ms; the SDL input toggle has not established a
repeatable heavy-load improvement. Native/VM host-record receipt is not a common
host presentation endpoint, so do not interpret different totals as one path
presenting faster. The old captured-output metric and this runtime endpoint
also cannot be compared as a speedup.

A stock VKD3D research knob, `VKD3D_SWAPCHAIN_LATENCY_FRAMES`, controls its internal
swapchain queue. The probe and latency runner expose it as
`--swapchain-latency-frames 1|2|3`, with no override unless requested. They require
a startup log confirming the requested value actually activated. The setting is
scoped to the owned process/prefix. VKD3D's source explains that its default is
three because lower depths can cause false GPU idling and clock/throughput
regressions. This is a candidate to qualify per workload, not a new default.

The method preserves the original capture mode when `--runtime-feedback` is
absent. A legacy capture regression passes all 40 responses. The new method's
20 ms source-delay control shifts the host median by **19.79 ms**, and its
no-input control times out with zero responses. Neither control captures during
timing. The initial native/VM light-workload smokes both verify all 40 inputs,
frame IDs, original fences and final images.

The queue-depth experiment (`build/presentation-feedback/queue-depth-01`) uses
stock `VKD3D_SWAPCHAIN_LATENCY_FRAMES`, 65536 shader iterations and frame-driven
SDL input throughout. Each VM setting has 80 responses, ordered 3/2/2/3:

| Internal queue | Host record receipt median | 95th percentile | Source input → runtime feedback | Source rendering FPS |
| --- | ---: | ---: | ---: | ---: |
| Three frames | 64.12 ms | 74.37 ms | 41.90 ms | 70.46 |
| Two frames | 50.50 ms | 55.30 ms | 28.04 ms | 70.10 |

The host-record median falls **21.2%** and the source runtime interval loses
**13.86 ms**, approximately one GPU frame in this workload. Source throughput
changes about −0.5%. Source FPS and 1% lows include diagnostic images/reporting
hitches, so they are not a clean game throughput benchmark. The native control
(one run per setting) also shows the source interval falling from 41.43 to
27.68 ms. All responses have original-fence proof, zero timed captures and
independent start/end barcode checks. These remain runtime-feedback timings,
not host presentation or photons.

A separate owned UE5 D3D12 fullscreen smoke with no runtime-feedback instrument
reports **81.32 FPS at three frames / 81.81 FPS at two**. Both activate their
requested setting, use stock Proton/FSYNC and finish with zero host fence errors.
That is one pair, approximately +0.6%, and does not establish an FPS gain. The
ordinary sparse correctness captures remain enabled. Results are in
`build/presentation-feedback/ue5-queue-depth-01/report.json`.

**Keep the override opt-in.** This is a useful candidate for reducing queueing,
not evidence that every game should use two frames. Game-controlled latency
fences, CPU/GPU balance and clock behavior can change the throughput tradeoff.
No packaged Legion default or installed Proton file was changed.

```sh
python3 tools/measure_input_latency.py --mode sdl --runtime-feedback \
  --sdl-frame-input --swapchain-latency-frames 2 --fragment-iterations 65536 \
  --proton /path/to/GE-Proton --fixture /path/to/current/workload.exe \
  --output build/presentation-feedback/queue-two-new
```

### UE5 reverse-order throughput follow-up (2026-11-20)

The additional stock Proton/FSYNC UE5 pair reverses queue-depth order, retaining
the same 1920x1080 fullscreen settings, strict SDL path and frame-driven input.
Presentation telemetry is disabled throughout the FPS runs:

| Pair/order | Three-frame queue | Two-frame queue | Two vs three |
| --- | ---: | ---: | ---: |
| Original, 3 then 2 | 81.32 FPS | 81.81 FPS | +0.60% |
| Follow-up, 2 then 3 | 83.86 FPS | 84.02 FPS | +0.19% |

All four runs confirm the requested VKD3D setting and finish with zero host
synchronization errors. Two-run means are 82.59 versus 82.92 FPS, approximately
+0.4%. The small difference does not establish an FPS improvement. The
follow-up's frame-period-derived rates actually slightly favor three frames
(79.57 versus 79.44); the reported benchmark score averages instantaneous FPS.
The evidence supports no meaningful throughput loss in this workload while the
earlier reactive fixture demonstrated less queueing. It does not qualify
arbitrary games, CPU-bound/GPU-bound variants or a default queue-depth change.
Raw reports and snapshots: `build/host-presentation/ue5-queue-depth-reverse-01`.

An attempted resolution comparison was rejected: despite receiving
`-ResX=1280 -ResY=720 -ForceRes`, the benchmark reported 1920x1080. The assertion
stopped the series before a second resolution trial. Its 84.13 FPS score is
excluded from the queue-depth and resolution comparisons; no 720p or 1440p
performance claim is made. The rendering report passes its ordinary checks,
but `ue5-resolution-01/resolution-validation.json` correctly fails resolution
qualification. This preserves the failed method without misclassifying it.

## Host compositor presentation feedback (2026-11-20)

The optional D-Bus viewer now requests `wp_presentation` feedback on SDL's
borrowed Wayland surface immediately before each EGL swap. A separate event
queue collects presented/discarded callbacks. The diagnostic requires
CLOCK_MONOTONIC and rejects timestamps preceding the original GL read-completion
fence or following callback receipt. GPU-read acknowledgements still follow the
original GL fence; they do not wait for presentation. No pixels are mapped or
read back by this instrumentation.

`--viewer-presentation` produces an in-memory bounded trace, written after
rendering stops. The runner enables `WAYLAND_DEBUG=client` only for its owned
viewer. `tools/audit_host_presentation.py` independently requires each viewer
swap to contain exactly one feedback request, one non-null buffer attachment
and one commit on the owned surface, in that order. It rejects missing,
duplicate, intermediate and wrong-surface events. Wayland object IDs can be
reused; the association is scoped to each swap, not inferred from global ID
uniqueness. This audit is necessary because EGL controls the actual commits.

All presentation objects must resolve. Shutdown first drains callbacks, then
unmaps an unshown window if necessary and drains again. An unresolved tail is
reported as abandoned and fails the diagnostic; it is never silently counted
as presented. The initial development run (`cube-smoke-01`) failed with one
pending tail and remains preserved. The fixed private-display runs pass:

| Workload | Audited swaps | Presented | Discarded | Abandoned/errors |
| --- | ---: | ---: | ---: | ---: |
| Vulkan cube | 4,445 | 2,207 | 2,238 | 0 / 0 |
| Cube, artificial 20 ms post-GPU delay | 425 | 425 | 0 | 0 / 0 |
| Stock Proton D3D12, queue depth two | 7,049 | 3,474 | 3,575 | 0 / 0 |
| Physical desktop cube, final viewer | 4,438 | 2,223 | 2,215 | 0 / 0 |
| Final viewer, D3D12 with artificial 20 ms delay | 658 | 658 | 0 | 0 / 0 |

The post-GPU delay control reports **20.053 ms** median GPU-completion to
pre-swap interval, versus less than 0.001 ms without it. Feedback delivery is
separate: the delayed run's callback receipt lags its presentation timestamp
by approximately 19.64 ms median. Timestamping callback receipt alone would
therefore misattribute the viewer's blocked event pumping to display latency.
The headless D3D12 run reports 1.61 ms median / 3.39 ms p95 GPU-completion to
compositor presentation; the full trace includes startup/shutdown outliers of
approximately 201 ms. These are diagnostic scheduling observations, not game
input latency or steady-state performance claims.

These private Sway outputs use NVIDIA GLES2 at 1920x1080@240 Hz and return
presentation flags zero: there is no physical scanout. The final viewer also
passes on the physical Hyprland desktop (`physical-cube-02`): all 2,223 presented
updates report flags five (vsync plus hardware completion), with **4.51 ms
median / 4.91 ms p95** GPU-completion to compositor presentation and an 8.35 ms
maximum. Flags five do not include hardware-provided timestamps or zero-copy.
This is a compositor-reported host-stage observation, not optical validation.
The earlier desktop development run remains failed; it is not reclassified.
No headless result substitutes for monitor measurements. A compositor's
zero-copy flag, if observed later, would
describe its handling of the viewer's EGL backbuffer, not prove direct scanout
of the original guest image.

**Viewer update IDs are not source frame IDs.** Source dimensions and DMA-BUF
device/inode are recorded for diagnostics, but a reused buffer cannot identify
which Proton Present or input generation it contains. The baseline viewer cannot join the existing
runtime-feedback input records to these timestamps without explicit source
frame identity. The separate tagged-fixture diagnostic below supplies it. Never subtract the guest's QPC absolute
clock from the host's monotonic clock. This work qualifies the final host
presentation stage, not physical input-to-photon or a complete game latency
measurement.

Telemetry-off rendering/input passes. The intentional read-failure regression
still disconnects before a successful acknowledgement, and the guest reports
`external viewer read failed; source remains owned`. Two Rust viewer tests,
Clippy with warnings denied, formatting and five Python audit tests pass;
the latter cover malformed protocol associations, invalid clocks, abandoned
feedback and treating all-discarded updates as a failure. Results and exact
qualification scripts are in `build/host-presentation`.

The final initialization creates its registry through a Wayland proxy wrapper
already assigned to the private event queue. This prevents an EGL reader thread
from routing early registry replies onto SDL's default queue between object
creation and a later queue reassignment. The physical-desktop qualification and
final D3D12 delay control use this version.
The latter confirms a 20.054 ms median pre-swap control interval. The aggregate
verification covers **17,015 audited updates**, with all presented/discarded
outcomes accounted for and no unresolved callbacks or host read-fence errors.

```sh
python3 tools/probe_nvkvm.py --build build/nvkvm-pv-dbus \
  --host-display dbus --window --presentation cube --viewer-presentation \
  --sync-file-bridge --kms-explicit-sync --kms-hz 240 --input-check \
  --output build/host-presentation/cube-new
python3 -m unittest discover -s tools -p test_host_presentation.py -v
```

**Keep this diagnostic opt-in and exclude it from FPS comparisons.** Per-frame
Wayland debug logging adds CPU work. It is a tool for identifying presentation
queueing and discarded updates, not a performance optimization. Packaged
runtime, driver and Proton defaults remain unchanged.

## Exact source-frame to host-presentation qualification (2026-11-20)

The input fixture now has an opt-in `frame_tags` mode. The existing barcode
identifies input generation; a top strip identifies the exact rendered frame.
The source counter starts at one and fails before 16-bit wrap. After each draw,
the fixture retains its tag, input generation, returned DXGI Present count and
original queue fence in memory. The complete mapping is written only after
rendering and original queue drain. Initial/final full GPU image oracles cover
both barcode strips and the shader workload. Stock Proton, NVIDIA driver code,
QEMU transport and production defaults are unchanged.

The optional viewer tag pass reads the same imported image as its window draw,
checks three samples per barcode bit and decodes two words into a 2x1 RGBA8
target. An eight-byte pixel-pack transfer is recorded **before the existing
real GL completion fence**. CPU mapping happens after that fence, and the usual
strict source-buffer acknowledgement still follows completion of all source
reads. Disabled rendering allocates no tag resources or extra GPU work. This
is an instrumented eight-byte readback per update, not a readback-free method.

`tools/link_frame_presentation.py` matches each decoded frame to its source
Present/fence record, then follows its viewer update through the independent
Wayland feedback/attach/commit audit. It selects the first host-presented
response for each input; discarded updates cannot count. It rejects unknown,
stale or wrong-generation tags, missing presented responses, frames predating
the source target/fence, unidentified presented images during response
observation, and responses after the next input. The final correctness capture
is delayed 250 ms; every timed presentation must precede its capture request.
Runtime feedback still paces inputs, but its file receipt is a separate metric
and is not the new presentation endpoint. Host CLOCK_MONOTONIC supplies both
latency endpoints; guest/source QPC absolute timestamps are never subtracted.

**300 input responses across 53,598 audited updates pass the identity join.**
The no-input control times out with zero responses, and the older completed
capture method passes all 20 regression inputs. A separate host-dispatch
calibration waits approximately 20.05 ms before actual injection: the control
start to presented median rises by **20.02 ms**, while actual injection to
presentation remains 13.25 ms, matching the 13.29 ms undelayed repeat. Both
clocks are recorded, so the deliberate wait does not overwrite the ordinary
input-dispatch timestamp.

A game-thread `--delay-ms 20` pause gives a different result: 13.29 to 24.34 ms
host median. This is consistent with draining existing queued work while the
thread sleeps, but queue drain was not directly measured. This source pause is
retained as a pipeline experiment, not accepted as a linear +20 ms calibration.

The heavy shader uses 65,536 iterations at 1920x1080 on NVIDIA. The queue-depth
order is 3/2/2/3, with 80 independently linked responses per setting:

| Queue depth | Dispatch → first presented response median | p95 | Diagnostic source FPS |
| --- | ---: | ---: | ---: |
| Three | 93.10 ms | 109.45 ms | 69.72 |
| Two | 80.21 ms | 110.40 ms | 69.69 |

Two frames reduce the median by **13.8% / 12.89 ms**, approximately one source
frame, without a meaningful source-throughput change. **The tail does not
improve.** The responding frame reaches the viewer at 66.08 versus 52.38 ms
median after injection. CPU-observed viewer draw/fence spans are approximately
12.9 ms; observed GPU completion to host presentation is approximately 14.2 ms.
The directly computed viewer-start to presentation interval is about **27.1 ms
median**, rising to 41.2 ms p95 for the two-frame cohort. Stage medians must not
be added to infer a median total, and these CPU spans are not hardware GPU
timestamps. Host rendering and composition under GPU contention are concrete
targets for the next scheduling experiment; these observations alone do not
establish their precise internal cause.

The cost controls render the same source tags and enable the same Wayland
audit logging while disabling only viewer extraction. Heavy-source means are
70.16 FPS off versus 69.69 on (approximately **−0.68%**); the lightweight control
is 472.31 versus 469.50 (approximately **−0.60%**, one off/on pair). These are
diagnostic source rates including correctness/readback hitches, not clean game
FPS benchmarks. Mapping/decoding after the fence takes around 1–2 microseconds
median, but that excludes the additional GPU work and is not its total cost.
Do not subtract this cost to invent uninstrumented latency results.

All joined runs retain original source fences, initial/final image checks,
zero timed desktop captures, zero host synchronization errors and fully resolved
presentation callbacks. The headless Sway output reports presentation flags
zero; no physical scanout or optical input-to-photon claim follows. Exact frame
identity is qualified for this fixture, not automatically for arbitrary games.
Queue-depth and packaged defaults remain unchanged.

```sh
python3 tools/measure_input_latency.py --mode dbus --runtime-feedback \
  --host-presentation --swapchain-latency-frames 2 --fragment-iterations 65536 \
  --proton /path/to/GE-Proton --fixture /path/to/current/workload.exe \
  --observer /path/to/current/observer --output build/frame-identity/heavy-new
python3 -m unittest discover -s tools -p 'test_*presentation.py' -v
```

`host-latency.json` holds each original run's join; the final replay with added
stage breakdowns is `host-latency-reanalysis.json`. Raw input records, source
mapping, protocol traces, controlled comparison and `verification.json` are in
`build/frame-identity`. Validation passes 24 fixture library tests, two Rust
viewer tests, nine Python presentation/identity tests and three observer-validity
tests, plus the C parser self-test, Windows fixture Clippy, viewer Clippy and
format checks. Host lib/test Clippy uses explicit `serde/std,serde_json/std`
features for bootstrap path serialization; the Windows fixture retains its
ordinary allocation-only Serde build.

## GPU scheduling and earlier window submission (2026-11-20)

The EGL priority experiment does not produce a repeatable latency improvement.
The private compositor already receives high priority; SDL's original viewer
context receives medium. A process-local EGL proxy now requests and verifies
priority at original context creation, preserving every other context
attribute. Silent driver downgrades fail qualification. Its private compositor
hook can observe or independently request medium/high. It never changes global
driver settings, privilege levels or another compositor process.

The preliminary second-context approach changed latency even in the medium
control, so it was stopped after its first four trials. Those source snapshots,
binaries and reports remain under `build/gpu-priority`, excluded from priority
conclusions. The corrected comparison uses the same original SDL context and
proxy binary in all eight runs, with reversed-order pairs and 80 tagged input
responses per combination:

| Viewer priority | Compositor priority | Median response | p95 | Diagnostic source FPS |
| --- | --- | ---: | ---: | ---: |
| Medium | Medium | 79.38 ms | 106.19 ms | 70.48 |
| Medium | High | 79.27 ms | 111.15 ms | 70.46 |
| High | Medium | 84.80 ms | 106.48 ms | 70.31 |
| High | High | 79.45 ms | 116.45 ms | 70.53 |

All 320 responses pass exact frame joins and synchronization checks. Driver
priority queries confirm each requested assignment, but there is no useful
measured win here. An extra 40-response GPU timer profile brackets only the
host viewer's draw, tag extraction and eight-byte transfer. The GPU interval
is **44.03 microseconds median / 48.18 microseconds p95**, compared with a
**12.76 ms CPU-observed draw-to-fence interval** in that same run. The GPU bracket
can include dependencies/preemption inside it; it is not exclusive shader time.
It also excludes the game's heavy shader. This locates most of the CPU interval
outside the short measured bracket, without distinguishing queueing, driver
waits and CPU wakeup delay. No absolute GPU/host clocks are subtracted.

That result motivated a submission-order change: enqueue the window with EGL
**before** CPU fence waiting, allowing the compositor to prepare its work while
the GPU completes the copy. `--viewer-early-swap` retains the original real GL
source-read fence, now waited after swap. The D-Bus reply is still sent only
after successful completion and tag retrieval. Swap/commit/presentation cannot
substitute for a successful read fence.

**Later physical-desktop measurements found this ordering slower on Hyprland.**
The headless results in this section are retained as method-specific evidence,
not a recommendation. The physical qualification below takes precedence for
the tested desktop; the original order remains the default.

The opt-in mode requires Wayland and presentation diagnostics. Its independent
trace audit now verifies the explicit acquire/release points on every owned
buffer commit, including surface/timeline identity and non-reused acquire
points. EGL remains the owner of the surface's synchronization objects; the
viewer does not bind conflicting objects. The compositor must wait on EGL's
acquire point before sampling the window buffer. This uses the existing
driver's standard explicit-sync path, not a new driver or QEMU patch.

CPU completion observation occurs after swap in this mode. The audit records
and enforces that order explicitly, retaining the old order for baseline
records. Presentation can precede CPU fence observation; its signed offset
has a separate name and is not interpreted as GPU-ready latency. Original
frame identity, input/presentation timestamps and source-fence checks remain
mandatory. The negative after-swap injection confirms no successful read reply
is sent: QEMU reports `external viewer read failed; source remains owned`.

The matched 65,536-iteration, 1920x1080, two-frame queue comparison runs baseline /
early / early / baseline on the same binary, with 80 responses per mode:

| Submission order | Median dispatch → presented | p95 | Diagnostic source FPS |
| --- | ---: | ---: | ---: |
| CPU read-fence wait before window submission | 78.62 ms | 106.39 ms | 70.60 |
| Window submission before CPU read-fence wait | **62.67 ms** | **77.54 ms** | 70.80 |

That is **20.3% lower median / 27.1% lower p95**, across **7,076 audited updates**.
The directly computed viewer-start to presentation median falls from **27.07
to 12.83 ms**. EGL swap itself takes approximately 0.05 ms in the new path;
the CPU then waits approximately 12.51 ms on its real read fence. Window
preparation is already queued during that interval. Source throughput changes
about +0.28%, not an established FPS gain. Every run retains initial/final
images, zero timed desktop captures, original fences, zero host synchronization
errors and fully resolved presentation feedback.

The separate owned UE5 D3D12 physical-desktop smoke completes at **80.40 FPS
baseline / 82.42 FPS early** with zero host synchronization errors. Both use
stock Proton/FSYNC, the two-frame setting, presentation protocol logging and
matched sparse correctness captures; tag extraction is disabled. This is one
pair, approximately +2.5%, and does not establish a repeatable FPS improvement.
It verifies rendering and throughput behavior on a game workload and real
desktop, not the physical input latency reduction.

**The latency comparison remains instrumented headless-compositor feedback.**
It is not optical input-to-photon or a native-versus-VM latency comparison.
GPU priority, GPU timer queries and early submission stay opt-in; packaged
runtime and queue-depth defaults are unchanged. The early mode currently
requires per-commit trace verification, so broader driver/compositor testing
and production capability handling remain before enabling it generally.

Results, traces, source maps and safety/throughput checks are under
`build/gpu-priority` and `build/early-swap`. The final viewer passes two Rust
tests and Clippy with warnings denied; eleven Python presentation/identity
tests include early-order clock and explicit-sync rejection cases. Three
observer-validity tests remain passing. A CPU-only fake EGL provider validates
local/global symbol lookup, get-proc dispatch, preserved attributes, rejected
priority downgrades and the private compositor guard.

```sh
python3 tools/measure_input_latency.py --mode dbus --runtime-feedback \
  --host-presentation --viewer-early-swap --swapchain-latency-frames 2 \
  --fragment-iterations 65536 --proton /path/to/GE-Proton \
  --fixture /path/to/current/workload.exe --observer /path/to/current/observer \
  --output build/early-swap/heavy-new
```

## Physical-desktop latency qualification (2026-11-20)

The headless early-swap result needed a desktop check before promotion.
`tools/measure_desktop_latency.py` runs the same tagged D3D12 fixture fullscreen
on the existing Hyprland desktop, NVIDIA 620.70.06, DP-2 1920x1080 at approximately
240 Hz. It calls the owned VM's private D-Bus keyboard interface instead of
injecting desktop keys. The timestamp precedes the `gdbus` helper invocation,
so helper startup and transport are included. Physical devices and host SDL
input routing are bypassed.

The endpoint is the first compositor-presented image containing the exact GPU
frame tag for that input. Every selected response must pass the existing
source-frame/present-count/original-fence join and independent commit audit,
and carry both vsync and hardware-completion flags (5 here). This is a real
desktop compositor's completion report, **not optical input-to-photon**. The
host clock supplies both endpoints; guest QPC is used only for source-local
validation. Initial/final owned-window images independently verify the input
barcode. No desktop capture or source image readback overlaps timed responses.
Tag extraction and per-commit Wayland audit logging remain enabled.

### Early swap did not improve this desktop

The matched baseline / early / early / baseline block has 40 inputs per run,
80 per mode, the same viewer binary and a two-frame VKD3D queue:

| Viewer order | Pooled median | p95 | Individual run medians |
| --- | ---: | ---: | ---: |
| Original CPU fence wait before swap | **56.70 ms** | **66.36 ms** | 55.74 / 57.42 ms |
| Early swap before CPU fence wait | 64.76 ms | 70.43 ms | 65.94 / 63.54 ms |

Early swap is **14.2% slower at the median** in this block. Input-to-viewer-draw
medians are essentially equal (39.71/39.73 ms), while viewer-start to reported
presentation increases from 16.58 to 26.82 ms. This locates the difference in
the viewer/compositor portion without establishing its internal cause. The
previous headless 20% reduction is not a physical gaming-latency prediction.

Keep the original submission order. The experimental early mode remains
available for controlled investigation with its strict fence and explicit-sync
audit. An unbuilt attempt to promote it into a logging-free runtime path was
withdrawn; its source snapshot is preserved under
`build/desktop-latency/abandoned-runtime-prototype`, outside active source.
The normal renderer already works without presentation logging. No runtime
promotion, driver patch or packaged default change follows from this result.

### One-frame queue: lower fixture latency, a throughput tradeoff

A separate matched 1 / 2 / 2 / 1 block retains the original viewer order and
65,536 shader iterations, with 80 inputs per setting:

| Stock VKD3D maximum latency | Pooled median | p95 | Individual run medians | Diagnostic source FPS |
| --- | ---: | ---: | ---: | ---: |
| One frame | **41.37 ms** | **49.19 ms** | 39.89 / 44.51 ms | 68.83 |
| Two frames | 67.03 ms | 83.28 ms | 59.61 / 73.35 ms | 70.87 |

One frame lowers the median by 38.3% in this block, at about 2.9% lower
diagnostic source throughput. Both repeats agree in direction, but session
variation is substantial. Compare within each matched block; do not select
the best baseline from another experiment or treat the pooled numbers as
universal latency. Fixture FPS includes diagnostics and is not the game FPS
tradeoff. The separate UE5 comparison below measures that cost with tag
extraction and presentation logging disabled.

All 320 responses in the two matched blocks pass exact frame identity,
physical feedback flags, original source fences, start/end image checks and
zero host synchronization errors. The 20-input early pilot and 40-input
one-frame pilot are preserved separately and excluded from these aggregates.
The new CPU-only tests reject corrupt/mistagged captures and headless/partial
hardware feedback. Research results and replayable validation are in
`build/desktop-latency/summary.json` and `build/desktop-latency/summarize.py`.

### UE5 confirms the cost of a one-frame queue

The throughput comparison uses the original D-Bus viewer order, stock
GE-Proton11-11/FSYNC, 1920x1080 fullscreen, and strict source-read completion.
Frame tags, GPU timer queries and presentation logging are disabled. Ordinary
matched sparse correctness captures remain. The first pair showed a large
difference, triggering the reversed follow-up, for an overall 2 / 1 / 1 / 2
sequence:

| Stock VKD3D maximum latency | First score | Reverse-pair score | Mean score | Frame-period-derived FPS, mean |
| --- | ---: | ---: | ---: | ---: |
| Two frames | 84.00 FPS | 83.79 FPS | **83.90 FPS** | 79.53 |
| One frame | 61.68 FPS | 62.08 FPS | 61.88 FPS | 59.57 |

The one-frame queue loses **26.2%** of the benchmark's reported FPS. Both
repeats agree closely. Frame-period-derived FPS also falls, by approximately
25.1%; those periods are reconstructed from the application's rounded FPS
log, not GPU timestamps. The benchmark score averages instantaneous FPS.
Every run confirms the requested stock VKD3D setting activated, completes its
rendering checks and reports zero host synchronization errors.

**Do not make one frame the default.** The fixture's approximately 3% diagnostic
throughput cost understated the game's cost substantially. Queue depth needs
workload-specific qualification. The earlier three-versus-two UE5 comparison
remains separate evidence; no new three-frame result or native comparison is
implied here. Keep the original viewer order and both experimental switches
opt-in. Raw game reports are under `build/desktop-latency/ue5-depth-01`.

```sh
python3 tools/measure_desktop_latency.py --samples 40 \
  --swapchain-latency-frames 2 --fragment-iterations 65536 \
  --proton /path/to/GE-Proton --fixture /path/to/current/workload.exe \
  --output build/desktop-latency/physical-new
```

The tool currently depends on the existing NVIDIA research build,
`build/venus/capture-toplevel`, and compatible owned-toplevel capture in `grim`.
It is a developer qualification tool, not a portable end-user diagnostic.

## UE5 frame-handoff profiling (2026-11-20)

The heavy synthetic viewer interval was not a measurement of the actual UE5
handoff. A new bounded, opt-in stage profiler separates host CPU state setup,
draw submission, fence creation/flush, the original GPU-fence wait and window
swap. It stores at most 32,768 records and writes them only after rendering.
Every row must have ordered host `CLOCK_MONOTONIC` observations and a successful
original fence; frame counts must match the viewer's qualified completions.
Missing/overflowed records, timeout-as-success or reordered clocks fail
qualification. CPU-only tests cover those rejection cases and stage partitioning.

This diagnostic works without Wayland presentation logging or GPU tag readback.
Optional GPU queries retain their own duration bracket, never joined to host
absolute clocks. Its results can include GPU dependencies/preemption; it is not
exclusive shader execution time. Source ownership remains tied to the original
read fence. Early swap stays disabled.

The first owned UE5 diagnostic (`build/frame-handoff/ue5-stage-01`) qualifies
**9,168 completed viewer frames**, zero host synchronization errors and zero
CPU frame copies/tag readback. It uses the stock two-frame VKD3D setting,
1920x1080 fullscreen and FSYNC:

| Measurement | Median | p95 |
| --- | ---: | ---: |
| CPU-observed viewer draw/fence/swap interval | **0.752 ms** | 1.237 ms |
| CPU GL fence wait | **0.646 ms** | 1.126 ms |
| CPU draw-call submission | 0.0095 ms | 0.0124 ms |
| CPU flush call | 0.0009 ms | 0.0012 ms |
| CPU window swap call | 0.0537 ms | 0.0706 ms |
| GPU duration bracket | **16.384 microseconds** | 21.504 microseconds |

The earlier approximately 13 ms heavy-fixture wait does not characterize UE5.
The actual game diagnostic spends most of its viewer interval in the CPU fence
wait, but this does not establish that the interval is removable. Earlier GPU
work, context scheduling and CPU wakeup delay are not separated by this bracket.
The instrumented benchmark score is excluded from throughput comparisons.

The clean comparison runner now applies and verifies the same requested stock
VKD3D latency natively and in the VM. Native here means the Windows benchmark
running through the same stock Proton on the Linux host. Both use FSYNC and
fresh private prefixes/shader caches. `--application-final-capture` preserves
the VM's completed score and CSV before capturing its owned window, with an
explicit bounded capture acknowledgement before stopping Proton. This removes
screenshots from the scored interval while keeping independent display checks.

The initial capture-handshake attempt used `seq`, absent in the minimal guest.
Its VM correctly failed qualification; the entire attempted pair in
`build/frame-handoff/ue5-clean-01` is excluded. The repaired loop uses shell
builtins, and the diagnostic passes its final capture acknowledgement. Failed
evidence is preserved alongside the corrected runs.

### Refreshed clean throughput baseline

`build/frame-handoff/ue5-clean-02` runs native / VM / VM / native with matching
two-frame VKD3D latency and FSYNC. All four runs pass; both VM runs use the
same viewer binary with stage profiling, GPU queries, tags, presentation
logging and early swap disabled. File ordering verifies that VM image captures
follow preservation of the completed score/CSV and precede shutdown acknowledgement.

| Path | First score | Reverse-pair score | Mean score | Frame-period-derived FPS, mean | Per-run estimated p99 frame time |
| --- | ---: | ---: | ---: | ---: | ---: |
| Host Linux, stock Proton | 93.61 | 93.78 | **93.70 FPS** | 91.67 | 15.21–15.31 ms |
| NVIDIA KVM, strict D-Bus viewer | 83.29 | 83.55 | **83.42 FPS** | 79.10 | 27.86–27.93 ms |

The reported score is **89.0% of native**, an 11.0% reduction in this matched
block. The frame-period-derived rate is approximately 86.3% of native, and the
estimated frame-time tails deserve attention alongside average FPS. Those
periods are reconstructed from the application's rounded instantaneous-FPS
CSV; they are not GPU timestamps. Each row's p99 range gives the two run
percentiles, not a pooled percentile. The instrumented viewer profile's
approximately 3 ms maximum interval cannot by itself explain these approximately
28 ms frame-time tails.

This updates the baseline for this setting and session. It is not an isolated
code-change comparison with the earlier 8.5% result: queue/capture configuration
and session differ. GPU clocks remain unlocked, the host CPU policy is
unrestricted while the guest retains Legion's XCR0/AVX-512 restrictions, and
only this game benchmark/GPU/driver/compositor is covered. No optical or
native-versus-VM input-latency claim follows.

### Shorter fence-wait timeout did not help

The new process-local `--viewer-fence-wait-us` control changes only the timeout
of each blocking GL wait. It retains the original successful fence requirement
and overall two-second failure bound. A timeout never releases source ownership.
Both diagnostic controls use the identical v2 viewer, stock two-frame queue,
FSYNC, GPU timing and stage profiling; presentation logging and tag readback
remain disabled:

| Per-call timeout | Completed frames | Median CPU fence wait | p95 wait | Mean wait calls/frame | Viewer process CPU / wall time |
| --- | ---: | ---: | ---: | ---: | ---: |
| Default 1000 microseconds | 9,342 | 0.662 ms | 1.129 ms | 1.17 | 11.35 / 149.97 seconds |
| 100 microseconds | 9,261 | 0.651 ms | 1.141 ms | 6.99 | 11.24 / 147.81 seconds |

The GPU bracket is 16.384 microseconds median in both. Median CPU wait changes
only about 11 microseconds, with slightly worse p95 and approximately six times
as many wait calls. Viewer CPU cost per completed frame is essentially equal
(1.215/1.214 ms), including its driver threads and idle/setup interval but
excluding profile serialization. This does not show a useful win; there is
only one instrumented pair and no repeatability/FPS improvement claim.

Keep the default 1000 microseconds. These instrumented scores are excluded
from throughput comparisons; no clean FPS experiment is justified by this
screening result. The data does not support blaming a fixed one-millisecond
per-call timeout for most of the observed fence wait. It does not identify
the underlying GPU scheduling/dependency or CPU wakeup contribution.

### Upstream submission/completion trace

The separate `build/frame-handoff/queue-trace-01` pair enables the stock VKD3D
timeline, with the same two-frame setting and FSYNC. Both writers use private
tmpfs during execution; the native helper now preserves its trace after the
writer stops, matching the guest's existing storage policy. These instrumented
scores are excluded from FPS comparisons. Raw traces retain 6.79/6.12 million
events; summaries select CPU spans after each trace-relative 20-second cutoff:

| CPU-observed span | Native median / p95 | VM median / p95 |
| --- | ---: | ---: |
| Completion `CALLBACK` | 0.304 / 0.600 ms | 0.403 / 0.946 ms |
| Submission overhead | 4.1 / 55.1 microseconds | 9.3 / 77.3 microseconds |
| Submit region | 0.152 / 1.813 ms | 0.123 / 2.152 ms |

The earlier multi-millisecond completion-callback result does not characterize
this corrected strict-sync configuration. The new per-call overhead difference
is a lead, not a critical-path attribution: submissions overlap across threads,
trace instrumentation perturbs execution, and those medians cannot be summed
to infer an FPS loss. These are CPU spans, not GPU execution timestamps.

### Pending-acquire eventfd wake prototype

Code inspection found that source acquire watches already have pollable
eventfds, but the KMS loop's blocking wait includes only Wayland, its listener
and the DRM card. A fence becoming ready can therefore wait for another event
or the existing 1 ms fallback timer. `--kms-acquire-wake` optionally adds the
live acquire eventfds to that wait set. It borrows the descriptors without
reading/consuming readiness. The existing acquire check, Wayland blocker
processing and original host read fence still govern buffer ownership. The
fallback timer remains for input and shutdown servicing.

The default path retains its stack-based poll array. Only the opt-in path
with pending acquires collects the extra descriptors. It requires explicit
sync and KMS event waits. Runtime verification checks the selected mode, and
benchmark qualification requires actual acquire wakeups. The compositor passes
41 tests and Clippy, including eventfd wakeups without display/DRM activity,
preserved event readiness, bounded idle waits and disconnected-FD rejection.
The GPU/input smoke verifies **3,602 acquire wakeups**, correct images, keyboard
and relative pointer input, and zero host synchronization errors.

The clean UE5 comparison (`build/frame-handoff/acquire-ue5-01`) uses original /
wake / wake / original, identical viewer/compositor binaries in all four runs,
and screenshots only after preserving the score. GPU queries, tags, stage
profiling, presentation logging and early swap are disabled. The GL fence wait
remains at its default 1000 microseconds:

| KMS wait set | First score | Reverse-pair score | Mean score | Frame-period-derived FPS, mean | Compositor CPU, mean |
| --- | ---: | ---: | ---: | ---: | ---: |
| Original | 84.61 | 83.88 | **84.25 FPS** | 79.99 | 2.629 seconds |
| Include acquire eventfds | 83.54 | 84.06 | 83.80 FPS | 79.48 | 2.652 seconds |

There is **no useful repeatable FPS improvement**: the mean score changes
about −0.53%, the two matched pair directions disagree, and frame-time tails
and CPU cost are similar. The wake-enabled runs record 18,361 actual wakeups
with zero synchronization errors. This is an exercised mechanism, not a
performance win. Keep it opt-in and retain the original default wait set.
Neither the old 13 ms synthetic wait nor the code-level missing wake source
justifies promoting an optimization without measured desktop/game benefit.

The physical-desktop original / wake / wake / original follow-up retains the
tagged source-frame join and independent commit audit. It verifies all 160
inputs, 7,075 viewer updates, original source fences, initial/final images,
zero timed captures and physical vsync/hardware-completion flags (5):

| Wait set | Individual run medians | Pooled median | p95 | Mean response |
| --- | ---: | ---: | ---: | ---: |
| Original | 57.62 / 73.85 ms | 66.54 ms | 83.61 ms | 66.29 ms |
| Include acquire eventfds | 74.51 / 55.27 ms | 59.06 ms | 84.22 ms | 63.90 ms |

**Do not call the lower pooled median a repeatable latency win.** The paired
directions reverse and session medians span approximately 55–75 ms; p95 does
not improve. Both modes produce one lower- and one higher-latency session.
This is owned-console software dispatch to compositor-reported physical
presentation, including helper transport, GPU tags and audit logging. It does
not measure physical input devices, SDL input routing or photons.

The forced viewer-read failure with acquire wakeups enabled stops after nine
successful GPU reads. QEMU reports `external viewer read failed; source remains
owned` and one expected host error; the launch correctly fails. Acquire
wakeups do not bypass the independent consumer-read ownership contract.

The phase adds profiling and reproducible controls, **not a default FPS
optimization**. Original submission order, queue overrides and wait-set
defaults remain. No new NVIDIA/QEMU patch is required. Replayable validation,
source/binary identities, raw traces and the verified recovery checkpoint are
under `build/frame-handoff`; see `summary.json` and `create-checkpoint.py`.

## GPU cache reuse and benchmark instrumentation (2026-11-20)

The next study separates empty GPU cache directories from seeds produced by
one complete benchmark pass. Every run retains a fresh Proton prefix, the same
stock Proton, FSYNC, two-frame VKD3D latency, 1920x1080 fullscreen settings and
original viewer order. Acquire-wake, GPU queries, frame tags and presentation
logging remain disabled. VM screenshots follow preservation of the completed
score and frame log. This phase changes Python research tools only.

`--preserve-shader-cache` exports VKD3D/NVIDIA cache data after application and
compositor writers stop. `--shader-cache-seed` copies a private seed into guest
RAM before starting Proton. Native and VM seeds remain separate, immutable and
identified by file sizes/SHA-256. Fontconfig, Proton-fix logs and prefix state
are excluded. The native path uses its private host cache directory; the guest
uses RAM and exports afterward. Within each path, storage policy is constant.
This compares practical paths, not isolated transport overhead.

Warmed study labels require successful VKD3D read-only mapping and no stale-cache
discard. A mapping alone does not prove every pipeline hit; opaque NVIDIA cache
entry hits are not counted. Rejected/unverified seeds remain advisory for
ordinary launching: metadata records them while hardware/rendering validation
continues. The benchmark study independently rejects an unverified warmed label.
The new regression test protects against stale caches becoming launch failures.

The initial `comparison-01` VM export used `cp -a`, whose attempt to preserve
guest ownership IDs fails through the mapped virtiofs share. The launch correctly
failed qualification despite correct rendering/zero GPU errors. The entire
attempted pair is excluded and preserved. Content-only `cp -R` and GPU-only
selection fix the export without changing guest/host ownership policies.

### Cold versus warmed GPU caches

`build/shader-cache-study/comparison-02` runs native-cold / VM-cold /
native-warm / VM-warm / VM-warm / native-warm / VM-cold / native-cold. Each path
therefore receives cold/warm/warm/cold ordering, with two scores per state:

| Path/cache state | Individual scores | Mean score | Mean run p99 | Mean run p99 after estimated 10 seconds |
| --- | ---: | ---: | ---: | ---: |
| Native cold | 93.64 / 89.54 | **91.59 FPS** | 15.49 ms | 15.29 ms |
| Native warmed | 94.83 / 91.28 | **93.06 FPS** | 15.46 ms | 14.37 ms |
| Legion cold | 83.53 / 82.39 | **82.96 FPS** | 29.57 ms | 23.41 ms |
| Legion warmed | 83.84 / 84.94 | **84.39 FPS** | 28.80 ms | 23.24 ms |

Warming improves the mean application score approximately **1.6% natively /
1.7% in Legion**. The Legion/native score ratio is nearly unchanged:
**90.58% cold / 90.69% warmed**. Native run variation is retained, not excluded.
GPU clocks remain unlocked, and two samples per state do not qualify other
systems or workloads. These session/configuration results are separate from
the earlier 89.0% baseline.

Both warmed VM runs map the seeded 235,904-byte VKD3D read-only cache and produce
only **488 bytes** of new write-cache data, versus 235,904 bytes when cold.
The seed is exercised, yet the persistent frame-time tail remains. The first
cold VM has 131 estimated frames over 25 ms: 61 in the first estimated ten
seconds and 70 later. Its first warmed counterpart has 122, including 69 later.
Cache reuse helps modestly; this evidence does not explain most of the gap as
missing shader-cache persistence.

`tools/analyze_ue5_frames.py` groups slow frames into estimated elapsed windows
and reports the subset after ten estimated seconds. CSV `Time` is a frame index.
Elapsed time and periods are reconstructed from rounded application FPS; they
are not GPU/host timestamps or optical latency. Whole boundary-crossing frames
are assigned to their estimated start window. Per-run p99 values are averaged
in the table rather than pooled across different frame counts.

### Remove guest-only diagnostics from FPS measurements

The research probe enabled surface diagnostics for application benchmarks:
bounded software-UI PPM dumps, per-commit cadence tracking and periodic JSON
writes over virtiofs. Native runs have no corresponding instrumentation.
`--surface-diagnostics off` disables this guest diagnostic path while retaining
the rendering result, strict GPU-read ownership and final owned-window check.
The source compositor binary is unchanged.

The separate warm-seeded on/off/off/on block uses identical binaries and the
same immutable VM seed throughout (`diagnostic-comparison-01`):

| Surface diagnostics | Scores | Mean score | Mean run p99 | Mean run p99 after estimated 10 seconds | Compositor CPU per scanout |
| --- | ---: | ---: | ---: | ---: | ---: |
| Enabled | 82.29 / 83.62 | 82.96 FPS | 30.01 ms | 22.92 ms | 0.284 ms |
| Disabled | 83.88 / 84.32 | **84.10 FPS** | 29.19 ms | 22.99 ms | 0.271 ms |

Disabling diagnostics improves the mean score approximately **1.38%** in this
block and lowers measured compositor CPU per scanout approximately **4.72%**.
Both score pair directions agree, but p99 directions differ and steady-state
p99 remains about 23 ms. This removes measurement overhead; it is not evidence
of a new production-renderer optimization or a large pacing fix. No updated
native percentage is inferred from this separate four-VM block.

The paired FPS runner now defaults to diagnostics disabled and owned-window
capture after score/CSV preservation. It records explicit timed instrumentation;
legacy `--application-periodic-captures` and `--surface-diagnostics on` remain
available for diagnostics. Probe/smoke defaults retain surface diagnostics,
and disabling them is refused for input/resize qualification. GPU-cache exports
are opt-in for the paired runner. Packaged runtime, driver/QEMU patches, queue
depth and submission-order defaults remain unchanged.

All eight cache runs and four diagnostic controls pass hardware/rendering,
cache/capture-order qualification and zero host synchronization errors. Thirty
Python tests pass, including immutable seeds, GPU-only export, positive cache
evidence, advisory cache rejection and frame-index/elapsed-time separation.
Rust/driver code is unchanged and was not rebuilt in this phase. Raw reports,
immutable seeds, replay scripts, source identities and the recovery checkpoint
are under `build/shader-cache-study`.

```sh
python3 tools/benchmark_nvkvm_ue5.py --build build/nvkvm-pv-dbus \
  --application-dir /path/to/owned/UE5/benchmark --proton /path/to/GE-Proton \
  --host-display dbus --window-fullscreen --kms-hz 240 --fsync \
  --sync-file-bridge --kms-explicit-sync --swapchain-latency-frames 2 \
  --preserve-shader-cache --pairs 2 --output build/cache-baseline-new
```

The locally archived eight-run controller accepts a fresh output path:
`python3 build/shader-cache-study/run-comparison.py build/cache-study-new`.
Its defaults assume this workstation's NVIDIA research setup and owned assets.

## Reproduction

These tools currently assume the existing Legion build image, built kernel and
`build/runtime`, plus NVIDIA at `/dev/dri/renderD128` for the comparison. Host
development packages for the QEMU SDL/EGL build are required. This is a
developer experiment, not an end-user installer. Build logs and source/driver
identities are recorded in the output directory.

```sh
python3 tools/build_nvkvm_probe.py --jobs 4
python3 tools/probe_nvkvm.py --presentation cube --window \
  --output build/nvkvm-pv-display/cube-new
python3 tools/probe_nvkvm.py --proton-window --window \
  --output build/nvkvm-pv-display/d3d12-window-new \
  --proton /path/to/GE-Proton11-11 \
  --fixture build/venus/amd-qualification/d3d12/fixture.exe
python3 tools/build_amd_native_probe.py --jobs 4
python3 tools/probe_nvkvm.py --backend amdgpu-native \
  --amd-mesa build/amd-native/stage/usr/lib/x86_64-linux-gnu/libvulkan_radeon.so \
  --amd-renderer build/amd-native/renderer-prefix/lib/libvirglrenderer.so.1 \
  --output build/nvkvm-pv-display/amd-native-new
python3 tools/probe_nvkvm.py --backend amdgpu-native --proton-window --window \
  --input-check --sdl-gl es --scanout-external --host-ui-vendor nvidia \
  --proton /path/to/GE-Proton11-11 \
  --fixture build/venus/amd-qualification/d3d12/fixture.exe \
  --output build/nvkvm-pv-display/amd-window-new
```

Use a fresh result directory each time. The Proton mode is for the repository's
owned D3D fixtures, or an explicitly supplied owned benchmark application.
It creates a guest-local prefix and uses a protocol-only Wayland server for
offscreen checks, or the direct KMS compositor for `--proton-window`.

For the current display/sparse patches and UE5 comparison:

```sh
python3 tools/build_nvkvm_probe.py --output build/nvkvm-pv-perf --jobs 4
python3 tools/probe_nvkvm.py --build build/nvkvm-pv-perf \
  --extra-probe build/nvkvm-pv-perf/fixtures/sparse-probe \
  --output build/nvkvm-pv-perf/sparse-new
python3 tools/benchmark_nvkvm_ue5.py --build build/nvkvm-pv-perf \
  --application-dir /path/to/owned/UE5-benchmark \
  --proton /path/to/GE-Proton11-11 --pairs 2 \
  --output build/nvkvm-pv-perf/ue5-pairs-new
```

The owned application must contain `Benchmark.exe` and write
`BenchmarkResults.txt`. Only a copied application tree is writable; the
original application and Proton installation are preserved. `--input-check`
injects an A key press/release and pointer movement through only the owned
VM's QMP socket and verifies compositor delivery counters. It does not certify
game-specific mouse behavior or host shortcut routing.

## What would justify a Legion backend

1. Qualify matching DXVK/VKD3D workloads against native and Venus, using stock
   Proton and the same binaries, resolution, scene and cache conditions.
2. Extend visible-pixel and swapchain qualification to resize/fullscreen,
   relative mouse, cursor and host shortcuts, after the initial 620 display fix.
3. Repeat the UE5 benchmark and qualify actual games, recording frame times and
   CPU use, including hybrid NVIDIA-render/AMD-display configurations.
4. Test driver upgrades, repeated starts/stops, allocation pressure and process
   cleanup. Automate matching library collection and version/layout checks.
   Automatically collect and compare NVIDIA's published structure layouts and
   NVKMS command names in CI, with explicit qualification before admitting a new
   display ABI. An unsupported driver should report the unsupported profile
   rather than guessing a private ABI. The current research builder measures layouts but
   does not automatically certify a new driver.
5. Review the isolation and GPU memory accounting before integrating the host
   forwarder into normal game launches.

The user has accepted driver maintenance as a performance tradeoff. The target
architecture is nvkvm-pv for NVIDIA and AMDGPU native context for AMD. Venus
remains the packaged runtime during development, rather than the intended AMD
solution. Driver profiles, regression fixtures and artifact identities should
make ongoing maintenance reproducible. Both paths need game frame-time and
compatibility qualification before the default runtime changes.
