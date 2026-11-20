## Venus research tools

The separate [nvkvm-pv investigation](nvkvm-research.md) compares a pinned
NVIDIA forwarding prototype with the packaged Venus backend. It does not change
the production backend or AMD support.

These developer tools and saved reports are separate from the packaged runtime.
Historical NVIDIA-forwarding comparisons require the launcher/runtime from that
revision; the production launcher now has only Venus. Existing measurement files
under `build/venus/` remain usable by the report generators.

Private renderer and Mesa builders support A/B tests without replacing the
production bundle. `--reference-session` may name a prepared session or the
production runtime; Venus tests do not need guest NVIDIA library bindings.

Windows translation tests use the owned D3D11 and D3D12 shader fixtures through
stock Proton. Build both PE executables with LLVM tools and the Rust Windows
MSVC target installed, then run:

```sh
python3 tools/build_proton_workload.py --proton /path/to/Proton \
  --output build/venus/proton-imports --target-dir build/venus/windows-dx11 --api d3d11
python3 tools/build_proton_workload.py --proton /path/to/Proton \
  --output build/venus/proton-imports --target-dir build/benchmarks/windows-target
python3 tools/benchmark_venus_proton.py --reference-session /path/printed/by/doctor \
  --output build/venus/proton-comparison --proton /path/to/Proton --repetitions 3
```

Each trial gets a private prefix and cache. The tests check the selected GPU,
translation-layer logs and shader pixels; no Wine repair or CPU adapter is
injected. D3D11 uses three query slots and an explicit flush per frame; D3D12
also tests queue depths and mixed CPU work. Compare transports within each API.
The resulting rates describe synthetic offscreen work, not game performance.

For a D3D12 stress sweep, use the same selected Proton runtime on both guest
backends. Profiles vary shader arithmetic, resolution, repeated draws and CPU
work; every profile measures queue depths 1–3, with and without CPU work:

```sh
python3 tools/benchmark_venus_proton.py --reference-session /path/printed/by/doctor \
  --output build/venus/limits-screen --proton build/venus/winevulkan-test/proton \
  --apis d3d12 --modes venus nvidia --repetitions 1 --memory-mib 4096 \
  --profiles submission draws64 light1080 shader64 shader256 light4k shader4k overdraw64 cpu1m \
  --warmup-ms 500 --sample-ms 1500 --timeout 300
python3 tools/report_venus_limits.py --study limits-screen
```

Use a fresh output directory and more repetitions to confirm a result; the
report accepts repeated `--study` arguments for compatible saved studies.
`submission` and `draws64` use a 64×64 target. `shader4k` combines 3840×2160
with 256 integer shader rounds. `overdraw64` repeats 64 full-screen draws at
1080p. `cpu1m` adds one million dependent CPU rounds to the mixed cases.
Custom runs also accept `--draws-per-frame` and `--cpu-rounds`; multiple draws
are restricted to D3D12 because the DXVK fixture uses one draw.
If the tiny target produces equal GPU timestamps, the fixture rejects that
sample instead of assigning it a throughput score. `submission256` uses a
256×256 target to test submission throughput above that timing floor.

The report separates GPU execution, gaps between GPU frames, command recording,
submission and completed-frame cadence. It excludes CPU-to-GPU calibrated
latencies, whose clock mapping can produce negative intervals under Venus.
These probes do not measure texture streaming, descriptor churn, VRAM capacity,
ray tracing or all shader model features. The highest measured rate is specific
to this workload and its settings.

An experimental host command-ring polling patch targets scheduler overhead
between small Venus submissions. It polls with CPU pause instructions for a
bounded interval after the most recent command batch, then returns to the
original yield, sleep and idle notification policy. The default budget is zero;
fence and semaphore completion rules are unchanged. Continuous submissions can
keep the ring thread active, so throughput gains must be weighed against CPU
time rather than treated as free improvements.

```sh
python3 tools/build_venus_ring_probe.py --jobs 2
python3 tools/benchmark_venus_ring.py --reference-session /path/printed/by/doctor \
  --proton build/venus/winevulkan-test/proton --output build/venus/ring-ab
```

