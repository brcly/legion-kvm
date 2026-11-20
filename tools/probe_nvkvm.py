#!/usr/bin/env python3
"""Boot an isolated nvkvm-pv graphics research VM using the existing Legion kernel."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import resource
import shlex
import signal
import shutil
import socket
import subprocess
import tempfile
import time

from build import container_security
from vkd3d_queue_profile import normalize_trace
from shader_cache import copy_seed, cache_metadata, manifest as cache_manifest

ROOT = Path(__file__).resolve().parents[1]
# Match Legion's production bootstrap XSTATE policy for the transport comparison.
CPU_MODEL = 'host,' + ','.join('-'+feature for feature in (
    'avx512f avx512dq avx512ifma avx512pf avx512er avx512cd avx512bw avx512vl '
    'avx512vbmi avx512vbmi2 avx512vnni avx512bitalg avx512-vpopcntdq avx512-vp2intersect '
    'avx512-bf16 avx512-fp16 avx512-4vnniw avx512-4fmaps pku amx-tile').split())


def qmp_command(qmp, command, arguments=None):
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(2)
        client.connect(str(qmp))
        stream = client.makefile('rwb', buffering=0)
        json.loads(stream.readline())
        answer = None
        for request in [dict(execute='qmp_capabilities'), dict(execute=command, arguments=arguments or {})]:
            stream.write((json.dumps(request)+'\n').encode())
            while True:
                result = json.loads(stream.readline())
                if 'event' not in result:
                    break
            if 'error' in result:
                raise RuntimeError(str(result['error']))
            answer = result.get('return')
        return answer


def capture(qmp, destination):
    qmp_command(qmp, 'screendump', dict(filename=str(destination), format='ppm'))
    from PIL import Image
    with Image.open(destination) as source:
        source.save(destination, format='PNG')


def inject_input(qmp):
    events = [dict(type='key', data=dict(down=True, key=dict(type='qcode', data='a'))),
              dict(type='abs', data=dict(axis='x', value=16384)),
              dict(type='abs', data=dict(axis='y', value=16384))]
    qmp_command(qmp, 'input-send-event', dict(events=events))
    qmp_command(qmp, 'input-send-event', dict(events=[
        dict(type='key', data=dict(down=False, key=dict(type='qcode', data='a')))]))


def pin_vcpus(qmp, qemu_pid, count, threads_per_core):
    allowed = sorted(os.sched_getaffinity(0))
    if threads_per_core == 2:
        cores = {}
        for cpu in allowed:
            topology = Path(f'/sys/devices/system/cpu/cpu{cpu}/topology')
            key = tuple((topology/name).read_text().strip() for name in ('physical_package_id', 'core_id'))
            cores.setdefault(key, []).append(cpu)
        allowed = [cpu for siblings in cores.values() if len(siblings) >= 2 for cpu in siblings[:2]]
    if len(allowed) < count:
        raise RuntimeError('not enough allowed host CPUs for this guest topology')
    cpus = sorted(qmp_command(qmp, 'query-cpus-fast'), key=lambda cpu: cpu['cpu-index'])
    if len(cpus) != count:
        raise RuntimeError('QEMU vCPU count changed before pinning')
    records = []
    for guest, host in zip(cpus, allowed):
        tid = guest['thread-id']
        if not Path(f'/proc/{qemu_pid}/task/{tid}').is_dir():
            raise RuntimeError('refusing to change affinity outside the owned QEMU process')
        os.sched_setaffinity(tid, {host})
        records.append(dict(guest_cpu=guest['cpu-index'], thread_id=tid, host_cpu=host,
                            qemu_topology=guest.get('props')))
    return records


def pattern_verified(path):
    from PIL import Image
    with Image.open(path) as source:
        image = source.convert('RGB')
        width, height = image.size
        colors = {tuple(channel > 180 for channel in image.getpixel((width*x//4, height*y//4)))
                  for x, y in [(1, 1), (3, 1), (1, 3), (3, 3)]}
        return colors == {(True, False, False), (False, True, False),
                          (False, False, True), (True, True, True)}


def nonuniform(path):
    """Require colored fixture content; QEMU's inactive-display text is not a frame."""
    from PIL import Image
    with Image.open(path) as image:
        rgb = image.convert('RGB')
        varied = max(high-low for low, high in rgb.getextrema()) > 100
        raw = rgb.resize((160, 90)).tobytes()
        pixels = zip(raw[0::3], raw[1::3], raw[2::3])
        colored = sum(max(pixel)-min(pixel) > 40 and max(pixel) > 80 for pixel in pixels)
        return varied and colored > 160*90//200


