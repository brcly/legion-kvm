# Controlled input response measurement

This developer test runs the same input-reactive D3D12 executable natively and
through the NVIDIA research backend. A separate Wayland client sends Space and
recognizes the resulting sequence barcode in completed compositor captures.
It measures **software dispatch to captured response**, including capture delay.
It does not measure physical mouse/keyboard latency, monitor scanout or photons.

Each run owns a private headless Sway session, one NVIDIA GLES2 output at
1920×1080@240 Hz, a fresh Proton prefix and its own observer. Input stays inside
that private session. No desktop focus change or global input injection is
needed. The observer refuses an ordinary desktop runtime directory.

`tools/measure_desktop_latency.py` is a separate physical-desktop experiment.
It injects Space into the owned VM's private D-Bus keyboard interface; it sends
no global desktop key events. It measures host command dispatch through
compositor-reported presentation of an exactly tagged response, including
`gdbus` helper startup and transport. It bypasses physical input devices and
the viewer's SDL input routing, so it is not an input-to-photon measurement.

```sh
python3 tools/measure_desktop_latency.py --samples 40 \
  --swapchain-latency-frames 2 --fragment-iterations 65536 \
  --proton /path/to/GE-Proton --fixture /path/to/current/workload.exe \
  --output build/desktop-latency/physical-new
```

This currently requires the NVIDIA research build, the existing
`build/venus/capture-toplevel` helper, `grim` with owned-toplevel capture, and a
compatible Wayland desktop. It opens an owned fullscreen window and captures
its initial/final barcodes only. Source readbacks during timed inputs are
rejected. Every selected response must retain the original source fence,
exact GPU frame tag, independently audited commit, and presentation flags
containing both vsync and hardware completion. Absolute host and guest clocks
are never subtracted. GPU tag extraction and protocol audit logging remain
part of this diagnostic's measured path.

The Hyprland follow-up found early swap slower despite its headless gain.
Reducing the queue to one frame lowered fixture latency but substantially
reduced UE5 FPS. Keep both switches opt-in; see the research notes and
`build/desktop-latency` for matched comparisons and their session variation.

## Build

Use the existing Legion runtime/kernel and the NVIDIA research build described
in [the research notes](../../docs/nvkvm-research.md). This tool currently assumes
NVIDIA on `/dev/dri/renderD128`; it is not an AMD latency qualification tool.
Host development requirements include a C compiler, pkg-config, Wayland client
and xkbcommon headers, wayland-scanner, and a Sway build supporting headless
outputs, screencopy and virtual keyboards. Wine/Proton and the Windows fixture
build requirements are the same as `tools/build_proton_workload.py`.

```sh
python3 tools/build_nvkvm_probe.py --output build/nvkvm-pv-dbus --dbus-display --jobs 4
python3 tools/build_input_latency.py --proton /path/to/GE-Proton
```

`--sway /path/to/sway` uses a supplied compositor; `swaymsg` must be alongside
it. An installed Sway is used when available. On this CachyOS workstation, when
Sway is absent, the builder asks pacman for its package URL and SHA-256 in
read-only print mode, verifies the download, and extracts it into the build
directory. It never installs a host package. Other distributions should supply
`--sway`. Host compositor binaries/libraries are never added to the guest.
`build/input-latency/build.json` records binary and protocol identities.

## Run

Finish compilation first. Run GPU measurements sequentially and use a new
output directory for every invocation. The comparison runner executes baseline,
+20 ms source-delay and no-input controls before two trials per display mode in
reversed order. Native, SDL and broker each collect 80 responses total.

```sh
python3 tools/benchmark_input_latency.py --proton /path/to/GE-Proton \
  --output build/input-latency/comparison-new

python3 tools/measure_input_latency.py --mode sdl --proton /path/to/GE-Proton \
  --output build/input-latency/sdl-new
python3 tools/measure_input_latency.py --mode sdl --sdl-frame-input \
  --proton /path/to/GE-Proton --output build/input-latency/sdl-frame-new
python3 tools/measure_input_latency.py --mode dbus --proton /path/to/GE-Proton \
  --output build/input-latency/dbus-new
```

`--mode native|sdl|broker|dbus` selects the presentation path. SDL and D-Bus use
strict explicit synchronization; the broker uses its existing implicit path.
The test selects stock Proton, FSYNC, 16 guest vCPUs and 8 GiB guest memory.
The synthetic scene defaults to 16 fragment iterations;
`--fragment-iterations 512` runs the same fixture with 32 times as many shader
iterations. Use the same value and executable in every compared path. Native and guest CPU feature
policies differ, so a native/guest delta includes that difference.

`--sdl-frame-input` requires QEMU patch
`0006-sdl-service-input-on-active-scanout.patch`. It opts only that owned QEMU
into pumping SDL input on actual scanout activity, guarded against recursive
event dispatch. There is no additional polling timer; defaults remain unchanged.
The underlying research probe also exposes this flag for ordinary owned
D3D12 fixtures and benchmark applications.

Controls can be run individually:

```sh
python3 tools/measure_input_latency.py --mode native --delay-ms 20 \
  --proton /path/to/GE-Proton --output build/input-latency/delay-new
# Expected failure: timeout with zero responses. Retain the logs.
python3 tools/measure_input_latency.py --mode native --no-input \
  --proton /path/to/GE-Proton --output build/input-latency/no-input-new
```

## Validation and limits

The fixture starts at sequence zero and changes its barcode only on a Space
key press. The observer first identifies the fixed marker, then requires every
sequence from 1 through 40, in order, within a one-second response deadline.
Randomized 80–160 ms timer intervals avoid deliberately locking injection to a
capture phase. One response is outstanding at a time. All endpoints use host
`CLOCK_MONOTONIC`; there is no guest/host clock subtraction. Native and VM runs
must also pass the fixture's first/last GPU image oracles, drain its original
GPU queue and report exactly the requested number of Space events.

