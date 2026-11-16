# Legion KVM

Legion runs Windows applications through Proton inside a Linux KVM guest.
Graphics use Mesa Venus over virtio-GPU, with a local desktop window, input,
audio and shared directories. The guest kernel, Mesa, virglrenderer and QEMU
are bundled. GPU vendor drivers stay on the host; Legion no longer forwards
NVIDIA ioctls or maintains NVIDIA driver ABI tables.

## Running

Run the packaged executable as your desktop user with access to `/dev/kvm`
and your GPU render node. A Wayland desktop and a host Vulkan driver with the
external-memory and synchronization capabilities required by Venus are needed.
AVX-512 is not required; the guest uses the reduced XSTATE CPU profile.

Steam launch options:

```text
/path/to/legion %command%
```

For native Faugus, Lutris and Heroic packages, put only `/path/to/legion` in
the launch-command prefix or wrapper field and keep the launcher's installed
Proton and prefix selections. Faugus uses **Launch Arguments**, Lutris uses
**System options → Command prefix**, and Heroic uses **Advanced → Wrapper**.
Do not append the game executable or `%command%` in those fields.
See [launcher integration](docs/launcher-integration.md) for supported runners,
read-only diagnostics and Flatpak limitations.

For an environment toggle, configure `legion auto` as the wrapper, then set
`LEGION=1` for games that should use the VM. Unset it or use `LEGION=0` to run
the original launcher command. Use launcher-wide wrapper defaults where available.

An explicit launch can select an installed Proton and Wine prefix:

```sh
legion run --proton /path/to/Proton --prefix /path/to/prefix -- /path/to/application.exe
```

The selected prefix, application directory and home remain shared and writable.
Installed Proton files remain unchanged. Legion does not require a particular
Proton version, but that Proton must support Wine Wayland and the Vulkan features
the game needs. In particular, Vulkan layered-driver reporting through
`VK_KHR_maintenance7` matters for some VKD3D feature checks. The private matched
Wine graphics backport used in UE5 testing is a research tool, not a universal
replacement for arbitrary Proton builds. See [research notes](docs/venus-research.md).

```sh
legion doctor
legion doctor --graphics
legion cpu-check
```

`doctor` checks host prerequisites. `doctor --graphics` boots a disposable VM,
opens the host display window and verifies guest hardware Vulkan enumeration
without touching a game or prefix. Enumeration alone does not prove rendering;
application and synthetic tests provide that evidence separately.

Configuration:

- `LEGION_RENDER_NODE` selects a DRM render node, such as `/dev/dri/renderD128`.
- `LEGION_MEMORY_MIB` sets guest RAM; the default is 12288 MiB.
- `LEGION_FULLSCREEN=1` opens the host display fullscreen.
- `LEGION_WINDOW_SYSTEM=auto|wayland|x11` selects the host window system. Auto
  retries through XWayland only if initial Wayland window creation fails.
- Games use the guest Wayland compositor. `LEGION_DISPLAY_BACKEND=wayland`
  remains accepted; the retired NVIDIA XWayland forwarding path is unavailable.
- `LEGION_STARTUP_UI=auto|window|log|off` controls startup feedback.
- `LEGION_STARTUP_TIMEOUT_SECONDS` allows 30–900 seconds to the first frame
  (default 180). Interactive sessions have no time limit after that frame.

The desktop window leaves system shortcuts, including Super/Windows bindings,
with the host compositor. Guest cursor images use the virtual GPU's hardware
cursor; moving the pointer does not copy the game's framebuffer through the CPU.

Logs are saved under `XDG_STATE_HOME/legion`, normally `~/.local/state/legion`.
Keep the printed session directory when reporting failures. It contains the
build identifier, launch configuration, VM report, QEMU/console logs and guest
startup stages. Include `legion --version`, `doctor`, and `doctor --graphics`.
Runtime extraction is cached under `XDG_CACHE_HOME/legion`.

