#!/usr/bin/env python3
"""Pair an owned UE5 benchmark's native and nvkvm runs with fresh Proton prefixes."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import shutil
import statistics
import subprocess
import time
import tempfile
from ue5_frame_log import collect_benchmark_result
from vkd3d_queue_profile import normalize_trace
from shader_cache import copy_seed, export_gpu_cache, cache_metadata

ROOT = Path(__file__).resolve().parents[1]
ARGUMENTS = ['-d3d12', '-ResX=1920', '-ResY=1080', '-fullscreen', '-ForceRes', '-NoVSync',
             '-ExecCmds=r.VSync 0,r.ScreenPercentage 100,r.DynamicRes.OperationMode 0,t.MaxFPS 0']


def native(proton, source, output, fsync=False, queue_profile=False, swapchain_latency_frames=None,
           shader_cache_seed=None, preserve_shader_cache=False):
    output.mkdir()
    application = output/'application'
    subprocess.run(['cp', '-a', '--reflink=auto', source, application], check=True)
    for name in ('BenchmarkResults.txt', 'BenchmarkResultsPerFrame.csv'):
        (application/name).unlink(missing_ok=True)
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith(('VK_', 'VN_', 'NVKVM_', 'LEGION_', 'DXVK_', 'VKD3D_',
                                          'WINE', 'PROTON_', 'MANGOHUD', 'STEAM_COMPAT_'))
                   and key not in ('LD_PRELOAD', 'LD_LIBRARY_PATH', 'DRI_PRIME', 'GBM_BACKEND',
                                   'GBM_BACKENDS_PATH', '__EGL_VENDOR_LIBRARY_FILENAMES')}
    environment.update(STEAM_COMPAT_DATA_PATH=str(output/'compatdata'),
                       STEAM_COMPAT_CLIENT_INSTALL_PATH=str(output/'steam'),
                       STEAM_COMPAT_INSTALL_PATH=str(application), SteamAppId='0', SteamGameId='0',
                       PROTON_ENABLE_WAYLAND='1', WINEDEBUG='-all',
                       WINEDLLOVERRIDES='dxgi,d3d11,d3d12,d3d12core=n;lsteamclient=d;wineopenxr=d',
                       VKD3D_SHADER_CACHE_PATH=str(output/'cache'), VKD3D_DEBUG='info', DXVK_LOG_LEVEL='info',
                       XDG_CACHE_HOME=str(output/'cache'), __GL_SHADER_DISK_CACHE_PATH=str(output/'cache/nvidia'),
                       VK_DRIVER_FILES='/usr/share/vulkan/icd.d/nvidia_icd.json',
                       VK_LOADER_LAYERS_DISABLE='~implicit~',
                       __EGL_VENDOR_LIBRARY_FILENAMES='/usr/share/glvnd/egl_vendor.d/10_nvidia.json')
    if fsync:
        environment['PROTON_NO_NTSYNC'] = '1'
    if swapchain_latency_frames is not None:
        environment['VKD3D_SWAPCHAIN_LATENCY_FRAMES'] = str(swapchain_latency_frames)
    trace_directory = None
    if queue_profile:
        trace_directory = tempfile.TemporaryDirectory(prefix='legion-queue-', dir='/dev/shm')
        environment['VKD3D_QUEUE_PROFILE'] = 'Z:'+str(Path(trace_directory.name)/'queue-profile.json').replace('/', '\\')
    for name in ('compatdata', 'steam', 'cache'):
        (output/name).mkdir()
    (output/'cache/nvidia').mkdir()
    seed = copy_seed(shader_cache_seed, output/'cache') if shader_cache_seed else None
    executable = str(application/'Benchmark.exe')
    command = ['python3', str(proton/'proton'), 'runinprefix', executable, *ARGUMENTS]
    (output/'command.json').write_text(json.dumps(command, indent=2)+'\n')
    start = time.monotonic()
    child = None
    try:
        with (output/'prefix.log').open('w') as log:
            subprocess.run(['python3', str(proton/'proton'), 'getcompatpath', executable],
                           env=environment, stdout=log, stderr=subprocess.STDOUT, check=True, timeout=60)
        with (output/'proton.log').open('w') as log:
            child = subprocess.Popen(command, env=environment, cwd=application,
                                     stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            deadline = time.monotonic()+240
            while not (application/'BenchmarkResults.txt').exists():
                if child.poll() is not None or time.monotonic() > deadline:
                    raise RuntimeError('native benchmark ended or timed out without a result')
                time.sleep(.2)
        snapshot = collect_benchmark_result(application, output)
        score = snapshot['benchmark_fps']
        if not any('NVIDIA' in line and 'D3D12 - 1920x1080' in line for line in snapshot['details']):
            raise RuntimeError('native benchmark returned the wrong GPU/API/resolution')
        report = dict(**snapshot,
                      queue_profiled=queue_profile,
                      elapsed_seconds=time.monotonic()-start)
        startup_logs = ''.join((output/name).read_text() for name in ('prefix.log', 'proton.log'))
        report['sync_backend'] = next((name for name in ('ntsync', 'fsync', 'esync')
                                      if name+': up and running.' in startup_logs), 'unknown')
        if fsync and report['sync_backend'] != 'fsync':
            raise RuntimeError('native FSYNC control did not activate FSYNC')
        report.update(cache_metadata(seed, startup_logs))
        report['swapchain_latency_frames'] = swapchain_latency_frames
        if swapchain_latency_frames is not None:
            activated = f'Ensure maximum latency of {swapchain_latency_frames} frames with KHR_present_wait.' in startup_logs
            if not activated:
                raise RuntimeError('native swapchain latency override did not activate')
            report['swapchain_latency_verified'] = True
    finally:
        cleanup = dict(environment, WINEPREFIX=str(output/'compatdata/pfx'))
        subprocess.run([str(proton/'files/bin/wineserver'), '-k'], env=cleanup,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10)
        if child is not None:
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait()
        if trace_directory is not None:
            try:
                trace_path = Path(trace_directory.name)/'queue-profile.json'
                if trace_path.exists():
                    shutil.copyfile(trace_path, output/'queue-profile.json')
            finally:
                trace_directory.cleanup()
    if queue_profile:
        report['queue_trace'] = normalize_trace(output/'queue-profile.json')
        report['queue_profile_storage'] = 'private host tmpfs; preserved after writer stopped'
    if preserve_shader_cache:
        report['shader_cache_export'] = export_gpu_cache(output/'cache', output/'shader-cache')
    (output/'report.json').write_text(json.dumps(report, indent=2)+'\n')
    return score


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--build', type=Path, required=True)
    parser.add_argument('--application-dir', type=Path, required=True)
    parser.add_argument('--proton', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--pairs', type=int, default=1)
    parser.add_argument('--threads-per-core', type=int, choices=[1, 2], default=1)
    parser.add_argument('--virtual-refresh-hz', type=int)
    parser.add_argument('--kms-wait', choices=['sleep', 'events'], default='events',
                        help='KMS event waits (default) or legacy fixed polling')
    parser.add_argument('--kms-refresh', choices=['legacy', 'kms'], default='legacy')
    parser.add_argument('--kms-explicit-sync', action='store_true')
    parser.add_argument('--compare-explicit-sync', action='store_true',
                        help='run native, baseline and experimental explicit-sync variants')
    parser.add_argument('--compare-displays', action='store_true',
                        help='compare native, strict SDL, direct Wayland broker and strict D-Bus viewer')
    parser.add_argument('--window-fullscreen', action='store_true')
    parser.add_argument('--host-display', choices=['sdl', 'broker', 'dbus'], default='sdl')
    parser.add_argument('--kms-hz', type=int)
    parser.add_argument('--ntsync', action='store_true')
    parser.add_argument('--fsync', action='store_true', help='disable native NTSYNC to match the guest FSYNC control')
    parser.add_argument('--poll-arm', choices=['sync', 'async'], default='sync')
    parser.add_argument('--halt-poll-ns', type=int)
    parser.add_argument('--poll-transport', choices=['sync', 'enqueue'], default='sync')
    parser.add_argument('--sync-file-bridge', action='store_true')
    parser.add_argument('--application-storage', choices=['virtiofs', 'tmpfs'], default='virtiofs')
    parser.add_argument('--ram-layout', choices=['memfd', 'aligned-memfd'], default='aligned-memfd')
    parser.add_argument('--host-thp', choices=['off', 'system'], default='off')
    parser.add_argument('--queue-profile', action='store_true', help='diagnostic runs; exclude their scores from FPS conclusions')
    parser.add_argument('--swapchain-latency-frames', type=int, choices=[1, 2, 3])
    captures = parser.add_mutually_exclusive_group()
    captures.add_argument('--application-final-capture', action='store_true', default=True,
                          help='capture VM output after saving the score (default)')
    captures.add_argument('--application-periodic-captures', action='store_false', dest='application_final_capture',
                          help='legacy diagnostic screenshots during scoring; exclude from clean FPS comparisons')
    parser.add_argument('--surface-diagnostics', choices=['on','off'], default='off',
                        help='guest surface dumps/cadence logging; off by default for throughput comparisons')
    parser.add_argument('--preserve-shader-cache', action='store_true',
                        help='export GPU shader caches after both native and VM runs have stopped')
    args = parser.parse_args()
    if args.halt_poll_ns is not None:
        record = json.loads((args.build/'build.json').read_text())
        if not 0 <= args.halt_poll_ns <= 2000000 or '0024-control-halt-polling-only-for-the-owned-vm.patch' not in record.get('local_patches', {}):
            parser.error('--halt-poll-ns requires per-VM polling control and a value in 0..2000000')
    if args.compare_explicit_sync and (args.sync_file_bridge or args.kms_explicit_sync or args.host_display != 'sdl'):
        parser.error('--compare-explicit-sync selects both experimental flags and requires SDL')
    if args.compare_displays and (args.compare_explicit_sync or args.sync_file_bridge or args.kms_explicit_sync or args.host_display != 'sdl'):
        parser.error('--compare-displays selects display and synchronization flags itself')
    if args.compare_displays and not json.loads((args.build/'build.json').read_text()).get('dbus_display'):
        parser.error('--compare-displays requires a D-Bus research build')
    if args.kms_explicit_sync and (not args.sync_file_bridge or args.host_display not in ('sdl', 'dbus')):
        parser.error('--kms-explicit-sync requires --sync-file-bridge and SDL/D-Bus presentation')
    if args.sync_file_bridge or args.compare_explicit_sync:
        record = json.loads((args.build/'build.json').read_text())
        if '0018-bridge-native-and-guest-sync-file-imports.patch' not in record.get('local_patches', {}):
            parser.error('--sync-file-bridge requires the corrected experimental import build')
        if (args.kms_explicit_sync or args.compare_explicit_sync) and '0022-record-read-completions-on-normal-qemu-exit.patch' not in record.get('local_patches', {}):
            parser.error('--kms-explicit-sync requires strict host scanout completion support')
    if args.fsync and args.ntsync:
        parser.error('--fsync and --ntsync are mutually exclusive')
    if not 1 <= args.pairs <= 3:
        parser.error('--pairs must be 1..3')
    if args.virtual_refresh_hz is not None and not 1 <= args.virtual_refresh_hz <= 1000:
        parser.error('--virtual-refresh-hz must be 1..1000')
    if args.kms_hz is not None and not 1 <= args.kms_hz <= 1000:
        parser.error('--kms-hz must be 1..1000')
    if args.host_display == 'broker' and args.virtual_refresh_hz is not None:
        parser.error('broker timing uses --kms-hz instead of the SDL refresh override')
    output = args.output.resolve()
    source = args.application_dir.resolve(strict=True)
    proton = args.proton.resolve(strict=True)
    executable = source/'Benchmark.exe'
    if not executable.is_file():
        parser.error('the owned application must contain Benchmark.exe')
    output.mkdir(parents=True, exist_ok=False)
    records = []
    modes = ('native', 'nvkvm', 'nvkvm-explicit') if args.compare_explicit_sync else ('native', 'nvkvm')
    if args.compare_displays:
        modes = ('native', 'nvkvm', 'nvkvm-broker', 'nvkvm-dbus')
    for pair in range(args.pairs):
        for mode in (modes if pair % 2 == 0 else tuple(reversed(modes))):
            trial = output/f'{mode}-{pair}'
            use_bridge = mode != 'native' and (args.sync_file_bridge or mode == 'nvkvm-explicit')
            use_explicit = mode != 'native' and (args.kms_explicit_sync or mode == 'nvkvm-explicit')
            display = args.host_display
            if args.compare_displays:
                display = {'nvkvm': 'sdl', 'nvkvm-broker': 'broker', 'nvkvm-dbus': 'dbus'}.get(mode, 'sdl')
                use_explicit = use_bridge = mode in ('nvkvm', 'nvkvm-dbus')
            print(trial.name, flush=True)
            if mode == 'native':
                score = native(proton, source, trial, args.fsync, args.queue_profile, args.swapchain_latency_frames,
                               preserve_shader_cache=args.preserve_shader_cache)
            else:
                command = ['python3', str(ROOT/'tools/probe_nvkvm.py'), '--build', str(args.build.resolve()),
                           '--proton', str(proton), '--application-dir', str(source), '--window',
                           '--cpus', str(os.cpu_count()), '--memory-mib', '8192', '--output', str(trial),
                           '--threads-per-core', str(args.threads_per_core),
                           '--kms-wait', args.kms_wait,
                           '--poll-arm', args.poll_arm,
                           '--poll-transport', args.poll_transport,
                           '--application-storage', args.application_storage,
                           '--ram-layout', args.ram_layout,
                           '--host-thp', args.host_thp,
                           '--kms-refresh', args.kms_refresh,
                           '--host-display', display,
                           *['--argument='+argument for argument in ARGUMENTS]]
                if args.virtual_refresh_hz is not None:
                    command += ['--virtual-refresh-hz', str(args.virtual_refresh_hz)]
                if args.window_fullscreen:
                    command += ['--window-fullscreen']
                if args.kms_hz is not None:
                    command += ['--kms-hz', str(args.kms_hz)]
                if args.ntsync:
                    command += ['--ntsync']
                if args.halt_poll_ns is not None:
                    command += ['--halt-poll-ns', str(args.halt_poll_ns)]
                if args.queue_profile:
                    command += ['--queue-profile']
                if args.swapchain_latency_frames is not None:
                    command += ['--swapchain-latency-frames', str(args.swapchain_latency_frames)]
                if args.application_final_capture:
                    command += ['--application-final-capture']
                command += ['--surface-diagnostics', args.surface_diagnostics]
                if args.preserve_shader_cache:
                    command += ['--preserve-shader-cache']
                if use_bridge:
                    command += ['--sync-file-bridge']
                if use_explicit:
                    command += ['--kms-explicit-sync']
                with (output/f'{mode}-{pair}.log').open('w') as log:
                    subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, check=True)
                report = json.loads((trial/'report.json').read_text())
                if not report['passed'] or not report['desktop_capture_verified']:
                    raise RuntimeError('VM benchmark did not verify visible hardware rendering')
                if args.fsync and report['sync_backend'] != 'fsync':
                    raise RuntimeError('guest FSYNC control did not activate FSYNC')
                score = report['proton']['benchmark_fps']
            records.append(dict(mode=mode, pair=pair, fps=score, directory=str(trial),
                                host_display=display if mode != 'native' else None,
                                sync_file_bridge=use_bridge, kms_explicit_sync=use_explicit))
            print(f'{mode}: {score:.2f} FPS', flush=True)
            (output/'records.json').write_text(json.dumps(records, indent=2)+'\n')
    medians = {mode: statistics.median(row['fps'] for row in records if row['mode'] == mode)
               for mode in modes}
    report = dict(schema=1, passed=True, scope='owned UE5 D3D12 benchmark; exploratory performance',
                  arguments=ARGUMENTS, proton_path=str(proton), records=records, median_fps=medians,
                  cache_policy='fresh private Proton prefix, VKD3D cache and NVIDIA driver cache per run',
                  threads_per_core=args.threads_per_core, virtual_refresh_hz=args.virtual_refresh_hz,
                  kms_wait=args.kms_wait,
                  kms_refresh=args.kms_refresh,
                  kms_explicit_sync=args.kms_explicit_sync,
                  window_fullscreen=args.window_fullscreen,
                  host_display=args.host_display, ntsync_requested=args.ntsync,
                  fsync_control=args.fsync, poll_arm=args.poll_arm,
                  halt_poll_ns=args.halt_poll_ns,
                  poll_transport=args.poll_transport, application_storage=args.application_storage,
                  sync_file_bridge=args.sync_file_bridge,
                  compare_explicit_sync=args.compare_explicit_sync,
                  compare_displays=args.compare_displays,
                  queue_profiled=args.queue_profile,
                  swapchain_latency_frames=args.swapchain_latency_frames,
                  application_final_capture=args.application_final_capture,
                  surface_diagnostics=args.surface_diagnostics,
                  shader_cache_preserved=args.preserve_shader_cache,
                  timed_instrumentation=(['guest_surface_diagnostics'] if args.surface_diagnostics=='on' else [])
                      + (['periodic_frame_captures'] if not args.application_final_capture else [])
                      + (['vkd3d_queue_timeline'] if args.queue_profile else []),
                  ram_layout=args.ram_layout,
                  host_thp=args.host_thp,
                  nvkvm_native_percent=100*medians['nvkvm']/medians['native'],
                  executable_sha256=hashlib.sha256(executable.read_bytes()).hexdigest(),
                  limitations=['GPU clocks are not locked; no general game-compatibility claim',
                               'Native uses the host CPU policy; VM uses Legion XCR0/AVX-512 restrictions',
                               'Displayed score is the mean of instantaneous FPS; inspect per-run frame logs',
                               'Native and guest Windows synchronization backends may differ; inspect per-run sync_backend'])
    if args.compare_explicit_sync:
        report['explicit_native_percent'] = 100*medians['nvkvm-explicit']/medians['native']
        report['explicit_change_percent'] = 100*(medians['nvkvm-explicit']/medians['nvkvm']-1)
    if args.compare_displays:
        report['display_native_percent'] = {mode: 100*value/medians['native'] for mode, value in medians.items() if mode != 'native'}
        report['limitations'].append('SDL and D-Bus use explicit guest sync; broker uses its existing implicit path. This compares usable paths, not transport overhead in isolation. Viewer timing is not input-to-photon latency.')
    (output/'report.json').write_text(json.dumps(report, indent=2)+'\n')
    print(json.dumps(report, indent=2))


if __name__ == '__main__':
    main()
