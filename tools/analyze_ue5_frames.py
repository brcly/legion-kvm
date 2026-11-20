#!/usr/bin/env python3
"""Locate slow frames using elapsed time reconstructed from benchmark FPS."""
import argparse
import csv
import io
import json
import math
from pathlib import Path

from ue5_frame_log import analyze_frame_text

THRESHOLDS = [20, 25, 1000/30, 50, 100]


def windows(text, window_seconds=10):
    if not math.isfinite(window_seconds) or window_seconds <= 0:
        raise ValueError('window length must be positive')
    whole = analyze_frame_text(text)
    bins = {}
    elapsed_ms = 0
    steady_periods = []
    for row in csv.DictReader(io.StringIO(text)):
        period = 1000/float(row['FPS'])
        if elapsed_ms >= 10000:
            steady_periods.append(period)
        index = int(elapsed_ms/(window_seconds*1000))
        bucket = bins.setdefault(index, dict(first_frame=int(row['Time']), frames=0, span_ms=0,
            slow_frames={f'over_{limit:g}_ms': 0 for limit in THRESHOLDS}))
        bucket['frames'] += 1
        bucket['span_ms'] += period
        for limit in THRESHOLDS:
            bucket['slow_frames'][f'over_{limit:g}_ms'] += period > limit
        elapsed_ms += period
    result = []
    for index, bucket in bins.items():
        result.append(dict(estimated_start_seconds=index*window_seconds,
            frame_period_fps=1000*bucket['frames']/bucket['span_ms'], **bucket))
    counts = {f'over_{limit:g}_ms': sum(bucket['slow_frames'][f'over_{limit:g}_ms'] for bucket in result)
              for limit in THRESHOLDS}
    steady = None
    if steady_periods:
        ordered = sorted(steady_periods)
        position = (len(ordered)-1)*.99
        low, high = math.floor(position), math.ceil(position)
        steady = dict(frames=len(ordered), frame_period_fps=1000*len(ordered)/sum(ordered),
            p99_ms=ordered[low]+(ordered[high]-ordered[low])*(position-low),
            over_25_ms_frames=sum(value>25 for value in ordered))
    return dict(passed=True, frames=whole['samples'], estimated_trace_seconds=elapsed_ms/1000,
        after_estimated_10_seconds=steady,
        slow_frames=counts, windows=result,
        scope='Approximate benchmark-relative elapsed windows, reconstructed from rounded instantaneous FPS',
        limitations=['CSV Time is a frame index, not a timestamp',
            'Whole crossing frames are assigned to the window containing their estimated start',
            'No GPU timing, absolute clock alignment or causal attribution to compilation'])


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('csv',type=Path)
    parser.add_argument('--output',type=Path)
    args=parser.parse_args()
    rendered=json.dumps(windows(args.csv.read_text()),indent=2)+'\n'
    if args.output:args.output.write_text(rendered)
    else:print(rendered,end='')


if __name__=='__main__':main()