`observer.jsonl` preserves per-response injection, observation, capture-request
and capture-timestamp records. `report.json` contains percentiles, identities,
validation and limitations. The decoder rejects placeholders, intermediate
colors and corrupt marker/sequence cells. The no-input control must time out;
the +20 ms source delay must increase the measured median by 15–30 ms.

```sh
build/input-latency/observer --self-test
python3 tools/test_input_latency.py
```

Capture forces composition, adds readback/scheduling delay and prevents this
test from demonstrating direct-scanout benefits. The approximately 4 ms capture
wait at 240 Hz is included in the results, not subtracted. This headless session
is not the user's desktop compositor. A physical input-to-photon claim still
needs an external sensor/high-speed camera. The lightweight synthetic scene is
not representative of every game or frame rate. At 65536 diagnostic shader
iterations (roughly 70 source FPS here), capture completion itself often takes
over 100 ms. That heavy test does not establish a gaming latency improvement;
its raw numbers include substantial observation delay. See the research notes
before comparing those results. The diagnostic cap is 65536; ordinary fixture
loads keep the existing 8192 cap. Resize is disabled for latency
trials so the observer geometry remains fixed. The initial latency investigation
exposed a size mismatch rejection in the research compositor. NVIDIA research
patch 0026 and the atomic primary-plane transition path now address whole-image
resize; see the research notes for qualification and remaining restrictions.
AMDGPU native context keeps its existing same-size scanout requirement.

For the measured controls, baseline, matched optimization trials and UE5 smoke,
see [the research results](../../docs/nvkvm-research.md#controlled-input-response-measurement).

For isolated resize, input-coordinate and bounded-cache qualification, run
`tools/qualify_nvkvm_resize.py --help`. It uses a private session and captures
only the owned fullscreen output; actual desktop input cannot overwrite the
injected coordinate check. Its four cases cover ordinary resize, repeated
resize, startup at a smaller size and D-Bus resize.

## Runtime frame-feedback mode

`--runtime-feedback` uses the owned D3D12 fixture's public DXGI frame IDs,
completion statistics and original queue fence. The observer captures the
initial/final barcode only, and receives bounded records through inotify during
timing. It requires a fixture rebuilt with this opt-in support; old fixtures
cannot silently satisfy it.

```sh
python3 tools/measure_input_latency.py --mode sdl --runtime-feedback \
  --sdl-frame-input --fragment-iterations 65536 --proton /path/to/GE-Proton \
  --output build/presentation-feedback/runtime-new
```

The reported host interval ends at receipt of the runtime record. It includes
file/virtiofs and scheduling overhead. Separate source durations use QPC only;
no absolute host/guest clocks are subtracted. DXGI feedback is approximate and
in the VM describes the guest runtime, not the host window. It is not a monitor
or physical input-to-photon measurement. Original GPU-fence completion is
required independently of the runtime's statistics. Both independent image
checks must pass, with zero captures and zero source diagnostic readbacks during
timing. The existing capture method remains the default.

`--swapchain-latency-frames 2` selects the stock VKD3D queue override for the
owned trial only. Values 1–3 are accepted; 2 versus 3 is qualified here, and 1
is not. The runner requires a startup message confirming activation. With the
option absent, no override is set. The corresponding underlying variable is
`VKD3D_SWAPCHAIN_LATENCY_FRAMES`; forwarding it through a production launcher
is outside this research tool's qualification. Lower depths may trade GPU
throughput for latency, so the default remains unchanged.

## Tagged source frame to host presentation

The D-Bus diagnostic can now join each responsive rendered fixture frame to
an audited host Wayland commit and its compositor presentation feedback:

```sh
python3 tools/measure_input_latency.py --mode dbus --runtime-feedback \
  --host-presentation --swapchain-latency-frames 2 --fragment-iterations 65536 \
  --proton /path/to/GE-Proton --fixture /path/to/new/workload.exe \
  --observer /path/to/new/observer --output build/frame-identity/heavy-new
```

Both fixture and observer must be rebuilt. The normal runtime-feedback metric
is retained for pacing; `host-latency.json` contains the separate host dispatch
to first compositor-presented response. It matches input generation and exact
source frame barcode to the source's recorded DXGI Present ID/original fence,
then follows that viewer update through the audited commit and feedback. The
frame counter fails before 16-bit wrap. Reused DMA-BUF storage is not used as
frame identity. Missing/stale tags, mismatched input generations, discarded-only
responses, incomplete original fences and late responses fail qualification.

This adds an opt-in GPU barcode pass and **eight bytes of readback per viewer
update**, covered by the original real GL fence. No full desktop capture occurs
during timing. The final correctness capture is delayed 250 ms, and the join
requires every timed response to precede its request. Private Sway's headless
feedback is not monitor scanout or optical input-to-photon. Tag extraction and
Wayland audit logging remain in the measured pipeline; exclude these runs from
clean FPS comparisons. Source QPC absolute time is never subtracted from host
time. See `tools/qemu-display-probe/README.md` for the association checks.

`--source-frame-tags --viewer-presentation` enables matching source content and
protocol instrumentation with tag extraction disabled, for a cost control. It
produces the existing runtime-feedback metric, not joined host latency.
`--dispatch-delay-ms 20` is an explicit host calibration wait before actual
keyboard dispatch. Records retain both the control start and actual injection
clock; normal input latency starts at actual injection, while the separate
`control_response_ms` includes the measured wait. A source game-thread pause
(`--delay-ms`) can drain queued work and need not add linearly to total latency.