def initramfs(destination, entries):
    """Write rootless newc; inputs are only the init and three guest modules."""
    with destination.open('wb') as out:
        for inode, (name, mode, data) in enumerate([
            ('modules', 0o040755, b''), *entries, ('TRAILER!!!', 0, b'')], 1):
            name = name.encode() + b'\0'
            fields = [inode, mode, 0, 0, 1, 0, len(data), 0, 0, 0, 0, len(name), 0]
            out.write(('070701' + ''.join(f'{value:08x}' for value in fields)).encode())
            out.write(name)
            out.write(b'\0' * (-out.tell() % 4))
            out.write(data)
            out.write(b'\0' * (-out.tell() % 4))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--build', type=Path, default=ROOT/'build/nvkvm-pv-display')
    parser.add_argument('--output', type=Path, required=True, help='new result directory')
    parser.add_argument('--config', type=Path, help='owned Vulkan fixture configuration')
    parser.add_argument('--cpus', type=int, default=4)
    parser.add_argument('--cpu-policy', choices=['restricted', 'host'], default='restricted',
                        help='research control only: compare Legion CPU restrictions with unmasked host features')
    parser.add_argument('--ntsync', action='store_true', help='load the matching guest NT synchronization module')
    parser.add_argument('--threads-per-core', type=int, choices=[1, 2], default=1,
                        help='research guest topology; keep vCPU count unchanged')
    parser.add_argument('--pin-vcpus', action='store_true', help='pin only owned QEMU vCPU threads to matching allowed host CPUs')
    parser.add_argument('--proton', type=Path, help='read-only Proton installation for an owned PE fixture')
    parser.add_argument('--fixture', type=Path, help='owned offscreen D3D11 or D3D12 fixture')
    parser.add_argument('--presentation', choices=['gbm', 'egl', 'cube'], help='virtual KMS presentation fixture')
    parser.add_argument('--window', action='store_true', help='show only the owned VM display in SDL')
    parser.add_argument('--window-fullscreen', action='store_true', help='match a fullscreen native presentation path')
    parser.add_argument('--host-display', choices=['sdl', 'broker', 'dbus'], default='sdl',
                        help='NVIDIA research: compare SDL GL scanout with direct Wayland DMA-BUF relay')
    parser.add_argument('--sdl-gl', choices=['on', 'es'], default='on', help='host SDL context for presentation qualification')
    parser.add_argument('--sdl-frame-input', action='store_true', help='experimental input pumping on active SDL scanout frames')
    parser.add_argument('--scanout-external', action='store_true', help='exercise the packaged external-texture shader (AMD research)')
    parser.add_argument('--host-ui-vendor', choices=['mesa', 'nvidia'], help='separate the host display driver from AMD guest rendering')
    parser.add_argument('--proton-window', action='store_true', help='exercise the owned D3D12 swapchain fixture')
    parser.add_argument('--fixture-resize', action='store_true', help='qualify owned D3D12 swapchain resize transitions')
    parser.add_argument('--private-output', help='capture an isolated headless qualification output')
    parser.add_argument('--fixture-start-scaled', action='store_true')
    parser.add_argument('--fixture-resize-cycles', type=int, default=1)
    parser.add_argument('--fixture-fragment-iterations', type=int, default=16)
    parser.add_argument('--kms-hz', type=int, help='NVIDIA virtual refresh fallback for headless measurements')
    parser.add_argument('--virtual-refresh-hz', type=int, help='override the virtual display cadence for research (1..1000)')
    parser.add_argument('--kms-wait', choices=['sleep', 'events'], default='events',
                        help='KMS event waits (default) or legacy fixed polling')
    parser.add_argument('--kms-refresh', choices=['legacy', 'kms'], default='legacy')
    parser.add_argument('--kms-explicit-sync', action='store_true',
                        help='experimental Wayland timelines with verified host GPU read completion')
    parser.add_argument('--kms-acquire-wake', action='store_true', help='wake the guest KMS loop on pending acquire timeline eventfds')
    parser.add_argument('--ring-idle-iterations', type=int, help='bounded upstream command-ring idle spin budget (0 selects its default)')
    parser.add_argument('--poll-arm', choices=['sync', 'async'], default='sync', help='NVIDIA event registration strategy')
    parser.add_argument('--halt-poll-ns', type=int,
                        help='research KVM halt polling maximum for this owned NVIDIA VM only')
    parser.add_argument('--poll-transport', choices=['sync', 'enqueue'], default='sync',
                        help='NVIDIA helper registration acknowledgement policy (research)')
    parser.add_argument('--sync-file-bridge', action='store_true',
                        help='experimental NVIDIA host-to-guest GPU completion fences')
    parser.add_argument('--memory-mib', type=int, default=4096)
    parser.add_argument('--ram-layout', choices=['memfd', 'aligned-memfd'], default='aligned-memfd',
                        help='compare default memfd RAM with an explicitly 2 MiB-aligned mapping')
    parser.add_argument('--host-thp', choices=['off', 'system'], default='off',
                        help='match packaged QEMU process policy (off), or test host THP policy')
    parser.add_argument('--application-dir', type=Path, help='copy an owned benchmark application into this run')
    parser.add_argument('--application-storage', choices=['virtiofs', 'tmpfs'], default='virtiofs',
                        help='owned benchmark I/O control: stage its files in guest RAM')
    parser.add_argument('--executable', default='Benchmark.exe')
    parser.add_argument('--argument', action='append', default=[])
    parser.add_argument('--timeout', type=int, default=240)
    parser.add_argument('--extra-probe', type=Path, help='owned Linux graphics probe run after the checksum fixture')
    parser.add_argument('--extra-probe-argument', action='append', default=[])
    parser.add_argument('--backend-debug', action='store_true')
    parser.add_argument('--present-timing', action='store_true', help='upstream present timing counters')
    parser.add_argument('--vulkan-details', action='store_true', help='capture complete Vulkan capabilities for native/guest comparison')
    parser.add_argument('--telemetry', action='store_true', help='record GPU clocks/load and owned QEMU thread CPU time')
    parser.add_argument('--profile-host', action='store_true', help='launch the owned VM under userspace perf sampling')
    parser.add_argument('--debug-qemu', action='store_true', help='diagnostic only: obtain an owned QEMU crash backtrace')
    parser.add_argument('--viewer-gpu-priority', choices=['default','medium','high'], default='default')
    parser.add_argument('--viewer-priority-shim', type=Path, default=ROOT/'build/gpu-priority/liblegion_egl_priority.so')
    parser.add_argument('--viewer-early-swap', action='store_true')
    parser.add_argument('--viewer-gpu-timing', action='store_true')
    parser.add_argument('--viewer-stage-profile', action='store_true', help='bounded CPU stage observations, saved after rendering')
    parser.add_argument('--viewer-fence-wait-us', type=int, choices=[50,100,250,1000], default=1000,
                        help='research GL fence wait timeout; source ownership still requires successful completion')
    parser.add_argument('--viewer-presentation', action='store_true')
    parser.add_argument('--viewer-frame-tags', action='store_true')
    parser.add_argument('--fixture-frame-tags', action='store_true')
    parser.add_argument('--viewer-presentation-delay-ms', type=int, default=0)
    parser.add_argument('--viewer-fail-read-after', type=int, help='D-Bus negative test: terminate before acknowledging this frame')
    parser.add_argument('--request-stats', action='store_true', help='print virtual-device request counts on VM exit')
    parser.add_argument('--queue-profile', action='store_true', help='diagnostic VKD3D queue timeline for an owned D3D12 application')
    parser.add_argument('--ioctl-profile', type=Path, help='owned diagnostic preload for the Linux checksum workload only')
    parser.add_argument('--input-check', action='store_true', help='inject input into only the owned VM and inspect delivery')
    parser.add_argument('--runtime-feedback', action='store_true')
    parser.add_argument('--swapchain-latency-frames', type=int, choices=[1,2,3])
    parser.add_argument('--application-final-capture', action='store_true',
                        help='capture application output only after saving its completed benchmark result')
    parser.add_argument('--shader-cache-seed', type=Path, help='copy a private shader cache into guest RAM before starting Proton')
    parser.add_argument('--preserve-shader-cache', action='store_true', help='export shader cache after Proton and compositor exit')
    parser.add_argument('--surface-diagnostics', choices=['on','off'], default='on',
                        help='research KMS surface/cadence diagnostics; off removes periodic writes and software UI dumps')
    parser.add_argument('--latency-probe', action='store_true', help='diagnostic input-reactive barcode fixture in a private compositor')
    parser.add_argument('--latency-delay-ms', type=int, default=0)
    parser.add_argument('--latency-observer-report', type=Path)
    parser.add_argument('--backend', choices=['nvidia', 'amdgpu-native'], default='nvidia')
    parser.add_argument('--amd-mesa', type=Path, default=ROOT/'build/amd-native/stage/usr/lib/x86_64-linux-gnu/libvulkan_radeon.so')
    parser.add_argument('--amd-renderer', type=Path, default=ROOT/'build/amd-native/renderer-prefix/lib/libvirglrenderer.so.1')
    args = parser.parse_args()
    if args.extra_probe_argument and not args.extra_probe:
        parser.error('--extra-probe-argument requires --extra-probe')
    if not 1 <= args.cpus <= (os.cpu_count() or 1):
        parser.error('--cpus exceeds the available host CPUs')
    if args.profile_host and (args.pin_vcpus or args.telemetry):
        parser.error('--profile-host is a diagnostic run; use separate runs for pinning and telemetry')
    if (args.poll_arm == 'async' or args.poll_transport == 'enqueue') and args.backend != 'nvidia':
        parser.error('poll registration experiments apply to the NVIDIA research module')
    if args.cpus % args.threads_per_core:
        parser.error('--cpus must divide evenly into the requested topology')
    if bool(args.proton) != bool(args.fixture or args.application_dir) or (args.fixture and args.application_dir):
        parser.error('--proton requires exactly one of --fixture or --application-dir')
    if args.queue_profile and not args.proton:
        parser.error('--queue-profile requires an owned Proton application')
    if not 2048 <= args.memory_mib <= 12288 or not 60 <= args.timeout <= 360:
        parser.error('memory or timeout outside research bounds')
    if args.kms_hz is not None and not 1 <= args.kms_hz <= 1000:
        parser.error('--kms-hz must be 1..1000')
    if args.virtual_refresh_hz is not None and (not 1 <= args.virtual_refresh_hz <= 1000 or not args.window or args.backend != 'nvidia'):
        parser.error('--virtual-refresh-hz requires an NVIDIA window and a value in 1..1000')
    if args.ring_idle_iterations is not None and (not 0 <= args.ring_idle_iterations <= 1000000 or args.backend != 'nvidia'):
        parser.error('--ring-idle-iterations requires NVIDIA and a value in 0..1000000')
    if args.halt_poll_ns is not None and (not 0 <= args.halt_poll_ns <= 2000000 or args.backend != 'nvidia'):
        parser.error('--halt-poll-ns requires NVIDIA and a value in 0..2000000')
    if args.application_dir:
        executable = Path(args.executable)
        if executable.is_absolute() or '..' in executable.parts or not args.executable.lower().endswith('.exe'):
            parser.error('--executable must be a relative Windows executable')
        args.proton_window = True
    elif args.application_storage != 'virtiofs':
        parser.error('--application-storage requires --application-dir')
    if args.kms_explicit_sync and not (args.sync_file_bridge and args.backend == 'nvidia'
            and args.window and args.host_display in ('sdl', 'dbus')
            and (args.proton_window or args.presentation == 'cube')):
        parser.error('--kms-explicit-sync requires NVIDIA sync-file-bridge and an SDL/D-Bus cube/Proton window')
    if args.host_display == 'dbus' and not (args.backend == 'nvidia' and args.window
            and args.kms_explicit_sync):
        parser.error('--host-display dbus requires an NVIDIA window with sync-file-bridge and kms-explicit-sync')
    if args.viewer_early_swap and (not args.viewer_presentation or args.viewer_presentation_delay_ms):parser.error('early swap requires presentation reporting without delay')
    if args.viewer_gpu_timing and not (args.viewer_presentation or args.viewer_stage_profile):
        parser.error('GPU timing requires viewer presentation reporting or stage profiling')
    if args.viewer_gpu_priority!='default' and args.host_display!='dbus':parser.error('viewer GPU priority requires D-Bus')
    if args.viewer_stage_profile and (args.host_display!='dbus' or args.viewer_early_swap):
        parser.error('viewer stage profile requires D-Bus and original swap ordering')
    if args.viewer_fence_wait_us!=1000 and args.host_display!='dbus':
        parser.error('viewer fence wait control requires D-Bus')
    if args.kms_acquire_wake and (not args.kms_explicit_sync or args.kms_wait!='events'):
        parser.error('acquire wake requires explicit sync and KMS event waits')
    if args.viewer_presentation and args.host_display!='dbus':parser.error('--viewer-presentation requires the D-Bus viewer')
    if not 0<=args.viewer_presentation_delay_ms<=100 or (args.viewer_presentation_delay_ms and not args.viewer_presentation):parser.error('presentation delay requires the opt-in viewer report and0..100ms')
    if args.viewer_fail_read_after is not None and (args.host_display != 'dbus' or args.viewer_fail_read_after < 1):
        parser.error('--viewer-fail-read-after requires D-Bus and a positive frame count')
    if args.sdl_frame_input and not (args.window and args.host_display == 'sdl' and args.backend == 'nvidia'):
        parser.error('--sdl-frame-input requires an NVIDIA SDL window')
    if args.proton and args.presentation:
        parser.error('Proton and KMS fixture tests run separately')
    if args.proton_window and not args.proton:
        parser.error('--proton-window requires --proton and --fixture')
    if not 1 <= args.fixture_resize_cycles <= 48 or (args.fixture_resize_cycles != 1 and not args.fixture_resize):
        parser.error('--fixture-resize-cycles requires --fixture-resize and a value in 1..48')
    if not 0 <= args.fixture_fragment_iterations <= (65536 if args.latency_probe else 8192) or (args.fixture_fragment_iterations != 16 and (not args.proton_window or args.application_dir)):
        parser.error('--fixture-fragment-iterations requires an owned presentation fixture and at most 8192 iterations (65536 in latency diagnostic mode)')
    if args.fixture_start_scaled and (not args.proton_window or args.fixture_resize or args.application_dir or args.latency_probe):
        parser.error('--fixture-start-scaled requires the ordinary non-resizing owned Proton window fixture')
    if args.fixture_resize and (not args.proton_window or args.application_dir or args.latency_probe):
        parser.error('--fixture-resize requires the ordinary owned Proton window fixture')
    if args.window_fullscreen and not args.window:
        parser.error('--window-fullscreen requires --window')
    if args.host_display == 'broker' and (not args.window or args.backend != 'nvidia' or args.virtual_refresh_hz is not None):
        parser.error('--host-display broker requires an NVIDIA window and uses broker/monitor timing')
    if args.input_check and not (args.presentation == 'cube' or args.proton_window):
        parser.error('--input-check requires the cube or Proton window fixture')
    if args.latency_probe and not (args.proton_window and args.window and args.latency_observer_report
                                  and not args.application_dir and not args.input_check):
        parser.error('--latency-probe requires an owned Proton window fixture and independent observer report')
    if args.swapchain_latency_frames is not None and not args.proton:parser.error('--swapchain-latency-frames requires Proton')
    if args.application_final_capture and not (args.application_dir and args.window):
        parser.error('--application-final-capture requires a windowed application benchmark')
    if (args.shader_cache_seed or args.preserve_shader_cache) and not args.application_dir:
        parser.error('shader cache controls require an owned application benchmark')
    if args.surface_diagnostics=='off' and (not args.application_dir or args.input_check or args.fixture_resize):
        parser.error('surface diagnostics can be disabled only for application benchmarks without input/resize checks')
    if args.viewer_frame_tags and not (args.viewer_presentation and args.fixture_frame_tags):parser.error('viewer frame tags require presentation feedback and source tags')
    if args.fixture_frame_tags and not args.runtime_feedback:parser.error('source frame tags require runtime feedback')
    if args.runtime_feedback and not args.latency_probe:parser.error('--runtime-feedback requires --latency-probe')
    if not 0 <= args.latency_delay_ms <= 100 or (args.latency_delay_ms and not args.latency_probe):
        parser.error('latency delay requires the diagnostic probe and 0..100 ms')
    if args.backend == 'amdgpu-native' and (args.presentation in ('gbm', 'egl') or args.application_dir or args.kms_hz):
        parser.error('AMD native-context uses the cube/Proton fixtures; NVIDIA module settings do not apply')
    if args.scanout_external and (args.backend != 'amdgpu-native' or not args.window):
        parser.error('--scanout-external requires the AMD window presentation test')
    if args.host_ui_vendor and (args.backend != 'amdgpu-native' or not args.window):
        parser.error('--host-ui-vendor requires the AMD window presentation test')
    base = args.build.resolve()
    if args.halt_poll_ns is not None:
        record = json.loads((base/'build.json').read_text())
        if '0024-control-halt-polling-only-for-the-owned-vm.patch' not in record.get('local_patches', {}):
            parser.error('--halt-poll-ns requires per-VM polling control in the research build')
    if args.kms_explicit_sync:
        record = json.loads((base/'build.json').read_text())
        if '0020-acknowledge-real-host-scanout-completion.patch' not in record.get('local_patches', {}):
            parser.error('--kms-explicit-sync requires strict host scanout completion support')
    if args.sync_file_bridge:
        if args.backend != 'nvidia':
            parser.error('--sync-file-bridge requires NVIDIA')
        record = json.loads((base/'build.json').read_text())
        required = {'0013-experimental-native-sync-file-export-bridge.patch',
                    '0015-recognize-negotiated-fence-response-types.patch',
                    '0016-match-fence-monitor-wait-to-irq-wakeup.patch',
                    '0018-bridge-native-and-guest-sync-file-imports.patch'}
        if not required.issubset(record.get('local_patches', {})):
            parser.error('--sync-file-bridge requires the bridge completion, wakeup and import fixes')
    if args.poll_transport == 'enqueue':
        build_record = json.loads((base/'build.json').read_text())
        required = {'0010-enqueue-poll-registrations-without-a-helper-round-trip.patch',
                    '0011-keep-poll-flags-internal-and-check-backpressure.patch'}
        if not required.issubset(build_record.get('local_patches', {})):
            parser.error('--poll-transport enqueue requires the qualified enqueue patches in this build')
    if args.private_output:
        private_runtime = Path(os.environ.get('XDG_RUNTIME_DIR','/invalid'))
        if not (args.private_output=='HEADLESS-1' and private_runtime.name.startswith('legion-latency-')
                and private_runtime.parent==Path('/tmp') and private_runtime.is_dir()
                and private_runtime.stat().st_uid==os.getuid() and private_runtime.stat().st_mode&0o077==0
                and args.window and args.window_fullscreen):
            parser.error('--private-output requires an owned private headless qualification session')
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    shader_seed = copy_seed(args.shader_cache_seed.resolve(strict=True), output/'shader-cache-seed') if args.shader_cache_seed else None
    if shader_seed:
        (output/'shader-cache-seed.json').write_text(json.dumps(shader_seed,indent=2)+'\n')
    runtime = ROOT/'build/runtime'
    amd = args.backend == 'amdgpu-native'
    vendor = 0x1002 if amd else 0x10de
    node = '/dev/dri/renderD129' if amd else '/dev/dri/renderD128'
    expected_device = int((Path('/sys/class/drm')/Path(node).name/'device/device').read_text(), 16)
    driver = 'RADV DRM native context' if amd else subprocess.check_output(['nvidia-smi', '--query-gpu=driver_version',
                                     '--format=csv,noheader'], text=True).splitlines()[0].strip()
    bundle = output/'amd-driver' if amd else base/f'host-libs-{driver}'
    if amd:
        bundle.mkdir()
        shutil.copy2(args.amd_mesa.resolve(strict=True), bundle/'libvulkan_radeon.so')
        # Resolve guest driver dependencies in the portable build image, avoiding
        # workstation libraries which may require CPU features hidden in guests.
        script = "import sys,shutil;sys.path.insert(0,'/src/tools');from package_runtime import libraries;[(shutil.copy2(path,'/bundle/'+name)) for name,path in libraries(['/driver/libvulkan_radeon.so']).items()]"
        subprocess.run(['podman', 'run', '--rm', '--userns=keep-id', *container_security('podman'),
                        '--user', f'{os.getuid()}:{os.getgid()}', '-v', f'{ROOT}:/src:ro',
                        '-v', f'{bundle}:/bundle:rw', '-v', f'{args.amd_mesa.resolve().parent}:/driver:ro',
                        'localhost/legion-build:1', 'python3', '-c', script], check=True)
        manifests = sorted((args.amd_mesa.parent.parent.parent/'share/vulkan/icd.d').glob('radeon_icd*.json'))
        manifests += sorted(args.amd_mesa.parent.glob('radeon_icd*.json'))
        if not manifests:
            raise ValueError('selected RADV build has no matching ICD manifest')
        icd = json.loads(manifests[0].read_text())
        icd['ICD']['library_path'] = '/nvidia/libvulkan_radeon.so'
        (bundle/'icd.json').write_text(json.dumps(icd, indent=2)+'\n')
    qemu = base/'qemu-11.1.2/build/qemu-system-x86_64'
    init = base/'probe-init'
    # Host static glibc can require AVX-512 (e.g. CachyOS v4); compile the
    # research init against baseline musl in the existing Legion build image.
    subprocess.run(['podman', 'run', '--rm', '--userns=keep-id', *container_security('podman'),
                    '--user', f'{os.getuid()}:{os.getgid()}', '-v', f'{ROOT}:/src:ro',
                    '-v', f'{base}:/research:rw', 'localhost/legion-build:1',
                    'musl-gcc', '-static', '-O2', '-Wall', '-Wextra', '-Werror',
                    '/src/tools/nvkvm_probe_init.c', '-o', '/research/probe-init'], check=True)
    source = base/('patched-source' if (base/'patched-source').is_dir() else 'source')
    modules = [runtime/'guest/modules/001.ko', runtime/'guest/modules/002.ko',
               source/'src/guest/nvkvm-guest.ko']
    if amd:
        modules = [runtime/'guest/modules'/f'{i:03d}.ko' for i in range(4)]
    if args.ntsync:
        modules.append(runtime/'guest/modules/005.ko')
    entries = [('init', 0o100755, init.read_bytes())]
    entries += [(f'modules/{i}.ko', 0o100600, path.read_bytes())
                for i, path in enumerate(modules)]
    module_parameters = []
    if args.kms_hz is not None:
        module_parameters.append(f'kms_hz={args.kms_hz}')
    if args.ring_idle_iterations is not None:
        module_parameters.append(f'ring_idle_us={args.ring_idle_iterations}')
    if args.poll_arm == 'async':
        module_parameters.append('poll_async=1')
    if args.kms_refresh == 'kms' and not amd:
        module_parameters.append('kms_normalize_refresh=1')
    if module_parameters:
        entries.append(('modules/2.params', 0o100600, ' '.join(module_parameters).encode()))
    initramfs(output/'initramfs.cpio', entries)
    config = json.loads((args.config or runtime/'guest/workload.json').read_text())
    if not args.config:
        config.update(warmup_ms=500, sample_ms=3000)
    (output/'config.json').write_text(json.dumps(config, indent=2)+'\n')
    guest_script = '''set -eu
echo LEGION_NVKVM_GUEST_READY
ls -l /dev/nvidia* /dev/dri/*
cat /proc/driver/nvidia/version
timeout 30 /usr/bin/vulkaninfo --summary > /research/vulkaninfo.stdout 2> /research/vulkaninfo.stderr
cat /research/vulkaninfo.stdout
timeout 30 /research/workload /research/config.json /research/workload-result.json > /research/workload.stdout 2> /research/workload.stderr
cat /research/workload.stdout
'''
    if args.vulkan_details:
        guest_script += 'timeout 30 /usr/bin/vulkaninfo > /research/vulkaninfo-details.stdout 2> /research/vulkaninfo-details.stderr\n'
    if amd:
        guest_script = guest_script.replace('ls -l /dev/nvidia* /dev/dri/*\ncat /proc/driver/nvidia/version', 'ls -l /dev/dri/*')
    (output/'guest.sh').write_text(guest_script)
    if args.ioctl_profile:
        shutil.copy2(args.ioctl_profile.resolve(strict=True), output/'ioctl-profile.so')
        text = (output/'guest.sh').read_text()
        text = text.replace('timeout 30 /research/workload ',
                            'timeout 30 env LD_PRELOAD=/research/ioctl-profile.so LEGION_IOCTL_PROFILE=/research/ioctl-profile.json /research/workload ')
        (output/'guest.sh').write_text(text)
    # Share only the owned probe and this run's writable output, never the repository/home.
    shutil.copy2(ROOT/'build/target/release/legion-workload-probe', output/'workload')
    if args.extra_probe:
        shutil.copy2(args.extra_probe.resolve(strict=True), output/'extra-probe')
        with (output/'guest.sh').open('a') as script:
            command = shlex.join(['/research/extra-probe', *args.extra_probe_argument])
            script.write('timeout 20 '+command+' > /research/extra-probe.stdout 2> /research/extra-probe.stderr\n')
    if args.presentation:
        if args.presentation == 'cube':
            compositor = base/'rust-target/release/legion-guest-compositor'
            shutil.copy2(compositor, output/'presentation')
            settings = dict(schema=1, client='cube', seconds=10, width=1920, height=1080,
                            drm_driver='virtio_gpu' if amd else 'nvidia-drm', diagnostic_shm=True, kms_wait=args.kms_wait, kms_refresh=args.kms_refresh,
                            explicit_sync=args.kms_explicit_sync, validate_primary_scaling=not amd and (args.fixture_resize or args.fixture_start_scaled))
            (output/'kms-presentation.json').write_text(json.dumps(settings)+'\n')
            # The compositor requires a prepared initramfs, not an ordinary host session.
            entries.append(('modules/load-order', 0o100600, b'0.ko\n1.ko\n2.ko\n'))
            initramfs(output/'initramfs.cpio', entries)
            command = '/research/presentation --proton-forward'
        else:
            program = 'gbmgl_present' if args.presentation == 'egl' else 'gbmflip'
            shutil.copy2(base/'fixtures'/program, output/'presentation')
            command = '/research/presentation /dev/dri/card0 180'
        with (output/'guest.sh').open('a') as script:
            script.write('''
touch /research/presentation-started
timeout 25 '''+command+''' > /research/presentation.log 2>&1
sleep 4
''')
    if args.proton:
        if args.application_dir:
            shutil.copy2(ROOT/'tools/ue5_frame_log.py', output/'wait-frame-log.py')
            subprocess.run(['cp', '-a', '--reflink=auto', str(args.application_dir.resolve(strict=True)), str(output/'application')], check=True)
            (output/'application-enabled').touch()
            for name in ['BenchmarkResults.txt', 'BenchmarkResultsPerFrame.csv']:
                (output/'application'/name).unlink(missing_ok=True)
            job = ['/application/'+args.executable, *args.argument]
            (output/'application-command.json').write_text(json.dumps(job)+'\n')
            (output/'run-application.py').write_text("import json,os,sys\nos.chdir('/application')\nargs=json.load(open('/research/application-command.json'))\nphase='getcompatpath' if len(sys.argv)>1 else 'runinprefix'\nif phase=='getcompatpath':args=args[:1]\nos.execv('/usr/bin/python3',['python3','/proton/proton',phase,*args])\n")
            if args.application_storage == 'tmpfs':
                (output/'application-ram-enabled').touch()
                with (output/'guest.sh').open('a') as script:
                    script.write('cp -a /application-source/. /application/\n')
        else:
            shutil.copy2(args.fixture.resolve(strict=True), output/'fixture.exe')
        (output/'proton-enabled').touch()
        fixture_config = dict(schema=1, width=256, height=256, warmup_ms=200, sample_ms=1000,
                              cpu_rounds=25000, gpu_clears=1, gpu_timestamps=True,
                              pipeline=dict(shader_rounds=1, draws_per_frame=1))
        if args.proton_window:
            fixture_config.update(width=1920, height=1080, presentation=dict(seconds=15, resize=False, fragment_iterations=args.fixture_fragment_iterations))
            if args.fixture_start_scaled:
                fixture_config.update(width=1440,height=810)
            if args.fixture_resize:
                fixture_config['presentation'].update(resize=True, resize_cycles=args.fixture_resize_cycles, seconds=30 if args.fixture_resize_cycles>1 else 15)
            if args.latency_probe:
                fixture_config['presentation'].update(seconds=25, latency_probe=True, latency_delay_ms=args.latency_delay_ms)
                if args.runtime_feedback:fixture_config['presentation']['runtime_feedback']=True
                if args.fixture_frame_tags:fixture_config['presentation']['frame_tags']=True
            fixture_config.pop('pipeline')
            shutil.copy2(base/'rust-target/release/legion-guest-compositor', output/'compositor')
            settings = dict(schema=1, client='external', seconds=15, width=1920, height=1080,
                            drm_driver='virtio_gpu' if amd else 'nvidia-drm',
                            application=bool(args.application_dir), diagnostic_shm=args.surface_diagnostics=='on', kms_wait=args.kms_wait, kms_refresh=args.kms_refresh,
                            explicit_sync=args.kms_explicit_sync, validate_primary_scaling=not amd and (args.fixture_resize or args.fixture_start_scaled))
            settings['kms_acquire_wake'] = args.kms_acquire_wake
            if args.latency_probe:
                settings['seconds'] = 25
            (output/'kms-presentation.json').write_text(json.dumps(settings)+'\n')
            entries.append(('modules/load-order', 0o100600, b'0.ko\n1.ko\n2.ko\n'))
            initramfs(output/'initramfs.cpio', entries)
        (output/'fixture-config.json').write_text(json.dumps(fixture_config, indent=2)+'\n')
        with (output/'guest.sh').open('a') as script:
            guest_script = r'''
mv /research/workload-result.json /research/vulkan-workload-result.json
export STEAM_COMPAT_DATA_PATH=/tmp/compatdata
export STEAM_COMPAT_CLIENT_INSTALL_PATH=/tmp/steam
export STEAM_COMPAT_INSTALL_PATH=/research
export SteamAppId=0 SteamGameId=0 WINEDEBUG=-all PROTON_ENABLE_WAYLAND=1
export WINEDLLOVERRIDES='dxgi,d3d11,d3d12,d3d12core=n;lsteamclient=d;wineopenxr=d'
export LEGION_WORKLOAD_DIRECTORY='Z:\research'
export VKD3D_SHADER_CACHE_PATH=/tmp/cache DXVK_LOG_LEVEL=info VKD3D_DEBUG=info
export XDG_CACHE_HOME=/tmp/cache __GL_SHADER_DISK_CACHE_PATH=/tmp/cache/nvidia
mkdir -p /tmp/compatdata /tmp/steam /tmp/cache/nvidia
COMPOSITOR > /research/wayland.log 2>&1 &
compositor_pid=$!
for attempt in {1..100}; do
    test -S /run/legion-proton-0 && break
    sleep 0.05
done
export WAYLAND_DISPLAY=legion-proton-0
timeout 30 python3 /proton/proton getcompatpath /research/fixture.exe > /research/prefix.log 2>&1
timeout 60 python3 /proton/proton runinprefix /research/fixture.exe > /research/proton.log 2>&1
'''
            compositor = '/research/compositor --proton-forward' if args.proton_window else '/usr/share/legion/compositor --offscreen-protocol-server'
            if args.proton_window:
                guest_script = guest_script.replace('COMPOSITOR', 'touch /research/presentation-started\n'+compositor)
                guest_script += 'touch /research/proton-client-exited\nwait "$compositor_pid"\n'
            else:
                guest_script = guest_script.replace('COMPOSITOR', compositor)
            if args.application_dir:
                guest_script = guest_script.replace('python3 /proton/proton getcompatpath /research/fixture.exe', 'python3 /research/run-application.py getcompatpath')
                guest_script = guest_script.replace('timeout 60 python3 /proton/proton runinprefix /research/fixture.exe > /research/proton.log 2>&1',
                    'timeout '+str(args.timeout)+' python3 /research/run-application.py > /research/proton.log 2>&1 &\napplication_pid=$!\n'
                    'while kill -0 "$application_pid" 2>/dev/null; do\n'
                    '    if test -s /application/BenchmarkResults.txt; then\n'
                    '        python3 /research/wait-frame-log.py /application /research > /research/frame-log.json\n'
                    '        WINEPREFIX=/tmp/compatdata/pfx /proton/files/bin/wineserver -k\n'
                    '        break\n    fi\n    sleep 1\ndone\n'
                    'wait "$application_pid" || test -s /application/BenchmarkResults.txt')
                if args.application_storage == 'tmpfs':
                    guest_script += ('cp /application/BenchmarkResults.txt /research/application/\n'
                                     'cp /application/BenchmarkResultsPerFrame.csv /research/application/\n')
            if args.swapchain_latency_frames is not None:
                guest_script = f'export VKD3D_SWAPCHAIN_LATENCY_FRAMES={args.swapchain_latency_frames}\n'+guest_script
            if args.application_final_capture:
                guest_script = guest_script.replace(
                    '        WINEPREFIX=/tmp/compatdata/pfx /proton/files/bin/wineserver -k',
                    '        touch /research/application-result-ready\n'
                    '        capture_wait=0\n'
                    '        while test "$capture_wait" -lt 150; do\n'
                    '            test -f /research/application-final-capture-done && break\n'
                    '            capture_wait=$((capture_wait + 1))\n'
                    '            sleep 0.1\n'
                    '        done\n'
                    '        test -f /research/application-final-capture-done || exit 1\n'
                    '        WINEPREFIX=/tmp/compatdata/pfx /proton/files/bin/wineserver -k')
            if args.queue_profile:
                # Trace to guest RAM so trace writes do not exercise virtiofs.
                guest_script = "export VKD3D_QUEUE_PROFILE='Z:\\tmp\\queue-profile.json'\n"+guest_script
                guest_script += 'cp /tmp/queue-profile.json /research/queue-profile.json\n'
            if shader_seed:
                guest_script = guest_script.replace(
                    'mkdir -p /tmp/compatdata /tmp/steam /tmp/cache/nvidia',
                    'mkdir -p /tmp/compatdata /tmp/steam /tmp/cache/nvidia\n'
                    'cp -R /research/shader-cache-seed/. /tmp/cache/')
            if args.preserve_shader_cache:
                guest_script += ('mkdir -p /research/shader-cache/nvidia\n'
                    'cp -R /tmp/cache/nvidia/. /research/shader-cache/nvidia/\n'
                    'for shader_file in /tmp/cache/vkd3d-proton.*; do\n'
                    '    if test -f "$shader_file"; then cp "$shader_file" /research/shader-cache/; fi\n'
                    'done\ntouch /research/shader-cache-exported\n')
            script.write(guest_script)
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith(('VK_', 'VN_', 'NVKVM_', 'LEGION_'))
                   and key not in ('LD_PRELOAD', 'LD_LIBRARY_PATH', 'GBM_BACKEND', 'GBM_BACKENDS_PATH',
                                   '__EGL_VENDOR_LIBRARY_FILENAMES', '__GLX_VENDOR_LIBRARY_NAME')}
    if args.sdl_frame_input:
        record = json.loads((base/'build.json').read_text())
        if '0006-sdl-service-input-on-active-scanout.patch' not in record.get('qemu_patches', {}):
            raise ValueError('SDL frame input requires the qualified experimental QEMU patch')
        environment['LEGION_SDL_FRAME_INPUT'] = '1'
    if args.poll_transport == 'enqueue':
        environment['NVKVM_POLL_ENQUEUE'] = '1'
    if amd:
        environment.update(GBM_BACKEND='dri', GBM_BACKENDS_PATH='/usr/lib/gbm',
                           __EGL_VENDOR_LIBRARY_FILENAMES='/usr/share/glvnd/egl_vendor.d/50_mesa.json')
        pci = (Path('/sys/class/drm')/Path(node).name/'device').resolve().name
        environment['DRI_PRIME'] = 'pci-'+pci.replace(':', '_').replace('.', '_')
    if args.backend_debug and not amd:
        environment['NVKVM_DEBUG'] = '1'
    if args.present_timing and not amd:
        environment['NVKVM_PRESENT_TIMING'] = '1'
    if args.request_stats and not amd:
        environment['NVKVM_REQUEST_STATS'] = '1'
    if args.virtual_refresh_hz is not None:
        environment['LEGION_VIRTUAL_REFRESH_HZ'] = str(args.virtual_refresh_hz)
    if args.scanout_external:
        environment['LEGION_SCANOUT_EXTERNAL'] = '1'
    if args.host_ui_vendor == 'nvidia':
        environment.update(__EGL_VENDOR_LIBRARY_FILENAMES='/usr/share/glvnd/egl_vendor.d/10_nvidia.json',
                           GBM_BACKEND='nvidia-drm')
        environment.pop('DRI_PRIME', None)
    children = []
    logs = []
    ram_fd = None
    start = time.monotonic()
    try:
        with tempfile.TemporaryDirectory(prefix='legion-nvkvm-') as temporary:
            cpu_model = CPU_MODEL if args.cpu_policy == 'restricted' else 'host'
            cpuinfo = Path('/proc/cpuinfo').read_text()
            if 'AuthenticAMD' in cpuinfo and 'topoext' in cpuinfo:
                cpu_model += ',+topoext'
            command = [str(qemu), '-machine', 'q35,accel=kvm,smm=off,memory-backend=ram',
                       '-cpu', cpu_model, '-smp', f'cpus={args.cpus},sockets=1,cores={args.cpus//args.threads_per_core},threads={args.threads_per_core}',
                       '-m', str(args.memory_mib), '-object',
                       f'memory-backend-memfd,id=ram,size={args.memory_mib}M,share=on',
                       '-nodefaults', '-no-user-config', '-display',
                       'sdl,gl='+args.sdl_gl+(',full-screen=on' if args.window_fullscreen else '') if args.window else (f'egl-headless,rendernode={node}' if args.presentation or args.proton_window or amd else 'none'),
                       '-serial', 'stdio', '-monitor', 'none', '-no-reboot',
                       '-device', 'virtio-nvgpu-pci-non-transitional,id=nvkvm0',
                       '-device', 'nvkvm-gpu,addr=7', '-kernel', str(runtime/'guest/kernel'),
                       '-initrd', str(output/'initramfs.cpio'), '-append',
                       'console=ttyS0 panic=-1 rdinit=/init loglevel=5']
            if args.sync_file_bridge:
                position = command.index('virtio-nvgpu-pci-non-transitional,id=nvkvm0')
                command[position] += ',sync-file-bridge=on'
                if args.kms_explicit_sync:
                    command[position] += ',strict-scanout-release=on'
                    if args.host_display == 'dbus':
                        command[position] += ',display-read-ack=on'
            if args.halt_poll_ns is not None:
                position = next(i for i, value in enumerate(command)
                                if value.startswith('virtio-nvgpu-pci-non-transitional,id=nvkvm0'))
                command[position] += ',halt-poll-ns='+str(args.halt_poll_ns)
            if args.ram_layout == 'aligned-memfd':
                # Match src/memory.rs: anonymous, shared, size-sealed RAM with
                # explicit alignment and a parent-owned proc-fd path. No RAM
                # descriptor is inherited by the VM or its GPU helpers.
                ram_fd = os.memfd_create('legion-aligned-ram', os.MFD_CLOEXEC | os.MFD_ALLOW_SEALING)
                os.ftruncate(ram_fd, args.memory_mib*1024*1024)
                fcntl.fcntl(ram_fd, fcntl.F_ADD_SEALS,
                            fcntl.F_SEAL_GROW | fcntl.F_SEAL_SHRINK | fcntl.F_SEAL_SEAL)
                command[command.index('-object')+1] = (
                    f'memory-backend-file,id=ram,size={args.memory_mib}M,share=on,'
                    f'mem-path=/proc/{os.getpid()}/fd/{ram_fd},align=2097152')
            if amd:
                position = command.index('virtio-nvgpu-pci-non-transitional,id=nvkvm0')
                command[position] = 'virtio-gpu-gl-pci,hostmem=1G,blob=on,drm_native_context=on,venus=off'
                position = command.index('nvkvm-gpu,addr=7')
                del command[position-1:position+1]
                loader = runtime/'host/lib/ld-linux-x86-64.so.2'
                search = str(args.amd_renderer.resolve(strict=True).parent)+':'+str(runtime/'host/lib')
                command = [str(loader), '--library-path', search, str(runtime/'libexec/qemu-system-x86_64'),
                           '-L', str(runtime/'share/qemu'), *command[1:]]
            qmp = Path(temporary)/'qmp.sock'
            title = f'legion-nvkvm-probe-{os.getpid()}'
            capture_title = f'QEMU ({title}-0)'
            viewer_child = None
            if args.host_display == 'dbus':
                if not json.loads((base/'build.json').read_text()).get('dbus_display'):
                    raise ValueError('D-Bus display requires a --dbus-display research build')
                viewer = ROOT/'tools/qemu-display-probe/target/release/legion-qemu-display-probe'
                if not viewer.is_file():
                    raise ValueError('build tools/qemu-display-probe with cargo build --locked --release first')
                bus_socket = Path(temporary)/'display-bus.sock'
                bus_address = 'unix:path='+str(bus_socket)
                bus_log = (output/'display-bus.log').open('w')
                logs.append(bus_log)
                bus = subprocess.Popen(['dbus-daemon', '--session', '--nofork', '--nopidfile',
                                        '--address='+bus_address], stdout=bus_log,
                                       stderr=subprocess.STDOUT, start_new_session=True)
                children.append(bus)
                bus_deadline = time.monotonic()+5
                while not bus_socket.exists():
                    if bus.poll() is not None or time.monotonic() > bus_deadline:
                        raise RuntimeError('owned display bus failed; inspect display-bus.log')
                    time.sleep(.02)
                capture_title = f'Legion D-Bus ({title})'
                viewer_command = [str(viewer), '--address', bus_address, '--title', capture_title,
                                  '--report', str(output/'viewer-result.json'), '--qmp', str(qmp)]
                if args.window_fullscreen:
                    viewer_command.append('--fullscreen')
                if args.input_check:
                    viewer_command.append('--inject-input')
                if args.viewer_fail_read_after is not None:
                    viewer_command += ['--fail-read-after', str(args.viewer_fail_read_after)]
                if args.viewer_early_swap:viewer_command+=['--early-swap']
                if args.viewer_gpu_timing:viewer_command+=['--gpu-timing']
                if args.viewer_stage_profile:viewer_command+=['--stage-profile',str(output/'viewer-stages.jsonl')]
                if args.viewer_fence_wait_us!=1000:viewer_command+=['--fence-wait-us',str(args.viewer_fence_wait_us)]
                if args.viewer_gpu_priority!='default':viewer_command+=['--gpu-priority',args.viewer_gpu_priority]
                if args.viewer_frame_tags:viewer_command+=['--frame-tags']
                if args.viewer_presentation:
                    viewer_command+=['--presentation-report',str(output/'host-presentation.jsonl'),'--presentation-delay-ms',str(args.viewer_presentation_delay_ms)]
                (output/'viewer-command.json').write_text(json.dumps(viewer_command, indent=2)+'\n')
                command[command.index('-display')+1] = f'dbus,gl=on,rendernode={node},addr={bus_address}'
            if args.host_display == 'broker':
                capture_title = f'Legion ({title})'
                broker_socket = Path(temporary)/'broker.sock'
                broker = source/'src/broker/nvkvm-display-broker'
                broker_log = (output/'broker.log').open('w')
                logs.append(broker_log)
                broker_command = [str(broker), '--socket', str(broker_socket), '--backend', 'wayland',
                                  '--title', capture_title, '--size', '1920x1080', '--resolution', '1920x1080',
                                  '--refresh', str(args.kms_hz) if args.kms_hz else 'auto',
                                  '--present-mode=native', '--clipboard', 'off']
                if args.window_fullscreen:
                    broker_command.append('--fullscreen')
                (output/'broker-command.json').write_text(json.dumps(broker_command, indent=2)+'\n')
                broker_child = subprocess.Popen(broker_command, env=environment, stdout=broker_log,
                                                stderr=subprocess.STDOUT, start_new_session=True)
                children.append(broker_child)
                broker_deadline = time.monotonic()+10
                while not broker_socket.exists():
                    if broker_child.poll() is not None or time.monotonic() >= broker_deadline:
                        raise RuntimeError('owned display broker failed; inspect broker.log')
                    time.sleep(.02)
                command[command.index('-display')+1] = f'nvkvm-broker,socket={broker_socket}'
            command += ['-name', title, '-qmp', f'unix:{qmp},server=on,wait=off']
            if args.window or args.input_check:
                command += ['-device', 'virtio-keyboard-pci', '-device', 'virtio-tablet-pci']
                if args.host_display == 'dbus':
                    command += ['-device', 'virtio-mouse-pci']
                if args.window:
                    environment['SDL_VIDEODRIVER'] = 'wayland'
                    environment['LEGION_DESKTOP_INPUT'] = '1'
                if args.window and not amd:
                    environment['__EGL_VENDOR_LIBRARY_FILENAMES'] = '/usr/share/glvnd/egl_vendor.d/10_nvidia.json'
            shares = [
                ('usr', runtime/'guest/usr', True), ('abi', runtime/'host/lib', True),
                ('nvidia', bundle, True), ('research', output, False)]
            if args.proton:
                shares.append(('proton', args.proton.resolve(strict=True), True))
            if args.application_dir:
                shares.append(('application', output/'application', False))
            for tag, directory, readonly in shares:
                sock = Path(temporary)/(tag+'.sock')
                log = (output/f'virtiofs-{tag}.log').open('w')
                logs.append(log)
                service = [str(runtime/'bin/legion-virtiofsd'), '--shared-dir', str(directory),
                           '--socket-path', str(sock), '--sandbox', 'namespace']
                if readonly:
                    service.append('--readonly')
                child = subprocess.Popen(service, env=environment, stdout=log,
                                         stderr=subprocess.STDOUT, start_new_session=True)
                children.append(child)
                deadline = time.monotonic()+10
                while not sock.exists():
                    if child.poll() is not None or time.monotonic() > deadline:
                        raise RuntimeError(f'{tag} virtiofs failed; inspect its log')
                    time.sleep(.02)
                command += ['-chardev', f'socket,id=fs-{tag},path={sock}', '-device',
                            f'vhost-user-fs-pci,chardev=fs-{tag},tag={tag}']
            (output/'command.json').write_text(json.dumps(command, indent=2)+'\n')
            launch_command = command
            if args.host_thp == 'off':
                launch_command = [str(ROOT/'build/dist/legion'), 'internal-qemu-exec', '--', *command]
            if args.profile_host:
                launch_command = ['perf', 'record', '-e', 'cycles:uH', '-F', '199',
                                  '-o', str(output/'perf-host.data'), '--', *launch_command]
                (output/'profiler-command.json').write_text(json.dumps(launch_command, indent=2)+'\n')
            if args.debug_qemu:
                launch_command = ['gdb', '--batch', '-ex', 'set pagination off',
                                  '-ex', 'handle SIGUSR1 nostop noprint pass',
                                  '-ex', 'run', '-ex', 'bt', '--args', *launch_command]
            (output/'launch-command.json').write_text(json.dumps(launch_command, indent=2)+'\n')
            with (output/'console.log').open('w') as log:
                usage_before = resource.getrusage(resource.RUSAGE_CHILDREN)
                cpu_measure_start = time.monotonic()
                child = subprocess.Popen(launch_command, env=environment, stdout=log,
                                         stderr=subprocess.STDOUT, start_new_session=True)
                children.append(child)
                qemu_pid = child.pid
                if args.host_display == 'dbus':
                    viewer_log = (output/'viewer.log').open('w')
                    logs.append(viewer_log)
                    viewer_environment = dict(environment, WAYLAND_DEBUG='client') if args.viewer_presentation else dict(environment)
                    if args.viewer_gpu_priority!='default':
                        viewer_environment.update(SDL_VIDEO_EGL_DRIVER=str(args.viewer_priority_shim.resolve(strict=True)),LEGION_VIEWER_GPU_PRIORITY=args.viewer_gpu_priority)
                    viewer_child = subprocess.Popen(viewer_command, env=viewer_environment, stdout=viewer_log,
                                                    stderr=subprocess.STDOUT, start_new_session=True)
                    children.append(viewer_child)
                affinities = None
                if args.pin_vcpus:
                    pin_deadline = time.monotonic()+10
                    while not qmp.exists():
                        if child.poll() is not None or time.monotonic() >= pin_deadline:
                            raise RuntimeError('owned VM ended before its QMP socket was ready')
                        time.sleep(.01)
                    affinities = pin_vcpus(qmp, qemu_pid, args.cpus, args.threads_per_core)
                    (output/'vcpu-affinity.json').write_text(json.dumps(affinities, indent=2)+'\n')
                if args.telemetry:
                    gpu_log = (output/'gpu-telemetry.csv').open('w')
                    logs.append(gpu_log)
                    children.append(subprocess.Popen(['nvidia-smi', '--query-gpu=timestamp,utilization.gpu,utilization.memory,clocks.sm,clocks.mem,power.draw,temperature.gpu',
                                                       '--format=csv,nounits', '--loop-ms=1000'],
                                                      stdout=gpu_log, stderr=subprocess.STDOUT, start_new_session=True))
                thread_samples = []
                last_thread_sample = 0
                budget = args.timeout+60 if args.application_dir else (150 if args.proton else 100)
                deadline = time.monotonic()+budget
                last_capture = 0
                captures = []
                input_sent = False
                resized_input_sent = False
                resized_input_snapshot = None
                ram_snapshot = False
                while child.poll() is None:
                    if not ram_snapshot and not args.profile_host and (output/'kms-first-frame.json').exists():
                        try:
                            maps = Path(f'/proc/{qemu_pid}/smaps').read_text()
                            sections = re.split(r'(?=^[0-9a-f]+-[0-9a-f]+ )', maps, flags=re.M)
                            ram_maps = [section for section in sections if
                                        '/memfd:memory-backend-memfd ' in section or
                                        '/memfd:legion-aligned-ram ' in section]
                            (output/'host-ram-mappings.txt').write_text(''.join(ram_maps))
                            ram_snapshot = bool(ram_maps)
                        except (FileNotFoundError, ProcessLookupError):
                            pass
                    if args.telemetry and time.monotonic()-last_thread_sample >= 1:
                        last_thread_sample = time.monotonic()
                        threads = []
                        for stat in Path(f'/proc/{qemu_pid}/task').glob('*/stat'):
                            try:
                                value = stat.read_text()
                                name = value[value.index('(')+1:value.rindex(')')]
                                fields = value[value.rindex(')')+2:].split()
                                threads.append(dict(tid=int(stat.parent.name), name=name,
                                                    cpu_ticks=int(fields[11])+int(fields[12])))
                            except (OSError, ValueError):
                                continue
                        thread_samples.append(dict(elapsed_seconds=last_thread_sample-start, threads=threads))
                    if time.monotonic() > deadline:
                        raise subprocess.TimeoutExpired(command, budget)
                    if viewer_child is not None and viewer_child.poll() is not None:
                        raise RuntimeError('owned D-Bus viewer ended before VM shutdown; inspect viewer.log')
                    if args.input_check and args.host_display != 'dbus' and not input_sent and (output/'kms-first-frame.json').exists():
                        inject_input(qmp)
                        input_sent = True
                    if args.fixture_resize and args.input_check and (output/'kms-diagnostics.json').exists():
                        try:
                            diagnostic = json.loads((output/'kms-diagnostics.json').read_text())
                            if diagnostic.get('source_transitions') != getattr(args,'_last_resize_diagnostic',None):
                                with (output/'resize-diagnostics.jsonl').open('a') as history:
                                    history.write(json.dumps(diagnostic)+'\n')
                                args._last_resize_diagnostic = diagnostic.get('source_transitions')
                            if diagnostic.get('source_size') == [1440,810] and diagnostic.get('input',{}).get('extent') == [1440,810]:
                                if not resized_input_sent:
                                    qmp_command(qmp, 'input-send-event', dict(events=[
                                        dict(type='abs', data=dict(axis='x',value=24575)),
                                        dict(type='abs', data=dict(axis='y',value=8192))]))
                                    resized_input_sent = True
                                elif diagnostic.get('input',{}).get('extent') == [1440,810]:
                                    x,y = diagnostic['input']['position']
                                    if abs(x-1080)<2 and abs(y-202.5)<2:
                                        resized_input_snapshot = diagnostic['input']
                        except (OSError, ValueError, KeyError):
                            pass
                    capture_ready = ((output/'kms-first-frame.json').exists() and not (output/'kms-result.json').exists()
                                     if args.presentation == 'cube' or args.proton_window
                                     else (output/'presentation-started').exists())
                    if args.application_final_capture:
                        capture_ready = ((output/'application-result-ready').exists()
                                         and not (output/'application-final-capture-done').exists())
                    if not args.latency_probe and (args.presentation or args.proton_window) and capture_ready and time.monotonic()-last_capture > (10 if args.application_dir else 1):
                        last_capture = time.monotonic()
                        path = output/f'frame-{len(captures):03}.png'
                        try:
                            capture(qmp, path)
                            captures.append(dict(path=str(path), source='qmp', pattern_verified=pattern_verified(path), nonuniform=nonuniform(path)))
                        except (OSError, RuntimeError, ValueError) as error:
                            captures.append(dict(error=str(error), source='qmp'))
                        if args.private_output:
                            host = output/f'private-{len(captures):03}.png'
                            subprocess.run(['grim','-o',args.private_output,str(host)],check=True,timeout=3,capture_output=True)
                            captures.append(dict(path=str(host), source='owned_wayland_window',
                                                 pattern_verified=pattern_verified(host),nonuniform=nonuniform(host),
                                                 scope='single owned fullscreen window in private headless output'))
                        helper = ROOT/'build/venus/capture-toplevel'
                        if args.window and not args.private_output and helper.is_file():
                            try:
                                identifier = ''
                                for suffix in ('', ' - Press Ctrl-Alt-G to exit grab'):
                                    found = subprocess.run([str(helper), capture_title+suffix],
                                                           text=True, timeout=3, capture_output=True)
                                    if found.returncode == 0:
                                        identifier = found.stdout.strip()
                                        break
                                if not identifier or '\n' in identifier:
                                    raise RuntimeError('owned VM toplevel is missing or ambiguous')
                                host = output/f'host-{len(captures):03}.png'
                                subprocess.run(['grim', '-T', identifier, str(host)], check=True, timeout=3, capture_output=True)
                                captures.append(dict(path=str(host), source='owned_wayland_window', pattern_verified=pattern_verified(host), nonuniform=nonuniform(host)))
                            except (OSError, RuntimeError, subprocess.SubprocessError) as error:
                                captures.append(dict(error=str(error), source='owned_wayland_window'))
                    if args.application_final_capture and any(item.get('source') == 'owned_wayland_window' and item.get('nonuniform') for item in captures):
                        (output/'application-final-capture-done').touch()
                    time.sleep(.1)
                exit_code = child.wait()
                if viewer_child is not None and exit_code != 0:
                    raise RuntimeError('owned D-Bus QEMU failed; inspect console.log')
                if viewer_child is not None:
                    if viewer_child.wait(timeout=5) != 0:
                        raise RuntimeError('D-Bus viewer failed; inspect viewer.log')
                usage_after = resource.getrusage(resource.RUSAGE_CHILDREN)
                host_cpu = dict(user_seconds=usage_after.ru_utime-usage_before.ru_utime,
                                system_seconds=usage_after.ru_stime-usage_before.ru_stime,
                                elapsed_seconds=time.monotonic()-cpu_measure_start,
                                scope='owned QEMU/viewer and reaped descendants during VM lifetime; optional capture helpers included')
            latency_observer = None
            if args.latency_probe:
                latency_observer = json.loads(args.latency_observer_report.read_text())
                if not latency_observer.get('passed') or latency_observer.get('sample_count', 0) < 20:
                    raise RuntimeError('independent latency observer did not verify the input-reactive image')
                captures.append(dict(source='owned_wayland_window', nonuniform=True, pattern_verified=True,
                                     verifier=latency_observer.get('capture_verifier','isolated compositor barcode capture'), report=str(args.latency_observer_report)))
                if args.telemetry:
                    (output/'thread-telemetry.json').write_text(json.dumps(dict(
                        ticks_per_second=os.sysconf('SC_CLK_TCK'), samples=thread_samples), indent=2)+'\n')
            result_path = output/('vulkan-workload-result.json' if args.proton else 'workload-result.json')
            result = json.loads(result_path.read_text()) if result_path.exists() else None
            summary = (output/'vulkaninfo.stdout').read_text() if (output/'vulkaninfo.stdout').exists() else ''
            vendors = re.findall(r'^\s*vendorID\s*=\s*(\S+)', summary, re.MULTILINE)
            devices = re.findall(r'^\s*deviceID\s*=\s*(\S+)', summary, re.MULTILINE)
            expected_driver = 'DRIVER_ID_MESA_RADV' if amd else 'DRIVER_ID_NVIDIA_PROPRIETARY'
            hardware = (vendors == [f'0x{vendor:04x}'] and devices == [f'0x{expected_device:04x}'] and expected_driver in summary
                        and result is not None and all(case.get('gpu', {}).get('vendor_id') == vendor
                                                      and case.get('gpu', {}).get('device_id') == expected_device
                                                      for case in result['cases']))
            passed = (exit_code == 0 and result is not None and result.get('gpu_verified') is True and hardware
                      and 'LEGION_NVKVM_DONE failed=0' in (output/'console.log').read_text())
            if args.host_thp == 'off':
                passed = passed and 'LEGION_QEMU_POLICY hugepages=off scope=qemu-process' in (output/'console.log').read_text()
            if args.halt_poll_ns is not None:
                passed = passed and f'LEGION_KVM_HALT_POLL max_ns={args.halt_poll_ns} scope=owned-vm' in (output/'console.log').read_text()
            if args.sdl_frame_input:
                passed = passed and 'LEGION_SDL_FRAME_INPUT enabled=1 scope=active-scanout' in (output/'console.log').read_text()
            viewer_result = None
            if args.host_display == 'dbus':
                viewer_result = json.loads((output/'viewer-result.json').read_text())
                passed = (passed and viewer_result.get('passed') is True
                          and viewer_result.get('gpu_completed_frames', 0) > 30
                          and viewer_result.get('cpu_frame_copies') == 0
                          and 'LEGION_DBUS_READ_COMPLETION negotiated=1' in (output/'console.log').read_text())
                if args.viewer_gpu_priority!='default':
                    requested={'medium':0x3102,'high':0x3101}[args.viewer_gpu_priority]
                    marker=f'LEGION_VIEWER_GPU_PRIORITY requested={requested:#x} assigned={requested:#x} valid=1 method=original-context'
                    passed=passed and marker in (output/'viewer.log').read_text()
                input_sent = bool(args.input_check and viewer_result.get('input_test_injected'))
                if args.viewer_presentation:
                    from audit_host_presentation import audit
                    host_presentation=audit([json.loads(line) for line in (output/'host-presentation.jsonl').read_text().splitlines()],(output/'viewer.log').read_text().splitlines())
                    (output/'host-presentation-audit.json').write_text(json.dumps(host_presentation,indent=2)+'\n')
                    viewer_result['host_presentation_audit']=host_presentation
                    passed=passed and host_presentation['passed']
                if args.viewer_stage_profile:
                    from analyze_viewer_stages import analyze
                    stages=analyze([json.loads(line) for line in (output/'viewer-stages.jsonl').read_text().splitlines()],
                                   viewer_result['gpu_completed_frames'])
                    (output/'viewer-stages-summary.json').write_text(json.dumps(stages,indent=2)+'\n')
                    viewer_result['cpu_stage_profile']=stages
                    passed=passed and stages['passed']
            proton_result = None
            if args.proton and (output/'workload-result.json').exists():
                proton_result = json.loads((output/'workload-result.json').read_text())
            if args.application_dir:
                snapshot_file = output/'frame-log.json'
                proton_result = json.loads(snapshot_file.read_text()) if snapshot_file.exists() else dict(passed=False)
                passed = passed and proton_result['passed']
            elif args.proton:
                passed = (passed and proton_result is not None and proton_result.get('passed') is True
                          and proton_result.get('gpu_verified') is True
                          and proton_result.get('vendor_id') == vendor)
            if args.presentation:
                presentation = (output/'presentation.log').read_text() if (output/'presentation.log').exists() else ''
                flips = re.search(r'RESULT flips_completed=(\d+)', presentation)
                if args.presentation == 'cube':
                    kms = json.loads((output/'kms-result.json').read_text()) if (output/'kms-result.json').exists() else {}
                    passed = (passed and kms.get('passed') is True and kms.get('completed_scanouts', 0) > 30
                              and any(item.get('nonuniform') for item in captures))
                else:
                    passed = passed and flips is not None and int(flips[1]) > 0
                if args.presentation == 'egl':
                    passed = passed and any(item.get('pattern_verified') for item in captures)
            if args.proton_window:
                kms = json.loads((output/'kms-result.json').read_text()) if (output/'kms-result.json').exists() else {}
                passed = (passed and kms.get('passed') is True and kms.get('completed_scanouts', 0) > 30
                          and any(item.get('nonuniform') for item in captures))
            resize_validation = None
            if args.fixture_resize:
                sizes = {(item['width'],item['height']) for item in kms.get('source_sizes',[])
                         if item.get('submissions',0)>30}
                checked_sizes = {(item['width'],item['height']) for item in (proton_result or {}).get('image_checks',[])}
                minimum = 2 if args.fixture_resize_cycles==1 else args.fixture_resize_cycles
                resize_validation = dict(passed=bool(
                    kms.get('source_transitions',0)>=minimum
                    and (proton_result or {}).get('resize_count',0)>=minimum
                    and {(1920,1080),(1440,810)}<=sizes
                    and {(1920,1080),(1440,810)}<=checked_sizes
                    and (not args.input_check or resized_input_snapshot is not None)),
                    resize_cycles_requested=args.fixture_resize_cycles,
                    source_transitions=kms.get('source_transitions'),
                    gpu_image_sizes=sorted(checked_sizes),
                    resized_input=resized_input_snapshot)
                passed = passed and resize_validation['passed']
            if args.window and (args.presentation == 'cube' or args.proton_window):
                passed = passed and any(item.get('nonuniform') and item.get('source') == 'owned_wayland_window'
                                        for item in captures)
            explicit = None
            if args.kms_explicit_sync:
                kms = json.loads((output/'kms-result.json').read_text()) if (output/'kms-result.json').exists() else {}
                synchronization = kms.get('synchronization', {})
                match = re.search(r'nvkvm strict releases: gpu=(\d+) unconsumed=(\d+) errors=(\d+)',
                                  (output/'console.log').read_text())
                explicit = dict(protocol_enabled=kms.get('explicit_sync', False), synchronization=synchronization,
                                gpu_completions=int(match[1]) if match else 0,
                                unconsumed=int(match[2]) if match else 0,
                                host_errors=int(match[3]) if match else None)
                explicit['unreleased_points'] = synchronization.get('commits', 0) - synchronization.get('released', 0)
                passed = (passed and explicit['protocol_enabled'] and synchronization.get('commits', 0) > 0
                          and synchronization.get('released', 0) > 0
                          and 0 <= explicit['unreleased_points'] <= 8
                          and explicit['gpu_completions'] > 0 and explicit['host_errors'] == 0)
            provenance = dict(init_source_sha256=hashlib.sha256((ROOT/'tools/nvkvm_probe_init.c').read_bytes()).hexdigest(),
                              fixture_sha256=hashlib.sha256((output/'workload').read_bytes()).hexdigest())
            provenance['guest_module_sha256'] = [hashlib.sha256(module.read_bytes()).hexdigest() for module in modules]
            if args.host_thp == 'off':
                provenance['qemu_exec_stage_sha256'] = hashlib.sha256((ROOT/'build/dist/legion').read_bytes()).hexdigest()
            if amd:
                provenance.update(guest_mesa_sha256=hashlib.sha256(args.amd_mesa.read_bytes()).hexdigest(),
                                  renderer_sha256=hashlib.sha256(args.amd_renderer.read_bytes()).hexdigest())
            elif (base/'build.json').exists():
                provenance['build'] = json.loads((base/'build.json').read_text())
            if args.proton:
                provenance['proton_path'] = str(args.proton.resolve())
                provenance['pe_fixture_sha256'] = hashlib.sha256((output/'application'/args.executable if args.application_dir else output/'fixture.exe').read_bytes()).hexdigest()
            pixels = bool(args.presentation in ('cube', 'egl') or args.proton_window) and passed
            input_delivery = None
            if args.input_check:
                diagnostics = json.loads((output/'kms-diagnostics.json').read_text()) if (output/'kms-diagnostics.json').exists() else {}
                input_delivery = dict(sent=input_sent, keyboard_events=diagnostics.get('keyboard_events', 0),
                                      pointer_events=diagnostics.get('pointer_events', 0),
                                      absolute_events=diagnostics.get('input', {}).get('absolute_events', 0))
                input_delivery['passed'] = (input_sent and input_delivery['keyboard_events'] >= 2
                                            and input_delivery['absolute_events'] >= 1)
                if args.host_display == 'dbus':
                    input_delivery['relative_events'] = diagnostics.get('input', {}).get('relative_events', 0)
                    input_delivery['passed'] = input_delivery['passed'] and input_delivery['relative_events'] >= 1
                passed = passed and input_delivery['passed']
            desktop = pixels and any(item.get('nonuniform') and item.get('source') == 'owned_wayland_window' for item in captures)
            scope = ('owned Vulkan/Proton presentation research' if pixels else
                     'KMS page-flip plumbing; pixels not verified' if args.presentation == 'gbm' else
                     'hardware Vulkan/Proton research; no game compatibility claim')
            report = dict(schema=1, passed=passed, scope=scope,
                          driver=driver, elapsed_seconds=time.monotonic()-start,
                          qemu_exit_code=exit_code, workload=result, proton=proton_result,
                          cpu_model=cpu_model, vcpus=args.cpus, provenance=provenance,
                          captures=captures, presentation_fixture=args.presentation, backend=args.backend,
                          presentation_verified=pixels, desktop_capture_verified=desktop, requested_kms_hz=args.kms_hz)
            report['input_delivery'] = input_delivery
            report['host_cpu'] = host_cpu
            report['sdl_frame_input'] = args.sdl_frame_input
            if args.application_dir:
                report['surface_diagnostics']=args.surface_diagnostics
                diagnostic_exists=(output/'kms-diagnostics.json').exists()
                report['surface_diagnostics_verified']=diagnostic_exists==(args.surface_diagnostics=='on')
                passed=passed and report['surface_diagnostics_verified']
                report['passed']=passed
            if args.kms_acquire_wake:
                kms_result=json.loads((output/'kms-result.json').read_text())
                report['kms_acquire_wake_verified']=kms_result.get('acquire_event_wait') is True
                passed=passed and report['kms_acquire_wake_verified']
                report['passed']=passed
            report['resize_validation'] = resize_validation
            if latency_observer is not None:
                report['latency_observer'] = latency_observer
            if viewer_result is not None:
                report['display_viewer'] = viewer_result
                provenance['display_viewer_sha256'] = hashlib.sha256(viewer.read_bytes()).hexdigest()
                if args.viewer_gpu_priority!='default':provenance['viewer_priority_shim_sha256']=hashlib.sha256(args.viewer_priority_shim.read_bytes()).hexdigest()
            report['threads_per_core'] = args.threads_per_core
            report['virtual_refresh_hz'] = args.virtual_refresh_hz
            report['kms_wait'] = args.kms_wait
            report['kms_refresh'] = args.kms_refresh
            report['explicit_sync'] = explicit
            report['vcpu_affinity'] = affinities
            report['ring_idle_iterations'] = args.ring_idle_iterations
            report['cpu_policy'] = args.cpu_policy
            report['window_fullscreen'] = args.window_fullscreen
            report['host_display'] = args.host_display
            report['host_profiled'] = args.profile_host
            report['request_stats'] = args.request_stats
            report['queue_profiled'] = args.queue_profile
            if args.queue_profile:
                report['queue_trace'] = normalize_trace(output/'queue-profile.json')
            report['poll_arm'] = args.poll_arm
            report['halt_poll_ns'] = args.halt_poll_ns
            report['poll_transport'] = args.poll_transport
            report['sync_file_bridge'] = args.sync_file_bridge
            report['ram_layout'] = args.ram_layout
            report['host_thp'] = args.host_thp
            report['application_storage'] = args.application_storage if args.application_dir else None
            if args.ioctl_profile:
                report['ioctl_profile_sha256'] = hashlib.sha256((output/'ioctl-profile.so').read_bytes()).hexdigest()
            report['ntsync_requested'] = args.ntsync
            if args.proton:
                startup_logs = ''.join((output/name).read_text() for name in ('prefix.log', 'proton.log'))
                report['ntsync_active'] = 'ntsync: up and running.' in startup_logs
                report['sync_backend'] = next((name for name in ('ntsync', 'fsync', 'esync')
                                               if name+': up and running.' in startup_logs), 'unknown')
                report['swapchain_latency_frames'] = args.swapchain_latency_frames
                report['application_final_capture'] = args.application_final_capture
                report.update(cache_metadata(shader_seed, startup_logs))
                if args.preserve_shader_cache:
                    exported=cache_manifest(output/'shader-cache')
                    report['shader_cache_export']=exported
                    passed=passed and exported['bytes']>0 and (output/'shader-cache-exported').exists()
                    report['passed']=passed
                if args.swapchain_latency_frames is not None:
                    activated = f'Ensure maximum latency of {args.swapchain_latency_frames} frames with KHR_present_wait.' in startup_logs
                    report['swapchain_latency_verified'] = activated
                    passed = passed and activated
                    report['passed'] = passed
                if args.ntsync:
                    passed = passed and report['ntsync_active']
                    report['passed'] = passed
            if args.host_display == 'broker':
                report['broker_sha256'] = hashlib.sha256(broker.read_bytes()).hexdigest()
            if args.extra_probe:
                report['extra_probe_sha256'] = hashlib.sha256((output/'extra-probe').read_bytes()).hexdigest()
                report['extra_probe_arguments'] = args.extra_probe_argument
            report.update(sdl_gl=args.sdl_gl if args.window else None, scanout_external=args.scanout_external)
            report['host_ui_vendor'] = args.host_ui_vendor or ('mesa' if amd else 'nvidia')
            (output/'report.json').write_text(json.dumps(report, indent=2)+'\n')
            print(json.dumps(dict(passed=passed, report=str(output/'report.json'))))
            return 0 if passed else 1
    except Exception as error:
        (output/'report.json').write_text(json.dumps(dict(schema=1, passed=False,
            error=str(error), error_type=type(error).__name__,
            elapsed_seconds=time.monotonic()-start), indent=2)+'\n')
        print(json.dumps(dict(passed=False, report=str(output/'report.json'), error=str(error))))
        return 1
    finally:
        for child in reversed(children):
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGTERM)
                try:
                    child.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    os.killpg(child.pid, signal.SIGKILL)
                    child.wait()
        for log in logs:
            log.close()
        if ram_fd is not None:
            os.close(ram_fd)


if __name__ == '__main__':
    raise SystemExit(main())