The Venus cutover is tested on an RTX 4070 Ti SUPER with NVIDIA 620.70.06;
the earlier performance studies used 615.78.08. The Ryzen 9800X3D integrated
Radeon 610M also passed D3D11 pixel checks and D3D12 pixel/presentation checks
through Venus using a private RADV 25.0.7 host driver. RADV 26.2.6 also passes
guest enumeration and a synthetic GPU pixel check on that integrated GPU,
including selection with both AMD and NVIDIA host drivers visible. Other
GPU/driver combinations and 32-bit games still require qualification.

An AMD kernel driver alone is insufficient: the host also needs an AMD Vulkan
driver (for example, `vulkan-radeon` on Arch-based systems). Normal sessions
verify that the guest Vulkan vendor/device IDs match the selected render node
before starting the application. Missing drivers or a conflicting
`VK_DRIVER_FILES` setting must fail rather than silently use another GPU.
Host Vulkan driver overrides and filters inherited from launchers are recorded
in `vm/report.json` under `host_vulkan_environment`. Their manifests must be
usable by Legion's 64-bit host renderer, outside any runner container.
NVAPI/NGX integration is disabled on this path pending separate validation.

`LEGION_GUEST_XDG_CACHE_HOME`, `LEGION_GUEST_XDG_CONFIG_HOME`,
`LEGION_GUEST_XDG_DATA_HOME` and `LEGION_GUEST_XDG_STATE_HOME` forward
per-profile directories to the application as their unprefixed equivalents.
They must be absolute paths inside the host home directory, the only tree that
keeps its original path inside the guest. Legion keeps reading the unprefixed
`XDG_*` variables for its own runtime cache and session logs, so profiles do
not duplicate the runtime.

## Building

Install Python 3.11 or later and rootless Podman or an accessible Docker engine.
The build uses an isolated Debian toolchain; host Rust and development libraries
are not required. Allow at least 25 GiB and internet access for a first build.

```sh
git clone https://github.com/brcly/legion-kvm.git
cd legion-kvm
python3 tools/build.py --jobs 4
```

The result is **`build/dist/legion`**, a single executable containing the runtime.
Users supply their applications, Proton and host Vulkan driver. No module is
installed into the host kernel. No installed Proton is needed to build Legion;
`--proton /path/to/Proton` optionally adds dependency discovery without pinning
normal launches to that version.

Sources/checksums live in `config/sources.json`; the private host ABI package
baseline is recorded in `config/host-packages.json`. The build compiles Venus,
virglrenderer with Venus enabled, QEMU, the guest kernel and production Rust
binaries, then checks host-tool startup on CPU profiles without AVX-512.
Downloads and intermediate files stay in ignored `build/`. Repeating the build
resumes completed stages; `--output` selects another build directory.

`--wine-repair --proton /matching/Proton` can include the existing optional
ntdll repair, checked against `config/compatibility.json`. It remains disabled
unless `LEGION_WINE_REPAIR=1` is set. Normal Venus launches use stock Proton and
have no ntdll hash requirement.

## Development

The production path is launcher → QEMU/virtio-GPU → virglrenderer/Vulkan, with
Mesa Venus in the guest. CPU compatibility, directory sharing, Proton startup,
KMS presentation, input and audio remain common services. Research workloads
and benchmark tools are not required in a distributed runtime.

Local development needs Rust 1.99 and Wayland, XKB, EGL, GBM, DRM, SDL2 and
Vulkan development libraries. `cargo build --release` builds only the launcher.

```sh
PYTHONPATH=tools python3 -m unittest discover -s tests/python -p 'test_*.py'
cargo fmt --all --check
cargo check --workspace --all-targets --all-features
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

[Venus research tools](docs/venus-research.md) cover correctness fixtures,
performance comparisons and the private Wine graphics experiment.

## License

The Rust workspace is AGPL-3.0-only. Linux, QEMU, Mesa, virglrenderer and other
bundled components retain their upstream licenses. Source notices and dependency
licenses accompany the packaged runtime. See `LICENSE` and `NOTICE`.
