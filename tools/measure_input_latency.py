#!/usr/bin/env python3
"""Measure an owned input-reactive D3D12 fixture inside one private compositor."""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import signal
import statistics
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]


def summarize(events, requested):
    samples = [event for event in events if event.get('event') == 'sample']
    summaries = [event for event in events if event.get('event') == 'summary']
    ready = [event for event in events if event.get('event') == 'fixture_ready']
    if len(ready)!=1 or ready[0].get('magic')!=0xa5c3:
        raise ValueError('missing independently identified fixture image')
    if len(summaries) != 1 or not summaries[0].get('passed'):
        raise ValueError('observer did not complete: '+str(summaries[-1:] or events[-1:]))
    if len(samples) != requested or summaries[0].get('samples') != requested:
        raise ValueError('missing independently observed responses')
    runtime_events = [e for e in events if e.get('event')=='runtime_measurement']
    runtime = bool(runtime_events)
    clocks = [e for e in events if e.get('event')=='runtime_ready']
    final = [e for e in events if e.get('event')=='final_image_verified']
    if runtime and (len(runtime_events)!=1 or runtime_events[0].get('timed_captures')!=0
                    or len(clocks)!=1 or not 0<clocks[0].get('qpc_frequency',0)<=1e12
                    or len(final)!=1 or final[0].get('sequence')!=requested):
        raise ValueError('runtime feedback lacks independent image checks or captured during timing')
    previous = ready[0]['time_ns']
    last_present = last_fence = 0
    for index, sample in enumerate(samples, 1):
        start, end = sample['injected_ns'], sample['observed_ns']
        if (sample['sequence'] != index or not previous < start < end
                or abs(sample['response_ms']-(end-start)/1e6) > .00001):
            raise ValueError('invalid response identity or timestamp')
        if runtime:
            qpc=clocks[0]['qpc_frequency']
            first,submitted,feedback,polled=[sample['source_'+name+'_qpc'] for name in ['input','submitted','feedback','polled']]
            if not (0<first<=submitted<=polled and first<=feedback<=polled
                    and sample['target_present_id']>last_present and sample['feedback_present_id']>=sample['target_present_id']
                    and sample['fence_value']>last_fence and sample['fence_value']<=sample['completed_fence_value']<2**64-1
                    and abs(sample['source_input_to_feedback_ms']-(feedback-first)*1000/qpc)<.00001
                    and abs(sample['source_feedback_to_poll_ms']-(polled-feedback)*1000/qpc)<.00001):
                raise ValueError('runtime feedback identity, original fence or source clock is invalid')
            last_present,last_fence=sample['target_present_id'],sample['fence_value']
        elif (not 0<sample['capture_requested_ns']<=end or not math.isfinite(sample['capture_ms'])
              or abs(sample['capture_ms']-(end-sample['capture_requested_ns'])/1e6)>.00001):
            raise ValueError('invalid capture observation')
        previous = end
    if runtime and not final[0]['time_ns']>previous:
        raise ValueError('final image was not checked after timed runtime responses')
    values = sorted(sample['response_ms'] for sample in samples)
    if not all(math.isfinite(value) and 0 < value < 1000 for value in values):
        raise ValueError('invalid latency observation')
    def percentile(percent):
        position = (len(values)-1)*percent/100
        low, high = math.floor(position), math.ceil(position)
        return values[low]+(values[high]-values[low])*(position-low)
    result = dict(schema=1, passed=True, sample_count=len(samples),
                response_ms=dict(p50=statistics.median(values), p95=percentile(95),
                                 p99=percentile(99), maximum=max(values)),
                capture_wait_ms=(dict(median=statistics.median(s['capture_ms'] for s in samples),
                                     maximum=max(s['capture_ms'] for s in samples)) if not runtime else None),
                observer=summaries[0], samples=samples,
                scope='host virtual-keyboard dispatch to first changed barcode observed in completed private-compositor capture; not physical input-to-photon',
                limitations=['Capture forces composition and adds observation/scheduling overhead',
                             'Private headless Sway at 240 Hz; not the desktop compositor or monitor',
                             'No guest/host clock subtraction; all measured endpoints use host CLOCK_MONOTONIC'])
    result['method']='runtime_feedback' if runtime else 'compositor_capture'
    if runtime:
        result.update(scope='host virtual-keyboard dispatch to receipt of source runtime frame-feedback record; includes transport and polling; not host display or photons',
                      runtime_measurement=runtime_events[0], qpc_frequency=clocks[0]['qpc_frequency'],
                      source_input_to_feedback_ms_median=statistics.median(s['source_input_to_feedback_ms'] for s in samples),
                      source_feedback_to_poll_ms_median=statistics.median(s['source_feedback_to_poll_ms'] for s in samples),
                      limitations=['DXGI completion statistics may be approximate; guest runtime feedback is not host window presentation',
                                   'Host response includes source file flush, virtiofs transport and host notification/scheduling',
                                   'No subtraction between host and guest clocks; source durations use one QPC clock',
                                   'Initial and final image captures only; zero captures and source readbacks during timed phase'])
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--mode', choices=['native', 'sdl', 'broker', 'dbus'], required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--proton', type=Path, required=True)
    parser.add_argument('--build', type=Path, default=ROOT/'build/nvkvm-pv-dbus')
    parser.add_argument('--fixture', type=Path, default=ROOT/'build/input-latency/windows-target/x86_64-pc-windows-msvc/release/legion-proton-workload.exe')
    parser.add_argument('--sway', type=Path)
    parser.add_argument('--observer', type=Path, default=ROOT/'build/input-latency/observer')
    parser.add_argument('--samples', type=int, default=40)
    parser.add_argument('--delay-ms', type=int, default=0, help='diagnostic source delay positive control')
    parser.add_argument('--dispatch-delay-ms', type=int, default=0, help='diagnostic host wait before actual input dispatch')
    parser.add_argument('--no-input', action='store_true', help='negative control: observe without sending the expected key')
    parser.add_argument('--sdl-frame-input', action='store_true')
    parser.add_argument('--runtime-feedback', action='store_true')
    parser.add_argument('--host-presentation', action='store_true')
    parser.add_argument('--source-frame-tags', action='store_true')
    parser.add_argument('--viewer-early-swap', action='store_true')
    parser.add_argument('--viewer-gpu-timing', action='store_true')
    parser.add_argument('--viewer-presentation', action='store_true')
    parser.add_argument('--viewer-gpu-priority', choices=['default','medium','high'], default='default')
    parser.add_argument('--compositor-gpu-priority', choices=['observe','medium','high'])
    parser.add_argument('--compositor-priority-shim', type=Path, default=ROOT/'build/gpu-priority/liblegion_egl_priority.so')
    parser.add_argument('--swapchain-latency-frames', type=int, choices=[1,2,3])
    parser.add_argument('--fragment-iterations', type=int, default=16)
    args = parser.parse_args()
    if not 20 <= args.samples <= 100 or not 0 <= args.delay_ms <= 100:
        parser.error('samples must be 20..100 and delay 0..100 ms')
    if not 0<=args.fragment_iterations<=65536:parser.error('fragment iterations must be 0..65536')
    if args.viewer_early_swap and not (args.mode=='dbus' and args.host_presentation):parser.error('early swap requires tagged host presentation mode')
    if args.viewer_gpu_timing and not (args.mode=='dbus' and args.host_presentation):parser.error('GPU timing requires tagged host presentation mode')
    if args.viewer_gpu_priority!='default' and args.mode!='dbus':parser.error('viewer GPU priority requires D-Bus')
    if args.sdl_frame_input and args.mode!='sdl':parser.error('--sdl-frame-input requires --mode sdl')
    if not 0<=args.dispatch_delay_ms<=100:parser.error('host dispatch delay must be 0..100 ms')
    if args.dispatch_delay_ms and not args.host_presentation:parser.error('host dispatch calibration requires host presentation measurement')
    if args.host_presentation:args.source_frame_tags=args.viewer_presentation=True
    if (args.source_frame_tags or args.viewer_presentation) and not (args.mode=='dbus' and args.runtime_feedback):parser.error('host frame diagnostics require D-Bus runtime-feedback mode')
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    proton, fixture = args.proton.resolve(strict=True), args.fixture.resolve(strict=True)
    sway = args.sway
    if sway is None:
        build_record = ROOT/'build/input-latency/build.json'
        sway = Path(json.loads(build_record.read_text())['compositor']) if build_record.exists() else ROOT/'build/input-latency/sway-root/usr/bin/sway'
    sway, observer = sway.resolve(strict=True), args.observer.resolve(strict=True)
    children, logs = [], []
    native_environment = None
    start = time.monotonic()
    try:
        with tempfile.TemporaryDirectory(prefix='legion-latency-') as temporary:
            runtime = Path(temporary)
            runtime.chmod(0o700)
            config = runtime/'sway.conf'
            config.write_text('output * mode 1920x1080@240Hz\ndefault_border none\ndefault_floating_border none\nfocus_follows_mouse no\nfor_window [app_id=".*"] fullscreen enable\n')
            environment = {key: value for key, value in os.environ.items()
                           if not key.startswith(('VK_', 'VN_', 'NVKVM_', 'LEGION_', 'DXVK_', 'VKD3D_', 'WINE', 'PROTON_', 'MANGOHUD', 'STEAM_COMPAT_'))
                           and key not in ('DISPLAY', 'WAYLAND_DISPLAY', 'SWAYSOCK', 'DBUS_SESSION_BUS_ADDRESS',
                                           'LD_PRELOAD', 'LD_LIBRARY_PATH', 'GBM_BACKEND', 'GBM_BACKENDS_PATH',
                                           '__EGL_VENDOR_LIBRARY_FILENAMES', 'DRI_PRIME')}
            environment.update(XDG_RUNTIME_DIR=str(runtime), WLR_BACKENDS='headless', WLR_HEADLESS_OUTPUTS='1',
                               WLR_RENDERER='gles2', WLR_RENDER_DRM_DEVICE='/dev/dri/renderD128')
            log = (output/'compositor.log').open('w'); logs.append(log)
            compositor_environment=dict(environment)
            if args.compositor_gpu_priority:
                compositor_environment.update(LD_PRELOAD=str(args.compositor_priority_shim.resolve(strict=True)),LEGION_COMPOSITOR_GPU_PRIORITY=args.compositor_gpu_priority)
            compositor = subprocess.Popen([str(sway), '--unsupported-gpu', '-V', '-c', str(config)],
                                          env=compositor_environment, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            children.append(compositor)
            deadline = time.monotonic()+10
            while True:
                sockets = [p for p in runtime.glob('wayland-*') if p.is_socket()]
                ipc = list(runtime.glob('sway-ipc*.sock'))
                if sockets and ipc:
                    break
                if compositor.poll() is not None or time.monotonic()>deadline:
                    raise RuntimeError('private compositor failed; inspect compositor.log')
                time.sleep(.02)
            environment['WAYLAND_DISPLAY'] = sockets[0].name
            query = subprocess.check_output([str(sway.with_name('swaymsg')), '-s', str(ipc[0]), '-r', '-t', 'get_outputs'], text=True)
            outputs = json.loads(query)
            (output/'private-outputs.json').write_text(query)
            if len(outputs)!=1 or outputs[0]['current_mode'] != dict(width=1920, height=1080, refresh=240000):
                raise RuntimeError('private output differs from the measurement configuration')
            (output/'isolation.json').write_text(json.dumps(dict(private_runtime=str(runtime),
                wayland_display=sockets[0].name, compositor_pid=compositor.pid, output_mode=outputs[0]['current_mode']), indent=2)+'\n')
            observer_log = (output/'observer.jsonl').open('w'); logs.append(observer_log)
            observer_error = (output/'observer.stderr').open('w'); logs.append(observer_error)
            command = [str(observer), '--samples', str(args.samples)]
            if args.no_input: command.append('--no-input')
            if args.host_presentation:command+=['--final-capture-delay-ms','250']
            if args.dispatch_delay_ms:command+=['--dispatch-delay-ms',str(args.dispatch_delay_ms)]
            if args.runtime_feedback:command+=['--runtime-feedback',str(output/('runtime-feedback.log' if args.mode=='native' else 'vm/runtime-feedback.log'))]
            capture = subprocess.Popen(command, env=environment, stdout=observer_log,
                                       stderr=observer_error, start_new_session=True)
            children.append(capture)
            if args.mode == 'native':
                fixture_config = dict(schema=1, width=1920, height=1080, warmup_ms=200, sample_ms=1000,
                                      cpu_rounds=25000, gpu_clears=1, gpu_timestamps=True,
                                      presentation=dict(seconds=25, resize=False, fragment_iterations=args.fragment_iterations,
                                                        latency_probe=True, latency_delay_ms=args.delay_ms))
                if args.runtime_feedback:fixture_config['presentation']['runtime_feedback']=True
                (output/'fixture-config.json').write_text(json.dumps(fixture_config, indent=2)+'\n')
                native_environment = dict(environment,
                    STEAM_COMPAT_DATA_PATH=str(output/'compatdata'), STEAM_COMPAT_CLIENT_INSTALL_PATH=str(output/'steam'),
                    STEAM_COMPAT_INSTALL_PATH=str(output), SteamAppId='0', SteamGameId='0',
                    PROTON_ENABLE_WAYLAND='1', PROTON_NO_NTSYNC='1', WINEDEBUG='-all',
                    WINEDLLOVERRIDES='dxgi,d3d11,d3d12,d3d12core=n;lsteamclient=d;wineopenxr=d',
                    LEGION_WORKLOAD_DIRECTORY='Z:'+str(output).replace('/', '\\'),
                    VK_DRIVER_FILES='/usr/share/vulkan/icd.d/nvidia_icd.json', VK_LOADER_LAYERS_DISABLE='~implicit~',
                    XDG_CACHE_HOME=str(output/'cache'), VKD3D_SHADER_CACHE_PATH=str(output/'cache'),
                    __GL_SHADER_DISK_CACHE_PATH=str(output/'cache/nvidia'))
                if args.swapchain_latency_frames is not None:
                    native_environment.update(VKD3D_SWAPCHAIN_LATENCY_FRAMES=str(args.swapchain_latency_frames), VKD3D_DEBUG='info')
                for name in ['steam', 'compatdata', 'cache/nvidia']:(output/name).mkdir(parents=True)
                with (output/'prefix.log').open('w') as prefix_log:
                    subprocess.run(['python3', str(proton/'proton'), 'getcompatpath', str(fixture)],
                                   env=native_environment, stdout=prefix_log, stderr=subprocess.STDOUT, check=True, timeout=60)
                command = ['python3', str(proton/'proton'), 'runinprefix', str(fixture)]
                job_environment = native_environment
            else:
                command = ['python3', str(ROOT/'tools/probe_nvkvm.py'), '--build', str(args.build.resolve()),
                           '--proton', str(proton), '--fixture', str(fixture), '--proton-window', '--window',
                           '--window-fullscreen', '--host-display', args.mode, '--kms-hz', '240', '--cpus', '16',
                           '--memory-mib', '8192', '--fixture-fragment-iterations', str(args.fragment_iterations), '--latency-probe', '--latency-delay-ms', str(args.delay_ms),
                           '--latency-observer-report', str(output/'latency-observer.json'), '--output', str(output/'vm')]
                if args.mode in ('sdl', 'dbus'):
                    command += ['--sync-file-bridge', '--kms-explicit-sync']
                if args.sdl_frame_input:command+=['--sdl-frame-input']
                if args.runtime_feedback:command+=['--runtime-feedback']
                if args.source_frame_tags:command+=['--fixture-frame-tags']
                if args.viewer_presentation:command+=['--viewer-presentation']
                if args.viewer_early_swap:command+=['--viewer-early-swap']
                if args.viewer_gpu_timing:command+=['--viewer-gpu-timing']
                if args.viewer_gpu_priority!='default':command+=['--viewer-gpu-priority',args.viewer_gpu_priority]
                if args.host_presentation:command+=['--viewer-frame-tags']
                if args.swapchain_latency_frames is not None:command+=['--swapchain-latency-frames',str(args.swapchain_latency_frames)]
                job_environment = environment
            (output/'command.json').write_text(json.dumps(command, indent=2)+'\n')
            log = (output/'launch.log').open('w'); logs.append(log)
            job = subprocess.Popen(command, env=job_environment, stdout=log,
                                   stderr=subprocess.STDOUT, start_new_session=True)
            children.append(job)
            deadline=time.monotonic()+100
            while capture.poll() is None:
                if job.poll() is not None:
                    raise RuntimeError('owned fixture ended before its input responses completed; inspect launch.log')
                if time.monotonic()>deadline:
                    raise TimeoutError('latency observer deadline exceeded')
                time.sleep(.02)
            observer_log.flush()
            events = [json.loads(line) for line in (output/'observer.jsonl').read_text().splitlines() if line]
            observed = summarize(events, args.samples)
            (output/'latency-observer.json').write_text(json.dumps(observed, indent=2)+'\n')
            if job.wait(timeout=100)!=0:
                raise RuntimeError('owned fixture/rendering validation failed; inspect launch.log')
            validation = json.loads((output/('workload-result.json' if args.mode=='native' else 'vm/report.json')).read_text())
            if not validation.get('passed'):
                raise RuntimeError('owned hardware fixture did not pass')
            application = validation if args.mode=='native' else validation['proton']
            if not (application.get('gpu_verified') and application.get('vendor_id')==0x10de
                    and application.get('latency_probe') and application.get('space_events')==args.samples
                    and application.get('runtime_feedback',False)==args.runtime_feedback
                    and (not args.runtime_feedback or application.get('runtime_feedback_samples')==args.samples)
                    and application.get('latency_delay_ms')==args.delay_ms
                    and application.get('fragment_iterations')==args.fragment_iterations
                    and application.get('original_queue_drained') and len(application.get('image_checks', []))>=2):
                raise RuntimeError('fixture did not confirm all input events and original GPU image/fence checks')
            if args.source_frame_tags and not application.get('frame_tags'):raise RuntimeError('source fixture did not activate frame tags')
            if args.host_presentation:
                from link_frame_presentation import link
                linked=link(events,application,[json.loads(line) for line in (output/'vm/host-presentation.jsonl').read_text().splitlines()],(output/'vm/viewer.log').read_text().splitlines())
                if linked['sample_count']!=args.samples:raise RuntimeError('missing presented input generations')
                (output/'host-latency.json').write_text(json.dumps(linked,indent=2)+'\n')
            if args.mode == 'native':
                startup_logs = ''.join((output/name).read_text() for name in ('prefix.log', 'launch.log'))
                sync_backend = next((name for name in ('ntsync', 'fsync', 'esync')
                                     if name+': up and running.' in startup_logs), 'unknown')
            else:
                sync_backend = validation.get('sync_backend')
            if sync_backend != 'fsync':
                raise RuntimeError('latency comparison requires verified FSYNC in every path')
            if args.swapchain_latency_frames is not None:
                activated = (f'Ensure maximum latency of {args.swapchain_latency_frames} frames with KHR_present_wait.' in startup_logs
                             if args.mode=='native' else validation.get('swapchain_latency_verified'))
                if not activated:raise RuntimeError('requested swapchain latency did not activate in stock VKD3D')
            if 'GL renderer: NVIDIA' not in (output/'compositor.log').read_text():
                raise RuntimeError('private compositor did not verify NVIDIA hardware rendering')
            compositor_priorities=[]
            if args.compositor_gpu_priority:
                import re
                compositor_priorities=[dict(requested=int(a,0),assigned=int(b,0),valid=int(c)) for a,b,c in re.findall(r'LEGION_COMPOSITOR_GPU_PRIORITY requested=(0x[0-9a-f]+|0) assigned=(0x[0-9a-f]+|0) valid=(\d+)',(output/'compositor.log').read_text())]
                expected={'observe':None,'medium':0x3102,'high':0x3101}[args.compositor_gpu_priority]
                if not compositor_priorities or any(not p['valid'] or (expected is not None and p['assigned']!=expected) for p in compositor_priorities):raise RuntimeError('private compositor did not confirm requested EGL priority')
            report = dict(**observed, mode=args.mode, delay_ms=args.delay_ms, dispatch_delay_ms=args.dispatch_delay_ms, elapsed_seconds=time.monotonic()-start,
                          sdl_frame_input=args.sdl_frame_input, viewer_gpu_priority=args.viewer_gpu_priority, swapchain_latency_frames=args.swapchain_latency_frames, fragment_iterations=args.fragment_iterations,
                          no_input_control=args.no_input, validation=validation,
                          fixture_sha256=hashlib.sha256(fixture.read_bytes()).hexdigest(),
                          observer_sha256=hashlib.sha256(observer.read_bytes()).hexdigest(),
                          compositor_sha256=hashlib.sha256(sway.read_bytes()).hexdigest(),
                          synchronization=sync_backend.upper(), virtual_output_hz=240,
                          compositor_gpu_priority=args.compositor_gpu_priority, compositor_priorities=compositor_priorities,
                          compositor_priority_shim_sha256=hashlib.sha256(args.compositor_priority_shim.read_bytes()).hexdigest() if args.compositor_gpu_priority else None)
            if args.host_presentation:report['host_presentation']=linked
            (output/'report.json').write_text(json.dumps(report, indent=2)+'\n')
            print(json.dumps(dict(passed=True, mode=args.mode, method=observed['method'], response_ms=observed['response_ms'], host_presentation_response_ms=linked['response_ms'] if args.host_presentation else None, report=str(output/'report.json'))))
    except Exception as error:
        report = dict(schema=1, passed=False, mode=args.mode, error=str(error), error_type=type(error).__name__)
        (output/'report.json').write_text(json.dumps(report, indent=2)+'\n')
        print(json.dumps(report))
        return 1
    finally:
        if native_environment is not None:
            subprocess.run([str(proton/'files/bin/wineserver'), '-k'],
                           env=dict(native_environment, WINEPREFIX=str(output/'compatdata/pfx')),
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10)
        for child in reversed(children):
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGTERM)
                try: child.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    os.killpg(child.pid, signal.SIGKILL); child.wait(timeout=3)
        for log in logs: log.close()
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
