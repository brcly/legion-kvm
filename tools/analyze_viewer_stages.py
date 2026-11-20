#!/usr/bin/env python3
"""Validate bounded host CPU stage observations; not GPU or presentation timing."""
import argparse
import json
import math
from pathlib import Path
import statistics

FIELDS = ['begin', 'state', 'draw', 'tag', 'fence', 'flushed', 'wait_begin',
          'wait_end', 'post_wait', 'swap_begin', 'swap_end']
STAGES = ['state_setup', 'draw_call', 'tag_call', 'fence_creation', 'flush_call',
          'wait_setup', 'fence_wait', 'post_wait', 'pre_swap', 'swap_call']
SIGNALED = 0x911a
TIMEOUT = 0x911b
SATISFIED = 0x911c


def distribution(values):
    values = sorted(values)
    position = (len(values) - 1) * .95
    low, high = math.floor(position), math.ceil(position)
    return dict(p50=statistics.median(values),
                p95=values[low] + (values[high] - values[low]) * (position-low),
                maximum=max(values), mean=statistics.mean(values))


def analyze(records, expected_frames=None):
    if len(records) < 2 or records[-1].get('event') != 'summary':
        raise ValueError('stage profile is missing its completed summary')
    summary, rows = records[-1], records[:-1]
    if (summary.get('clock') != 'CLOCK_MONOTONIC'
            or summary.get('writing') != 'after-rendering'
            or summary.get('limit') != 32768
            or summary.get('frames') != len(rows) or not 0 < len(rows) <= 32768
            or expected_frames is not None and expected_frames != len(rows)):
        raise ValueError('stage profile count, bound or clock is invalid')
    spans = {name: [] for name in STAGES}
    totals, waits, timeouts, gpu = [], [], [], []
    previous_end = 0
    already_signaled = 0
    for expected, row in enumerate(rows, 1):
        if row.get('event') != 'stage' or row.get('frame') != expected:
            raise ValueError('stage profile frame sequence is incomplete')
        points = [row[name+'_ns'] for name in FIELDS]
        if points[0] <= previous_end or any(right < left for left, right in zip(points, points[1:])):
            raise ValueError('stage profile clocks are zero or reordered')
        previous_end = points[-1]
        calls, expired = row['wait_calls'], row['timeouts']
        if (row['final_status'] not in [SIGNALED, SATISFIED]
                or not 0 <= expired < calls or calls != expired+1
                or row['first_status'] != (TIMEOUT if expired else row['final_status'])):
            raise ValueError('stage profile lacks successful original fence completion')
        already_signaled += row['first_status'] == SIGNALED
        waits.append(calls)
        timeouts.append(expired)
        elapsed = row['gpu_bracket_ns']
        if elapsed < 0:
            raise ValueError('invalid GPU bracket duration')
        gpu.append(elapsed/1000)
        totals.append((points[-1]-points[0])/1e6)
        for name, left, right in zip(STAGES, points, points[1:]):
            spans[name].append((right-left)/1e6)
    if any(gpu) and not all(gpu):
        raise ValueError('GPU bracket instrumentation is incomplete')
    return dict(passed=True, frames=len(rows), total_ms=distribution(totals),
                gpu_bracket_us=distribution(gpu) if any(gpu) else None,
                cpu_stage_ms={name: distribution(values) for name, values in spans.items()},
                wait_calls=distribution(waits), timeouts=distribution(timeouts),
                fences_already_signaled_at_first_wait=already_signaled,
                scope='CPU CLOCK_MONOTONIC spans inside host viewer_draw; no GPU/host clock subtraction',
                limitations=['Fence wait can include earlier GPU dependencies, scheduling and CPU wakeup delay',
                             'GL API call time does not identify exclusive GPU execution time',
                             'Optional GPU bracket is a same-clock duration and may include dependencies/preemption',
                             'Swap return does not establish compositor presentation',
                             'Diagnostic runs are excluded from FPS comparisons'])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('profile', type=Path)
    parser.add_argument('--viewer-report', type=Path)
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    expected = None
    if args.viewer_report:
        report = json.loads(args.viewer_report.read_text())
        if not report['passed']:
            raise ValueError('viewer did not qualify')
        expected = report['gpu_completed_frames']
    result = analyze([json.loads(line) for line in args.profile.read_text().splitlines()], expected)
    rendered = json.dumps(result, indent=2)+'\n'
    if args.output:
        args.output.write_text(rendered)
    else:
        print(rendered, end='')


if __name__ == '__main__':
    main()
