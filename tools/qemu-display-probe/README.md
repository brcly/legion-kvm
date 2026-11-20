# D-Bus display research viewer

This is an opt-in host display experiment for the `research/nvkvm-pv` branch.
It is not included in the packaged Legion runtime. It uses Marc-André Lureau's
`qemu-display` library at commit
`2592a2316e29e9187d29b4883570da2fb501b202`, with a separate Cargo lockfile.

The library handles QEMU's local D-Bus display and input interfaces. A small
SDL/EGL host renderer imports single-plane DMA-BUFs with their native modifier.
Normal rendering never reads or copies frame pixels through the CPU. The host GPU draws into
the viewer's window, so this is **not a physical direct-scanout path**.

An update reply is sent only after a real GL fence confirms the last source
read completed. A private, negotiated `org.legion.Display1.Listener.ReadCompletion`
contract connects that reply to nvkvm's strict guest buffer ownership. It is a
local research extension, not a guarantee provided by arbitrary D-Bus viewers.
Renderer failure disconnects instead of returning through the upstream void
handler API, which would otherwise send a successful reply. Only one update is
allowed in flight; channels, image caches and timing samples are bounded.

Each run starts its own D-Bus daemon inside a private temporary directory.
It does not publish a VM on the user's desktop session bus. Per-frame display
updates use QEMU's peer-to-peer listener connection. Input targets only the
owned console. The owned QMP socket is used for switching between the VM's
absolute tablet and relative mouse; it is not used for each mouse movement.

## Build

The existing Legion kernel/runtime, rootless `localhost/legion-build:1` image,
NVIDIA research build dependencies, Rust, `dbus-daemon`, SDL2, EGL, epoxy,
Wayland client development files and `wayland-scanner`
development packages are required. Run from the repository root:

```sh
cargo build --manifest-path tools/qemu-display-probe/Cargo.toml --locked --release
python3 tools/build_nvkvm_probe.py --output build/nvkvm-pv-dbus --dbus-display --jobs 4
```

The QEMU builder stages the build image's Python `gdbus-codegen` privately when
building this variant. It does not install host packages or replace drivers.
Its metadata records the build image, patches and binary checksums. Preserve
the `--dbus-display` flag when rebuilding that output directory.

## Qualification

Use new output directories; the runner deliberately refuses to overwrite runs.
Replace 240 with the test monitor's refresh rate and use the same setting for
comparisons. The renderer currently assumes the selected NVIDIA GPU is able
to import its native buffers on the host.

```sh
python3 tools/probe_nvkvm.py --build build/nvkvm-pv-dbus \
  --host-display dbus --window --presentation cube \
  --sync-file-bridge --kms-explicit-sync --kms-hz 240 --input-check \
  --output build/nvkvm-pv-dbus/cube-new

python3 tools/probe_nvkvm.py --build build/nvkvm-pv-dbus \
  --host-display dbus --window --proton-window \
  --proton /path/to/GE-Proton11-11-x86_64 \
  --fixture build/venus/amd-qualification/d3d12/fixture.exe \
  --sync-file-bridge --kms-explicit-sync --kms-hz 240 --ntsync --input-check \
  --output build/nvkvm-pv-dbus/proton-new

cargo test --manifest-path tools/qemu-display-probe/Cargo.toml --locked
cargo clippy --manifest-path tools/qemu-display-probe/Cargo.toml \
  --locked --all-targets -- -D warnings
```

`--input-check` sends owned synthetic keyboard, absolute-motion and
relative-motion events through the viewer's D-Bus input path. This differs from
the other research displays' QMP injection, which bypasses their frontend.
The guest compositor's diagnostics must confirm delivery; requesting injection
alone does not constitute a pass. `Ctrl+Alt+G` toggles the viewer's SDL input
grab. Focus loss releases tracked keys and restores absolute pointing.

The negative test below is **expected to fail**. It disconnects before sending
a successful GPU-read acknowledgement. Its console log must contain
`external viewer read failed; source remains owned`, and the runner must clean
up its owned processes. A passing ordinary rendering report is not expected.

```sh
python3 tools/probe_nvkvm.py --build build/nvkvm-pv-dbus \
  --host-display dbus --window --presentation cube \
  --sync-file-bridge --kms-explicit-sync --kms-hz 240 \
  --viewer-fail-read-after 10 --output build/nvkvm-pv-dbus/failure-new
```

