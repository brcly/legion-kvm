# Legion KVM

Legion runs Windows applications through Proton inside a Linux KVM guest,
with a local desktop window and shared graphics access. The launcher detects
CPU topology and selects the graphics backend independently of CPU vendor.

The runtime includes NVIDIA and AMD graphics paths, guest initialization,
CPU compatibility profiles, display transport, input, audio and directory
sharing. AVX-512 is not a host requirement. The guest uses reduced XSTATE.
Hardware and graphics-driver support depend on the selected backend.

## Running

A packaged executable contains the guest kernel and runtime assets. Run it as
your desktop user with access to `/dev/kvm` and the graphics device. The desktop
presentation path requires Wayland.

Steam launch options:

```text
/path/to/legion %command%
```

Other launchers can place Legion before their Proton command. An explicit
launch accepts an installed Proton and an existing Wine prefix:

```sh
legion run --proton /path/to/Proton --prefix /path/to/prefix -- /path/to/application.exe
```

The selected prefix and home directory remain shared with the application.
Normal application and Proton writes reach those locations. Compatibility
repairs are applied inside the guest, and installed Proton files remain
unchanged. Selected Wine components must match the runtime compatibility
recipe; arbitrary Proton builds are not interchangeable with a given recipe.

Useful commands:

```sh
legion doctor
legion cpu-check
legion --version
```

`LEGION_RENDER_NODE` selects a DRM render node on systems with multiple GPUs.
`LEGION_MEMORY_MIB` selects guest memory; the default is 12288 MiB.
`LEGION_FULLSCREEN=1` opens the AMD display fullscreen at its configured resolution.
`LEGION_DISPLAY_BACKEND=auto|wayland|xwayland` selects Wine's display backend on NVIDIA; the default remains XWayland. Direct Wayland requires support in the selected Proton build. AMD uses Wayland.
`LEGION_STARTUP_UI=auto|window|log|off` controls startup feedback. Runtime files
are cached under `XDG_CACHE_HOME/legion`; logs use `XDG_STATE_HOME/legion`.

`LEGION_GUEST_XDG_CACHE_HOME`, `LEGION_GUEST_XDG_CONFIG_HOME`,
`LEGION_GUEST_XDG_DATA_HOME` and `LEGION_GUEST_XDG_STATE_HOME` forward
per-profile directories to the application as their unprefixed equivalents.
They must be absolute paths inside the host home directory, the only tree that
keeps its original path inside the guest. Legion keeps reading the unprefixed
`XDG_*` variables for its own runtime cache and session logs, so profiles do
not duplicate the runtime.

## Building

Install Python 3.11 or later and rootless Podman, or use an accessible Docker
engine. The complete build uses an isolated Debian toolchain; host Rust and
development libraries are not required. Allow at least 25 GiB of free disk
space and an internet connection for the first build.

Clone the repository and build the executable:

```sh
git clone https://github.com/brcly/legion-kvm.git
cd legion-kvm
python3 tools/build.py --proton /path/to/GE-Proton11-11 --jobs 4
```

The result is **`build/dist/legion`**. This single executable includes the guest
kernel, matching modules, graphics backends, QEMU and guest userspace. Copy it
to another supported system and mark it executable. Users supply their own
applications and installed Proton; neither is included in the executable.

The current compatibility input is GE-Proton11-11. The build verifies the
selected installation against `config/compatibility.json` before compiling.
It reads that installation without modifying it. A different Proton version
needs a compatible repair recipe; bypassing the hash check is unsupported.

Public dependency sources and checksums are recorded in `config/sources.json`.
Portable host ABI packages are pinned in `config/host-packages.json`.
The build downloads and compiles those sources, builds the Rust workspaces,
assembles the runtime and checks host-tool startup without AVX-512. Downloads
and intermediate outputs stay under the ignored `build/` directory. Repeating
the command resumes completed stages. Use `--output` for another build directory
or `--engine docker` to select Docker explicitly.

No module is installed into the host kernel. Runtime use requires x86-64 Linux,
enabled hardware virtualization, access to `/dev/kvm`, a Wayland desktop, and a
supported NVIDIA or AMD graphics device. GPU drivers remain on the host. Intel
CPUs use the same KVM launch path; Intel graphics are not supported.

To check the resulting executable before launching an application:

```sh
build/dist/legion doctor
build/dist/legion cpu-check
```

`cargo build --release` builds only the launcher for development. Use the
Python build command above for a complete, distributable executable.

## Development

Local Rust development requires Rust 1.99 and development libraries for
Wayland, XKB, EGL, GBM, DRM, SDL2 and Vulkan. The NVIDIA backend has a separate
Cargo workspace under `backends/nvidia`.

Unit tests live in each package’s `tests/unit/` directory (backend binary tests
use `tests/bin/`). Test-only fixtures and helper executables live under
`tests/support/`, with recorded fixture data in `tests/fixtures/`. Rust source
modules load their unit tests with `#[cfg(test)]` and explicit paths, preserving
private access without exposing internal APIs.
Launcher integration tests stay in `tests/cli.rs`; Python tests live in
`tests/python/`.

```sh
PYTHONPATH=tools python3 -m unittest discover -s tests/python -p 'test_*.py'
cargo fmt --all --check
cargo check --workspace --all-targets --all-features
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --manifest-path backends/nvidia/Cargo.toml --check
cargo test --workspace --manifest-path backends/nvidia/Cargo.toml --features device/vhost-user
cargo clippy --workspace --all-targets --manifest-path backends/nvidia/Cargo.toml --features device/vhost-user -- -D warnings
```

## License

The main Rust workspace is licensed under AGPL-3.0-only. NVIDIA backend
components retain their Apache-2.0, BSD-3-Clause and GPL licenses. Kernel and
QEMU components retain their respective upstream licenses. See `LICENSE` and
`NOTICE`.
