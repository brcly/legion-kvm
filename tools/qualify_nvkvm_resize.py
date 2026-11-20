#!/usr/bin/env python3
"""Qualify whole-image NVIDIA resize in an owned, isolated headless compositor."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--build', type=Path, default=ROOT/'build/nvkvm-pv-dbus')
    parser.add_argument('--proton', type=Path, required=True)
    parser.add_argument('--fixture', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--sway', type=Path)
    parser.add_argument('--stress-cycles', type=int, default=48)
    args = parser.parse_args()
    if not 1 <= args.stress_cycles <= 48:
        parser.error('stress cycles must be 1..48')
    build, proton, fixture = [p.resolve(strict=True) for p in [args.build, args.proton, args.fixture]]
    identity = json.loads((build/'build.json').read_text())
    if not identity.get('dbus_display') or '0026-allow-whole-image-virtual-primary-scaling.patch' not in identity.get('local_patches', {}):
        parser.error('qualification requires the D-Bus research build and whole-image scaling patch 0026')
    sway = args.sway or Path(json.loads((ROOT/'build/input-latency/build.json').read_text())['compositor'])
    sway = sway.resolve(strict=True)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    base = ['python3', str(ROOT/'tools/probe_nvkvm.py'), '--build', str(build),
            '--proton', str(proton), '--fixture', str(fixture), '--window', '--window-fullscreen',
            '--private-output', 'HEADLESS-1', '--proton-window', '--sync-file-bridge',
            '--kms-explicit-sync', '--input-check', '--ntsync', '--cpus', '16',
            '--memory-mib', '8192', '--kms-hz', '240']
    records = []
    with tempfile.TemporaryDirectory(prefix='legion-latency-') as temporary:
        runtime = Path(temporary)
        runtime.chmod(0o700)
        config = runtime/'sway.conf'
        config.write_text('output * mode 1920x1080@240Hz\ndefault_border none\n'
                          'default_floating_border none\nfocus_follows_mouse no\n'
                          'for_window [app_id=".*"] fullscreen enable\n')
        environment = {key: value for key, value in os.environ.items()
                       if not key.startswith(('VK_', 'VN_', 'NVKVM_', 'LEGION_', 'DXVK_', 'VKD3D_',
                                              'WINE', 'PROTON_', 'MANGOHUD', 'STEAM_COMPAT_'))
                       and key not in ('DISPLAY', 'WAYLAND_DISPLAY', 'SWAYSOCK', 'DBUS_SESSION_BUS_ADDRESS',
                                       'LD_PRELOAD', 'LD_LIBRARY_PATH', 'GBM_BACKEND', 'GBM_BACKENDS_PATH',
                                       '__EGL_VENDOR_LIBRARY_FILENAMES', 'DRI_PRIME')}
        environment.update(XDG_RUNTIME_DIR=str(runtime), WLR_BACKENDS='headless',
                           WLR_HEADLESS_OUTPUTS='1', WLR_RENDERER='gles2',
                           WLR_RENDER_DRM_DEVICE='/dev/dri/renderD128')
        with (output/'compositor.log').open('w') as log:
            compositor = subprocess.Popen([str(sway), '--unsupported-gpu', '-V', '-c', str(config)],
                                         env=environment, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            try:
                deadline = time.monotonic()+10
                while True:
                    sockets = [p for p in runtime.glob('wayland-*') if p.is_socket()]
                    if sockets:
                        break
                    if compositor.poll() is not None or time.monotonic()>deadline:
                        raise RuntimeError('private compositor failed; inspect compositor.log')
                    time.sleep(.02)
                environment['WAYLAND_DISPLAY'] = sockets[0].name
                (output/'isolation.json').write_text(json.dumps(dict(runtime=str(runtime),
                    display=sockets[0].name, compositor=str(sway), output='1920x1080@240Hz',
                    input='owned QMP or D-Bus events only'), indent=2)+'\n')
                trials = [('resize', ['--fixture-resize', '--sdl-frame-input']),
                          ('stress', ['--fixture-resize', '--fixture-resize-cycles', str(args.stress_cycles), '--sdl-frame-input']),
                          ('start-scaled', ['--fixture-start-scaled', '--sdl-frame-input']),
                          ('dbus-resize', ['--fixture-resize', '--host-display', 'dbus'])]
                for name, flags in trials:
                    trial = output/name
                    print(name, flush=True)
                    with (output/(name+'.log')).open('w') as runlog:
                        result = subprocess.run(base+flags+['--output', str(trial)], env=environment,
                                                cwd=ROOT, stdout=runlog, stderr=subprocess.STDOUT)
                    report = json.loads((trial/'report.json').read_text())
                    kms = json.loads((trial/'kms-result.json').read_text()) if (trial/'kms-result.json').exists() else {}
                    passed = (result.returncode==0 and report.get('passed') and kms.get('scaling_invalid_geometry_checks')==3
                              and kms.get('import_cache_entries', 33)<=32)
                    from PIL import Image
                    observed = []
                    for capture in report.get('captures', []):
                        if capture.get('source')=='owned_wayland_window' and capture.get('nonuniform'):
                            with Image.open(capture['path']) as image:
                                observed.append(hashlib.sha256(image.convert('RGB').tobytes()).hexdigest())
                    passed = passed and len(set(observed))>=4
                    if name=='stress' and args.stress_cycles==48:
                        passed = passed and kms.get('import_evictions', 0)>0
                    if name=='start-scaled':
                        sizes = {(image['width'], image['height']) for image in report.get('proton', {}).get('image_checks', [])}
                        passed = passed and sizes=={(1440,810)} and kms.get('source_sizes', [{}])[0].get('width')==1440
                    records.append(dict(name=name, passed=bool(passed), report=str(trial/'report.json'),
                                        resize=report.get('resize_validation'), distinct_visible_frames=len(set(observed)), kms=kms, explicit_sync=report.get('explicit_sync')))
                    summary = dict(schema=1, passed=all(row['passed'] for row in records) and len(records)==4,
                                   scope='owned NVIDIA whole-image resize correctness, not FPS or physical latency', records=records)
                    (output/'report.json').write_text(json.dumps(summary, indent=2)+'\n')
                    if not passed:
                        raise RuntimeError(f'{name} failed; inspect its report')
            finally:
                if compositor.poll() is None:
                    os.killpg(compositor.pid, signal.SIGTERM)
                compositor.wait(timeout=5)
    print(json.dumps(dict(passed=True, report=str(output/'report.json'))))


if __name__ == '__main__':
    main()
