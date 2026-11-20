"""Preserve and normalize a VKD3D trace interrupted by application shutdown."""
import json
import math
from pathlib import Path
import statistics


def normalize_trace(path: Path):
    raw = path.with_name(path.stem+'.raw.json')
    if raw.exists():
        raise FileExistsError(raw)
    path.rename(raw)
    events = 0
    incomplete = 0
    with raw.open() as source, path.open('x') as destination:
        destination.write('[\n')
        separator = ''
        for line in source:
            text = line.strip().removesuffix(',')
            if text in ('', '[', ']'):
                continue
            try:
                event = json.loads(text)
            except ValueError:
                incomplete += 1
                continue
            if not isinstance(event, dict):
                raise ValueError('unexpected VKD3D timeline record')
            destination.write(separator+json.dumps(event))
            separator = ',\n'
            events += 1
        destination.write('\n]\n')
    if not events:
        raise ValueError('VKD3D queue profile has no complete events')
    return dict(events=events, ignored_incomplete_records=incomplete,
                raw_trace=str(raw),
                scope='captured CPU submission/completion timeline; application shutdown may omit its tail')


def summarize_trace(path: Path, after_seconds=20):
    """Stream a normalized trace without loading its millions of events at once."""
    samples = {'callback': [], 'submission_overhead': [], 'submit': []}
    events = 0
    with path.open() as source:
        for line in source:
            record = line.strip().removesuffix(',')
            if record in ('', '[', ']'):
                continue
            event = json.loads(record)
            events += 1
            if event.get('ph') != 'X' or event.get('ts', 0) < after_seconds * 1000000:
                continue
            group = None
            if event.get('tid') == 'regions' and event.get('name') == 'CALLBACK':
                group = 'callback'
            elif event.get('tid') == 'overhead':
                group = 'submission_overhead'
            elif event.get('tid') == 'submit':
                group = 'submit'
            if group:
                duration = float(event['dur'])
                if not math.isfinite(duration) or duration < 0:
                    raise ValueError('invalid trace duration')
                samples[group].append(duration)
    groups = {}
    for name, values in samples.items():
        if not values:
            continue
        values.sort()
        def percentile(percent):
            position = (len(values) - 1) * percent / 100
            low, high = math.floor(position), math.ceil(position)
            return values[low] + (values[high] - values[low]) * (position - low)
        groups[name] = dict(count=len(values), p50_us=statistics.median(values),
                            p95_us=percentile(95), p99_us=percentile(99))
    if not groups.get('callback'):
        raise ValueError('trace contains no presentation callback samples after cutoff')
    return dict(scope='instrumented CPU-observed spans; not GPU execution timestamps',
                after_trace_seconds=after_seconds, events=events, groups=groups)


if __name__ == '__main__':
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('path', type=Path)
    parser.add_argument('--summary', action='store_true')
    args = parser.parse_args()
    print(json.dumps(summarize_trace(args.path) if args.summary else normalize_trace(args.path), indent=2))