## Performance

```sh
python3 tools/benchmark_nvkvm_ue5.py --build build/nvkvm-pv-dbus \
  --application-dir /path/to/owned/UE5-benchmark \
  --proton /path/to/GE-Proton11-11-x86_64 --fsync \
  --compare-displays --pairs 2 --window-fullscreen --kms-hz 240 \
  --output build/nvkvm-pv-dbus/ue5-displays-new
```

This runs native, strict SDL, direct Wayland broker and strict D-Bus, then
reverses the order. Each run has fresh private caches and identical game
settings. SDL and D-Bus use explicit guest synchronization; the broker uses
its existing implicit path. It compares usable paths, not transport cost in
isolation. Do not compile or run other GPU workloads during the comparison.

`viewer-result.json` records imports, real GPU-read completions, errors and
CPU-observed dispatch/draw/fence/swap spans. These spans are not presentation
timestamps. Physical latency still needs a reactive visual workload and
preferably an external measurement. High-polling
rate mice, VRR, cursor-shape forwarding, resize/remode, hybrid-GPU display and
general game compatibility are not certified by these fixtures. The viewer
does not currently forward guest cursor-shape updates.

The reactive D3D12 fixture now supports capture and capture-free runtime-feedback
methods; see `tools/input-latency/README.md`. Runtime feedback does not establish
that a matching frame was presented by the host compositor.

## Host presentation diagnostic

On a Wayland host, add `--viewer-presentation` to the D-Bus probe command. The
viewer borrows SDL's native surface, requests `wp_presentation` feedback before
each swap and records compositor presentation timestamps on a private event
queue. It requires CLOCK_MONOTONIC. No pixels are mapped or read back. This
diagnostic does not delay GPU-read acknowledgements until presentation: buffer
ownership still follows the original real GL completion fence.

`host-presentation.jsonl` contains a viewer update token, source dimensions,
reused DMA-BUF storage identity, GPU completion, swap and feedback timestamps.
These tokens are **not Proton source-frame IDs**. The runner enables
`WAYLAND_DEBUG=client` for the owned viewer and audits every swap against one
feedback request, one buffer attachment and one commit on the owned surface.
It rejects ambiguous commits, abandoned feedback and invalid clock ordering.
Presented and discarded updates are counted separately. A bounded shutdown
drain unmaps the window if necessary to resolve an unshown tail as discarded.
An unresolved tail still fails the diagnostic.

`--viewer-presentation-delay-ms 20` adds an artificial post-GPU delay as a
positive control. Check its measured GPU-completion to pre-swap interval;
callback delivery can itself be delayed by the viewer's event pumping.
Records are buffered and written only after rendering stops. The protocol
audit logs add overhead, so **exclude these runs from FPS comparisons**.

```sh
python3 tools/probe_nvkvm.py --build build/nvkvm-pv-dbus \
  --host-display dbus --window --presentation cube --viewer-presentation \
  --sync-file-bridge --kms-explicit-sync --kms-hz 240 --input-check \
  --output build/host-presentation/cube-new
python3 -m unittest discover -s tools -p test_host_presentation.py -v
```

Headless Sway feedback verifies scheduling and association, not monitor scanout;
the qualified headless runs report presentation flags zero. Hardware/zero-copy
flags, when present, describe the compositor's presentation of the viewer's
EGL buffer, not end-to-end direct scanout of the original guest buffer. The base timestamp diagnostic needs separately verified source identity; the
tagged-fixture method below supplies that mapping. No physical input-to-photon claim follows from this method.

## Exact fixture-frame identity diagnostic

`tools/measure_input_latency.py --mode dbus --runtime-feedback
--host-presentation` enables a separate instrumented path. The owned D3D12
fixture renders its input generation as the existing central barcode and a
unique 16-bit source frame ID in its top strip. Frame IDs start at one and
fail on exhaustion rather than wrapping. The fixture retains an in-memory
mapping from each ID to input generation, DXGI Present count and original
queue fence; it writes the mapping after rendering and original queue drain.
Unchanged stock Proton and driver code carry these tags as image content.