This interleaves 0, 20 and 50 µs budgets across three repetitions of small-target
and light 1080p work, using identical renderer binaries and fresh private
prefixes. It needs access to the non-sampling `perf task-clock:u` counter.
The report preserves paired gains, observed ranges, GPU gaps and whole-trial
CPU counters. Startup and validation are included in those CPU counters;
they are not steady-state CPU consumption per frame. Ring diagnostic counters
are disabled during the comparison. Rebuild a report without launching work:

```sh
python3 tools/benchmark_venus_ring.py --output build/venus/ring-ab --report-only
```

Both the ordinary Proton benchmark and the window/application probe accept
`--renderer-prefix build/venus/ring-probe/prefix --ring-spin-us 50` for explicit
tests. Budgets are limited to 0–200 µs. `--ring-stats` on the ordinary benchmark
logs polling counts for diagnosis; do not mix those instrumented runs with the
performance comparison. These experiments leave the packaged runtime unchanged.

The completion-path experiment builds a separate portable Mesa driver and the
guest bootstrap required to forward its options:

```sh
python3 tools/build_venus_feedback_probe.py --jobs 2
python3 tools/benchmark_venus_sync.py --reference-session /path/printed/by/doctor \
  --proton build/venus/winevulkan-test/proton --output build/venus/sync-ab \
  --variants current feedback50 feedback200
```

The current control keeps the 50 µs host-ring budget and Mesa's timeline sync
path. Feedback variants select Mesa's existing `no_timeline_sync` and
`no_query_feedback` paths, with 50 or 200 µs of initial guest-side semaphore
polling. Polling still checks the actual semaphore value and API timeout; the
original backoff and watchdog take over afterwards. The default guest polling
budget is zero. Disabling timeline sync alone was slower in screening and is
not an equivalent optimisation.

The study interleaves three repeats with identical driver, renderer, Proton,
bootstrap and executable hashes. Optional `query-off` and `feedback200-query`
variants isolate query-result transport from the semaphore polling change.
CPU counters cover whole trials, including startup and validation. Regenerate a
saved report with `--output build/venus/sync-ab --report-only`.

For explicit window/application tests, both probe tools accept:

```sh
--renderer-prefix build/venus/ring-probe/prefix --ring-spin-us 50 \
--venus-driver build/venus/feedback-probe/stage/usr/lib/x86_64-linux-gnu/libvulkan_virtio.so \
--venus-perf no_timeline_sync no_query_feedback --feedback-spin-us 200
```

Guest budgets are bounded to 0–200 µs, and a nonzero budget requires the patched
driver and explicit legacy semaphore mode. These are experimental settings;
the normal game-launch defaults do not select them. Query-result transport,
multi-queue behaviour, frame pacing and broader game coverage need separate
qualification before selecting a production policy.

`tools/probe_venus_proton_window.py` separately exercises the owned D3D12
swapchain through Venus, guest KMS, and Legion's existing external-texture QEMU
display path. It accepts the same `--reference-session`, `--output` and
`--proton` arguments. `--application-dir`, `--executable` and repeated
`--argument=...` options test a private application copy; `--audio` supplies the
normal desktop audio device. `--exception-trace` preserves bounded software UI
captures and guest crash contexts, because an error dialog may otherwise be
invisible on a display path that accepts only GPU buffers. These diagnostic
captures are excluded from performance measurements.

The owned D3D12 fixture also saves `d3d12-capabilities.json`, including the
reported shader model and feature level. The window probe's explicit
`--diagnostic-shader-model 6_6` option can isolate an application's capability
gate. It does not implement missing features or qualify them, and is never
enabled automatically for ordinary launches. A successful diagnostic run must
be followed by testing the real driver-property forwarding and shader behavior.

