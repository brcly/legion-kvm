#!/usr/bin/env python3
"""Counterbalance private-compositor input-response trials and verify controls."""
import argparse
import json
from pathlib import Path
import statistics
import subprocess
import sys

ROOT=Path(__file__).resolve().parents[1]


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--proton', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--repetitions', type=int, default=2)
    parser.add_argument('--samples', type=int, default=40)
    args=parser.parse_args()
    if not 1<=args.repetitions<=3 or not 20<=args.samples<=100:parser.error('bounded repetitions/samples required')
    output=args.output.resolve();output.mkdir(parents=True,exist_ok=False)
    records=[]
    def trial(name,mode,delay=0,no_input=False):
        directory=output/name
        command=[sys.executable,str(ROOT/'tools/measure_input_latency.py'),'--mode',mode,
                 '--proton',str(args.proton.resolve()),'--output',str(directory),'--samples',str(args.samples)]
        if delay:command+=['--delay-ms',str(delay)]
        if no_input:command+=['--no-input']
        print(name,flush=True)
        with (output/(name+'.log')).open('w') as log:
            ret=subprocess.run(command,stdout=log,stderr=subprocess.STDOUT).returncode
        report=json.loads((directory/'report.json').read_text())
        if no_input:
            raw=[json.loads(line) for line in (directory/'observer.jsonl').read_text().splitlines()]
            summary=raw[-1]
            if ret==0 or report['passed'] or summary.get('error')!='input response timed out' or summary.get('samples')!=0:
                raise RuntimeError('no-input negative control incorrectly accepted a response')
            print('expected timeout confirmed',flush=True)
            return dict(name=name,expected_failure_verified=True,report=str(directory/'report.json'))
        if ret or not report['passed']:raise RuntimeError(f'{name} did not pass; inspect its report')
        row=dict(name=name,mode=mode,delay_ms=delay,response_ms=report['response_ms'],report=str(directory/'report.json'))
        print(str(row['response_ms']),flush=True)
        return row
    # Complete controls before treating response timings as measurements.
    control=trial('native-control','native')
    delayed=trial('native-delay-20ms','native',20)
    shift=delayed['response_ms']['p50']-control['response_ms']['p50']
    if not 15<=shift<=30:raise RuntimeError(f'20 ms source delay did not produce the expected shift: {shift}')
    negative=trial('no-input-control','native',no_input=True)
    (output/'controls.json').write_text(json.dumps(dict(passed=True,baseline=control,delay=delayed,
        observed_delay_shift_ms=shift,no_input=negative),indent=2)+'\n')
    for repeat in range(args.repetitions):
        modes=('native','sdl','broker')
        for mode in modes if repeat%2==0 else reversed(modes):
            row=trial(f'{mode}-{repeat}',mode);row['repeat']=repeat;records.append(row)
            (output/'records.json').write_text(json.dumps(records,indent=2)+'\n')
    summary={}
    for mode in ('native','sdl','broker'):
        values=[]
        for row in records:
            if row['mode']==mode:
                values += [s['response_ms'] for s in json.loads(Path(row['report']).read_text())['samples']]
        values.sort()
        def percentile(p):
            position=(len(values)-1)*p;low=int(position);high=min(low+1,len(values)-1)
            return values[low]+(values[high]-values[low])*(position-low)
        summary[mode]=dict(samples=len(values),p50_ms=statistics.median(values),p95_ms=percentile(.95),
                           p99_ms=percentile(.99),maximum_ms=max(values))
    report=dict(schema=1,passed=True,scope='controlled software input-to-captured-response latency; not physical input-to-photon',
                compositor='private headless Sway, NVIDIA GLES2, 1920x1080@240',
                controls=json.loads((output/'controls.json').read_text()),records=records,summary=summary,
                limitations=['Capture forces composition and includes observation delay',
                             'Native and VM paths differ in CPU policy and application environment',
                             'SDL is strict explicit sync; broker is the existing implicit path',
                             'Synthetic lightweight D3D12 scene, not general game latency'])
    (output/'report.json').write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(summary,indent=2))


if __name__=='__main__':main()