An additional GL pass samples the same imported source texture as the window
draw, decodes both barcodes into a 2x1 RGBA8 target and reads **eight bytes per
update** through a pixel-pack buffer. The original GL fence covers both draws
and the tag transfer; CPU mapping follows that fence. Source ownership is not
released early. The normal renderer allocates no tag resources or extra GPU
operations. This is a diagnostic readback, not an uninstrumented or completely
readback-free timing method.

The tag is retained with its viewer update and audited Wayland commit through
presented/discarded feedback. `tools/link_frame_presentation.py` checks every
decoded ID against the source mapping and selects the first host-presented
response per input, excluding discarded frames. It rejects stale IDs, missing
responses, wrong generations, frames predating the original target/fence,
unidentified presented images during response observation and responses after
the next input. The observer delays its final correctness capture by 250 ms;
every timed presentation must precede that capture request.

`host-latency.json` is the resulting host dispatch to compositor-presentation
metric. The separate runtime-feedback metric remains available and paces
inputs; its file receipt is not used as the presentation endpoint. All joined
latency timestamps use host CLOCK_MONOTONIC. No source/host absolute-clock
subtraction occurs. This instruments the owned fixture, not arbitrary games;
private headless compositor feedback does not measure monitor scanout. GPU
tag work, eight-byte readback and protocol logging remain in the measured path
and must be excluded from clean FPS comparisons.

```sh
python3 tools/measure_input_latency.py --mode dbus --runtime-feedback \
  --host-presentation --swapchain-latency-frames 2 --fragment-iterations 65536 \
  --proton /path/to/GE-Proton --fixture /path/to/current/workload.exe \
  --observer /path/to/current/observer --output build/frame-identity/heavy-new
python3 -m unittest discover -s tools -p test_frame_presentation.py -v
```

For an extraction-cost control, use `--source-frame-tags --viewer-presentation`
in place of `--host-presentation`. The fixture still renders the same tags and
the viewer still logs audited presentation, but no GPU tag pass/readback runs.
That control produces runtime-feedback timing only, not joined host latency.

Failed development runs are retained. See `docs/nvkvm-research.md` for the
qualification history, measurements and decision about defaults.

## GPU scheduling research

`--viewer-gpu-priority medium|high` in the probe/latency runners requests an
explicit EGL priority on the viewer's **original SDL context**. Build the
optional process-local proxy first:

```sh
mkdir -p build/gpu-priority
cc -O2 -fPIC -shared -Wall -Wextra -Werror \
  tools/input-latency/egl_priority.c \
  -o build/gpu-priority/liblegion_egl_priority.so -lEGL -ldl -pthread
```

The runner sets `SDL_VIDEO_EGL_DRIVER` and `LEGION_VIEWER_GPU_PRIORITY` only for
its viewer subprocess. The proxy forwards EGL operations through the system
libEGL, preserving context attributes and replacing/adding only the priority.
It also handles `eglGetProcAddress` creation dispatch. SDL retains context,
window and surface ownership. The default flag loads no viewer proxy and
leaves SDL creation unchanged. No driver installation or global settings change.