An isolated maintenance7 backport tests that property forwarding without a
shader-model override. `VK_KHR_maintenance7` lets VKD3D discover the native
driver underneath Venus and apply its existing driver workarounds. The builder
currently reconstructs the matching **GE-Proton11-11 x86_64 graphics stack**;
this is an experiment, not a replacement DLL installer for arbitrary Proton
versions. It includes the release's server protocol declarations so rebuilt
graphics modules use the installed Wine server's request numbers.

```sh
python3 tools/build_venus_winevulkan.py --proton /path/to/GE-Proton11-11 \
  --output build/venus/winevulkan-test --jobs 2
python3 tools/benchmark_venus_proton.py --reference-session /path/printed/by/doctor \
  --output build/venus/maintenance7-check --proton build/venus/winevulkan-test/proton \
  --apis d3d12 --modes venus --rounds 64 --repetitions 1
```

Build the normal Podman build image first. Wine and GE sources are verified
against recorded checksums; `runtime.json` records inputs, patches and the six
replaced graphics files. The private Proton copy leaves the installed runtime
untouched. The experiment does not replace ntdll or enable a CPU adapter.
The generated-code backport must be applied after the old Wine generator runs;
that generator cannot itself regenerate the recursive maintenance7 conversions.
32-bit games and other Proton releases still need separate qualification.

QMP capture can be unavailable for accelerated scanout. An optional host
`tools/venus_capture_toplevel.c` helper uses the generated
`ext-foreign-toplevel-list-v1` client protocol to identify only the named VM;
`grim -T` can then capture that exact window. The window probe checks captures
for nonuniform content separately from guest scanout counters. Neither check
establishes general game compatibility.

With Wayland development headers, protocol XML, `wayland-scanner`, a C compiler
and `grim` installed, build that optional host helper as follows:

```sh
wayland-scanner client-header /usr/share/wayland-protocols/staging/ext-foreign-toplevel-list/ext-foreign-toplevel-list-v1.xml build/venus/venus-toplevel-client.h
wayland-scanner private-code /usr/share/wayland-protocols/staging/ext-foreign-toplevel-list/ext-foreign-toplevel-list-v1.xml build/venus/venus-toplevel-code.c
cc -O2 -Wall -Wextra -Werror -I build/venus tools/venus_capture_toplevel.c build/venus/venus-toplevel-code.c -lwayland-client -o build/venus/capture-toplevel
```

The optional capture analysis uses Pillow. `tools/report_venus_proton.py`
generates the local study report and charts from saved measurements; it uses
Matplotlib and does not launch workloads.
`tools/report_venus_maintenance7.py` renders the saved maintenance7 follow-up,
including automatic capabilities, the UE game run and private runtime provenance.

## AMD loader and device-selection audit

The November 15 audit reproduced a reported `vkCreateInstance` failure with a
private Arch RADV 26.2.6 driver on the integrated Radeon 610M. The host driver
loaded normally outside Legion, but Legion's older bundled Wayland client
library lacked `wl_display_dispatch_queue_timeout`. Adding Wayland 1.26.0 to
the checksummed private host ABI resolves that failure without changing the
installed host driver. This is a packaging dependency fix, not a GPU model or
Proton version check.

With both RADV and NVIDIA ICDs visible, the Mesa device-selection layer also
returned groups containing the GPU hidden from ordinary device enumeration.
Venus could not translate those hidden handles. The bundled layer now returns
a singleton group for its selected device when force-default selection is on.
Both AMD and NVIDIA selection pass with the two ICDs visible.

The same guest loader error can also be reproduced with a stale
`VK_DRIVER_FILES` path or a `VK_LOADER_DRIVERS_SELECT` filter that hides RADV.
The failure report now preserves initialization errors before testing device
identity, and records the inherited driver overrides and filters. These
overrides remain supported for deliberate use of private host drivers.

Evidence is retained in `build/venus/amd-environment-audit/`: the failing
sessions, corrected driver-selection matrix, package checksum, and a synthetic
GPU pixel check with RADV 26.2.6. Driver packages were extracted privately;
no host packages were installed. This audit does not qualify the RX 9070 XT,
PBZ compatibility, or game presentation/performance with that driver.

