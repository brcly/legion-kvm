#!/usr/bin/env python3
"""Owned VM D-Bus input to physical-compositor feedback; no desktop input injection."""
import argparse
import json
from pathlib import Path
import random
import signal
import subprocess
import time

from PIL import Image
from link_frame_presentation import link
from measure_input_latency import summarize

ROOT=Path(__file__).resolve().parents[1]


def barcode(path):
    with Image.open(path) as source:
        image=source.convert('RGB');width,height=image.size
        if width<64 or height<32:return None
        word=0
        for bit in range(32):
            center=width*(bit*2+1)//64;state=None
            for dx in [-1,0,1]:
                pixel=image.getpixel((center+dx,height//2));low,high=min(pixel),max(pixel)
                if high-low>12 or (low<128 and high>80) or (128<=low<180):return None
                value=int(low>=128)
                if state is not None and state!=value:return None
                state=value
            word|=state<<bit
        return word&65535 if word>>16==0xa5c3 else None


def physical_feedback(joined,records):
    frames={row['frame']:row for row in records if row.get('event')=='frame'}
    selected=[frames[sample['viewer_update']] for sample in joined['samples']]
    flags=[row['flags'] for row in selected]
    if not flags or any(flag&5!=5 for flag in flags):
        raise ValueError('matched responses lack physical vsync/hardware-completion flags')
    refresh=[row.get('refresh_ns',0) for row in selected]
    return dict(physical_flag_counts={str(flag):flags.count(flag) for flag in set(flags)},
                physical_refresh_ns_counts={str(value):refresh.count(value) for value in set(refresh)})


def capture_owned(vm,output,name,expected,child):
    deadline=time.monotonic()+15;requested=time.monotonic_ns();attempt=0
    while time.monotonic()<deadline:
        if child.poll() is not None:raise RuntimeError('owned VM ended before image verification')
        try:
            command=json.loads((vm/'viewer-command.json').read_text())
            title=command[command.index('--title')+1]
            found=subprocess.run([str(ROOT/'build/venus/capture-toplevel'),title],text=True,capture_output=True,timeout=3,check=True)
            token=found.stdout.strip()
            if not token or '\n' in token:raise RuntimeError('owned toplevel is missing or ambiguous')
            path=output/f'{name}-{attempt:03}.png';attempt+=1
            subprocess.run(['grim','-T',token,str(path)],capture_output=True,timeout=3,check=True)
            tag=barcode(path)
            if tag==expected:return dict(path=str(path),sequence=tag,capture_requested_ns=requested,time_ns=time.monotonic_ns())
        except (FileNotFoundError,subprocess.SubprocessError):pass
        time.sleep(.05)
    raise TimeoutError(f'owned physical-window barcode {expected} was not independently verified')


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--proton',type=Path,required=True)
    parser.add_argument('--fixture',type=Path,required=True)
    parser.add_argument('--build',type=Path,default=ROOT/'build/nvkvm-pv-dbus')
    parser.add_argument('--samples',type=int,default=40)
    parser.add_argument('--swapchain-latency-frames',type=int,choices=[1,2,3],default=2)
    parser.add_argument('--early-swap',action='store_true')
    parser.add_argument('--kms-acquire-wake',action='store_true')
    parser.add_argument('--fragment-iterations',type=int,default=65536)
    args=parser.parse_args()
    if not 20<=args.samples<=100:parser.error('samples must be20..100')
    output=args.output.resolve();output.mkdir(parents=True,exist_ok=False);vm=output/'vm'
    observer_path=output/'observer-report.json';events=[];child=None
    command=['python3',str(ROOT/'tools/probe_nvkvm.py'),'--build',str(args.build.resolve()),
        '--host-display','dbus','--window','--window-fullscreen','--proton',str(args.proton.resolve(strict=True)),
        '--fixture',str(args.fixture.resolve(strict=True)),'--proton-window','--sync-file-bridge','--kms-explicit-sync',
        '--viewer-presentation','--viewer-frame-tags','--fixture-frame-tags','--latency-probe','--runtime-feedback',
        '--latency-observer-report',str(observer_path),'--fixture-fragment-iterations',str(args.fragment_iterations),
        '--swapchain-latency-frames',str(args.swapchain_latency_frames),'--kms-hz','240','--cpus','16','--memory-mib','8192','--output',str(vm)]
    if args.early_swap:command.append('--viewer-early-swap')
    if args.kms_acquire_wake:command.append('--kms-acquire-wake')
    (output/'command.json').write_text(json.dumps(command,indent=2)+'\n')
    try:
        with (output/'launch.log').open('w') as log:
            child=subprocess.Popen(command,stdout=log,stderr=subprocess.STDOUT,start_new_session=True)
            feedback=vm/'runtime-feedback.log';deadline=time.monotonic()+90
            while True:
                text=feedback.read_text() if feedback.exists() else ''
                ready=[line for line in text.splitlines() if line.startswith('LEGION_FEEDBACK_READY ')]
                if ready:break
                if child.poll() is not None or time.monotonic()>deadline:raise RuntimeError('source runtime capability did not arrive')
                time.sleep(.02)
            if len(ready)!=1:raise ValueError('duplicate source runtime capability')
            frequency=int(ready[0].split()[1]);initial=capture_owned(vm,output,'initial',0,child)
            events.extend([dict(event='fixture_ready',magic=0xa5c3,time_ns=initial['time_ns']),dict(event='runtime_ready',qpc_frequency=frequency)])
            viewer=json.loads((vm/'viewer-command.json').read_text());address=viewer[viewer.index('--address')+1]
            keyboard=['gdbus','call','--address',address,'--dest','org.qemu','--object-path','/org/qemu/Display1/Console_0','--method']
            rng=random.Random(0x59d1e67b);time.sleep(1)
            for sequence in range(1,args.samples+1):
                time.sleep(rng.uniform(.08,.16));injected=time.monotonic_ns()
                try:
                    subprocess.run(keyboard+['org.qemu.Display1.Keyboard.Press','57'],check=True,capture_output=True,timeout=2)
                finally:
                    subprocess.run(keyboard+['org.qemu.Display1.Keyboard.Release','57'],check=True,capture_output=True,timeout=2)
                deadline=time.monotonic()+1
                while True:
                    lines=feedback.read_text().splitlines()
                    rows=[line.split() for line in lines if line.startswith(f'LEGION_FEEDBACK {sequence} ')]
                    if rows:break
                    if child.poll() is not None or time.monotonic()>deadline:raise TimeoutError(f'owned D-Bus input {sequence} timed out')
                    time.sleep(.002)
                if len(rows)!=1 or len(rows[0])!=10:raise ValueError('invalid source feedback record')
                seq,target,present,first,submitted,done,polled,fence,completed=map(int,rows[0][1:])
                observed=time.monotonic_ns()
                events.append(dict(event='sample',sequence=seq,injected_ns=injected,observed_ns=observed,response_ms=(observed-injected)/1e6,
                    target_present_id=target,feedback_present_id=present,source_input_qpc=first,source_submitted_qpc=submitted,
                    source_feedback_qpc=done,source_polled_qpc=polled,fence_value=fence,completed_fence_value=completed,
                    source_input_to_feedback_ms=(done-first)*1000/frequency,source_feedback_to_poll_ms=(polled-done)*1000/frequency))
                (output/'observer.jsonl').write_text(''.join(json.dumps(event)+'\n' for event in events))
            readbacks=[int(line.split()[2]) for line in feedback.read_text().splitlines() if line.startswith('LEGION_FEEDBACK_READBACK ')]
            first_qpc=next(event['source_input_qpc'] for event in events if event.get('event')=='sample')
            last_qpc=events[-1]['source_polled_qpc']
            if any(first_qpc<=value<=last_qpc for value in readbacks):raise ValueError('source image readback overlapped timed inputs')
            time.sleep(.25);final=capture_owned(vm,output,'final',args.samples,child)
            events.extend([dict(event='final_image_verified',**{key:final[key] for key in ['sequence','time_ns','capture_requested_ns']}),
                dict(event='runtime_measurement',timed_captures=0),dict(event='summary',passed=True,samples=args.samples)])
            (output/'observer.jsonl').write_text(''.join(json.dumps(event)+'\n' for event in events))
            observed=summarize(events,args.samples)
            observed.update(capture_verifier='owned physical toplevel initial/final barcodes',input_route='owned_console_dbus_keyboard',
                scope='host gdbus command dispatch to received source-runtime record; includes helper startup, transport and polling')
            observer_path.write_text(json.dumps(observed,indent=2)+'\n')
            if child.wait(timeout=100)!=0:raise RuntimeError('VM or hardware rendering qualification failed')
        report=json.loads((vm/'report.json').read_text());source=report['proton']
        if not report['passed'] or source.get('space_events')!=args.samples:raise ValueError('source did not confirm all owned input events')
        records=[json.loads(line) for line in (vm/'host-presentation.jsonl').read_text().splitlines()]
        joined=link(events,source,records,(vm/'viewer.log').read_text().splitlines())
        hardware=physical_feedback(joined,records)
        joined.update(scope='host owned-console gdbus command dispatch to compositor-reported physical presentation of tagged response',
            input_route='owned_console_dbus_keyboard',**hardware,
            limitations=['Input includes gdbus helper startup; bypasses physical devices and host SDL input routing',
                'Physical compositor vsync/hardware completion feedback, not optical input-to-photon',
                'GPU tag extraction and Wayland audit logging remain in the measured path',
                'Initial/final owned-window captures only; no timed desktop captures',
                'Host/guest absolute clocks are never subtracted; no native latency comparison'])
        (output/'host-latency.json').write_text(json.dumps(joined,indent=2)+'\n')
        (output/'report.json').write_text(json.dumps(dict(passed=True,early_swap=args.early_swap,kms_acquire_wake=args.kms_acquire_wake,swapchain_latency_frames=args.swapchain_latency_frames,host_presentation=joined,validation=report,images=[initial,final]),indent=2)+'\n')
        print(json.dumps(dict(passed=True,early_swap=args.early_swap,response_ms=joined['response_ms'],physical_flags=joined['physical_flag_counts'])))
        return 0
    except Exception as error:
        failure=dict(passed=False,error=str(error),error_type=type(error).__name__)
        (output/'report.json').write_text(json.dumps(failure,indent=2)+'\n');print(json.dumps(failure));return 1
    finally:
        if child is not None and child.poll() is None:
            child.send_signal(signal.SIGINT)
            try:child.wait(timeout=15)
            except subprocess.TimeoutExpired:child.terminate();child.wait(timeout=5)


if __name__=='__main__':raise SystemExit(main())