[EGL_IMG_context_priority](https://registry.khronos.org/EGL/extensions/IMG/EGL_IMG_context_priority.txt)
defines a hint which implementations may downgrade. The proxy queries the
assigned priority and rejects a downgrade; the viewer checks the active
context again at completion. The probe requires the proxy's original-context
marker and records its binary hash. A granted hint does not prove that the GPU
will preempt other work sooner; that requires measured results. SDL's
[EGL loader](https://github.com/libsdl-org/SDL/blob/SDL2/src/video/SDL_egl.c)
supports selecting the process-local EGL driver used here.

The latency runner also accepts `--compositor-gpu-priority observe|medium|high`.
Its `LD_PRELOAD` applies only to the owned private headless Sway process, and
the proxy refuses compositor operation outside that guarded runtime directory.
`observe` logs the driver's actual assignment without modifying attributes.
Viewer and compositor priorities can therefore be varied independently.
Requests and actual assignments are retained in the reports.

`--viewer-gpu-timing` additionally brackets window drawing and the optional
tag pass/transfer with GL timestamp queries. Results are retrieved only after
the original real completion fence, and unavailable or regressing values fail
the diagnostic. `gpu_elapsed_ns` is a **GPU-clock duration**, not a host-clock
timestamp. It can include dependencies/preemption within the bracket; it is
not exclusive shader execution time. No absolute GPU/host clocks are joined.
The flag requires presentation diagnostics and stays disabled by default.

```sh
python3 tools/measure_input_latency.py --mode dbus --runtime-feedback \
  --host-presentation --viewer-gpu-priority high \
  --compositor-gpu-priority observe --swapchain-latency-frames 2 \
  --fragment-iterations 65536 --proton /path/to/GE-Proton \
  --fixture /path/to/current/workload.exe --observer /path/to/current/observer \
  --output build/gpu-priority/high-new
```

The preliminary approach that created a second EGL context changed latency
even in the medium-priority control. Its source/binaries and results remain
under `build/gpu-priority`, but it is excluded from priority conclusions. The
current implementation modifies SDL's initial creation rather than creating
an extra context.

## Submit the window before CPU fence waiting

**Physical-desktop follow-up: keep the original submission order.** The later
Hyprland comparison measured 56.70 ms median with the original order versus
64.76 ms with early swap (80 tagged responses each, reversed order, hardware
completion/vsync feedback). The headless gain below did not carry over to this
desktop. Early swap remains an experimental diagnostic, not a recommended
gaming optimization. See the physical-desktop section in the research notes.

The opt-in `--viewer-early-swap` runner flag changes the viewer's ordering to
draw/tag/transfer → record and flush the original GL fence → EGL swap → wait
for that same fence → read tags → acknowledge the source update. The compositor
can prepare the queued window while the CPU waits. A successful swap is never
used as source-read completion; a real successful GL fence remains mandatory.

This research mode requires Wayland and `--viewer-presentation`. Its audit
requires each owned surface commit to carry explicit acquire and release
points on imported DRM syncobj timelines. Acquire points cannot be absent,
duplicated, late, unknown or reused on a live timeline. EGL manages these
objects; the viewer does not create a second synchronization object on SDL's
surface. The [syncobj protocol](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/blob/main/staging/linux-drm-syncobj/linux-drm-syncobj-v1.xml)
requires the compositor to wait for the acquire point before sampling.

The CPU may observe its completed read fence after presentation. Early-swap
records therefore enforce draw → swap begin → swap return → CPU fence
observation → callback receipt, with presentation after swap begin and before
receipt. The normal mode retains its stronger original CPU ordering. The
signed observation-to-presentation offset is named separately and is not
called GPU-ready latency. Exact frame identity and the host dispatch/presented
endpoints remain unchanged.

```sh
python3 tools/measure_input_latency.py --mode dbus --runtime-feedback \
  --host-presentation --viewer-early-swap --swapchain-latency-frames 2 \
  --fragment-iterations 65536 --proton /path/to/GE-Proton \
  --fixture /path/to/current/workload.exe --observer /path/to/current/observer \
  --output build/early-swap/heavy-new
```

With this mode, `--viewer-fail-read-after 10` injects failure after an actual
swap and before read completion. The viewer must disconnect without a
successful acknowledgement, and QEMU must report that the source remains
owned. This negative is expected to fail rendering qualification.

The reversed-order tagged comparison reduces headless median response from
78.62 to 62.67 ms and p95 from 106.39 to 77.54 ms, with diagnostic source rates
70.60/70.80 FPS. This is instrumented headless compositor timing, not monitor
input-to-photon. The owned UE5 smoke reports 80.40/82.42 FPS; one pair with
protocol logging and matched sparse correctness captures does not establish
an FPS gain. All variants retain zero host synchronization errors. Keep the
mode opt-in. Subsequent physical-desktop testing found a regression, as noted
above; neither the headless numbers nor this single FPS pair justify promotion.

## CPU stage profiling without presentation logging

`tools/probe_nvkvm.py --host-display dbus --viewer-stage-profile` records
bounded CPU `CLOCK_MONOTONIC` observations around state setup, draw submission,
tag work, fence creation, flush, the original fence wait, and window swap.
It preserves the original submission order and source-read acknowledgement
contract. There is no per-frame file write: at most 32,768 records are held in
memory and serialized after rendering. Overflow fails the diagnostic. The
normal renderer performs no profile allocations or profile clock calls.

The profile requires no `--viewer-presentation` or `WAYLAND_DEBUG` logging and
does not enable tag readback. Optional `--viewer-gpu-timing` adds a GPU duration
bracket whose results are retrieved only after the original fence completes.
The CPU timestamps and GPU duration are distinct measurements; absolute GPU
and host clocks are never subtracted. The bracket may include dependencies or
preemption and does not measure exclusive shader execution.

`viewer-stages-summary.json` is validated against the viewer's completed frame
count. A row with incomplete/reordered clocks, missing frames, or a timed-out
or failed final fence cannot qualify. A long CPU fence wait does not establish
that an equivalent delay is removable: earlier game GPU work, GPU scheduling
and CPU wakeup delays can all contribute. Swap return is not presentation.
Exclude these diagnostic runs from FPS conclusions.

```sh
python3 tools/analyze_viewer_stages.py \
  build/frame-handoff/ue5-stage-01/viewer-stages.jsonl \
  --viewer-report build/frame-handoff/ue5-stage-01/viewer-result.json
python3 -m unittest discover -s tools -p test_viewer_stages.py -v
```

For clean UE5 throughput comparisons, the benchmark runner now accepts
`--swapchain-latency-frames 1|2|3` and verifies activation natively and in the VM.
`--application-final-capture` preserves the completed score/CSV before the VM
captures its owned window. The guest waits for capture acknowledgement with a
bounded shell-builtins loop before stopping Proton. This keeps image capture
outside the scored interval while retaining independent VM display checks.
Neither option changes installed Proton or packaged runtime defaults.

`--viewer-fence-wait-us 50|100|250|1000` changes the timeout passed to each
blocking GL fence wait, scoped to this viewer process. The default remains
1000 microseconds. The overall two-second failure deadline and successful
original-fence requirement remain: a timeout never acknowledges a source read.
Shorter waits can increase CPU use and need separate qualification; they do
not guarantee earlier GPU completion. Viewer reports include process CPU
seconds and wall time for the render-loop interval, including driver threads
but excluding profile serialization. This control is an experiment, not a
recommended game setting.

## Guest acquire-event wake experiment

`tools/probe_nvkvm.py --kms-acquire-wake` adds pending source acquire eventfds
to the guest KMS event-loop wait set. It requires explicit synchronization and
KMS event mode. Eventfds are borrowed and left readable for their fence owner;
the source acquire check and real host read-fence completion remain mandatory.
Runtime counters verify that the experiment activated and actually woke on
acquire events. The default remains off.

The clean UE5 original/wake/wake/original comparison scores 84.25/83.80 FPS
by two-run means, with similar frame-time tails and CPU use. The physical
tagged-response comparison varies substantially between sessions and does not
establish a repeatable latency win. This remains an experiment, not a recommended
optimization. The code-level missing wake source alone did not predict a
measured game benefit. The GPU/input smoke and forced-read-failure negative
retain correct fence ownership. See the frame-handoff section of the research
notes and `build/frame-handoff/summary.json` for all runs and their limits.

## Cache reuse and quieter benchmark defaults

The paired UE5 runner now defaults to `--surface-diagnostics off` and
`--application-final-capture`. This avoids periodic guest surface JSON/software
UI dumps and captures the owned VM window after saving the completed score/CSV.
Use `--surface-diagnostics on` or `--application-periodic-captures` explicitly
for instrumented investigation; reports list timed instrumentation. The probe
retains its diagnostic default for smoke/input checks. These are research
measurement defaults; the packaged runtime is unchanged.

`--preserve-shader-cache` exports GPU-only cache files after writers stop.
`tools/probe_nvkvm.py --shader-cache-seed /path/to/seed` copies a private seed
into guest RAM before Proton starts. Seeds exclude fonts, Proton-fix logs and
prefix state. Each has a content manifest; input seeds are not mounted writable.
Cache rejection is advisory for launching and recorded in metadata. The cold/
warmed study requires positive VKD3D mapping with no stale-cache discard before
assigning a warmed measurement label. NVIDIA per-entry hit counts are opaque.

The eight-run cache study observes approximately 1.6% native / 1.7% VM score
improvement, leaving the native ratio around 90.7%. Warmed VM p99 still averages
28.8 ms (23.2 ms after an estimated 10 seconds). A separate on/off/off/on control
observes approximately 1.4% higher score without surface diagnostics, while
post-startup p99 stays around 23 ms. These are small controlled observations on
one setup, not a general performance guarantee. See
`build/shader-cache-study/summary.json`, `diagnostic-summary.json` and the
corresponding section of the research notes.
