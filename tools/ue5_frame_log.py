"""Analyze the owned benchmark's frame index / rounded instantaneous-FPS CSV."""
import csv
import io
import math
from pathlib import Path
import statistics
import time


def analyze_frame_log(path: Path):
    if not path.exists():
        return None
    return analyze_frame_text(path.read_text())


def analyze_frame_text(text):
    if not text.endswith('\n'):
        raise ValueError('incomplete benchmark frame log')
    rows = list(csv.DictReader(io.StringIO(text)))
    if not rows:
        raise ValueError('empty benchmark frame log')
    fps = []
    for index, row in enumerate(rows):
        if row.get('FPS') is None or row.get('Time') is None:
            raise ValueError('incomplete benchmark frame log')
        value = float(row['FPS'])
        if int(row['Time']) != index or not math.isfinite(value) or value <= 0:
            raise ValueError('unexpected frame index or FPS value in benchmark log')
        fps.append(value)
    periods = sorted(1000/value for value in fps)

    def percentile(fraction):
        position = (len(periods)-1)*fraction
        low = math.floor(position)
        high = math.ceil(position)
        return periods[low]+(periods[high]-periods[low])*(position-low)

    return dict(samples=len(fps), mean_instantaneous_fps=statistics.mean(fps),
                estimated_trace_seconds=sum(periods)/1000,
                fps_from_logged_frame_periods=1000/statistics.mean(periods),
                estimated_frame_ms=dict(p50=percentile(.5), p95=percentile(.95), p99=percentile(.99)),
                method='frame periods reconstructed from rounded application FPS; not GPU timestamps')


def wait_for_frame_log(path: Path, score: float, timeout=15):
    """Read a complete CSV matching the score, including repeated rewrites."""
    if not math.isfinite(score) or score <= 0:
        raise ValueError('invalid benchmark score')
    deadline = time.monotonic()+timeout
    while True:
        try:
            before = path.stat()
            text = path.read_text()
            after = path.stat()
            if (before.st_size, before.st_mtime_ns) == (after.st_size, after.st_mtime_ns):
                result = analyze_frame_text(text)
                if abs(result['mean_instantaneous_fps']-score) <= .02:
                    return result
        except (FileNotFoundError, ValueError):
            pass  # Writer has not created or finished this snapshot yet.
        if time.monotonic() >= deadline:
            raise TimeoutError('no complete frame log matching the benchmark score')
        time.sleep(.05)


def collect_benchmark_result(application: Path, output: Path, timeout=15):
    """Preserve a consistent score/CSV pair before stopping a rewriting app."""
    frame_path = application/'BenchmarkResultsPerFrame.csv'
    score_path = application/'BenchmarkResults.txt'
    deadline = time.monotonic()+timeout
    while True:
        try:
            score_text = score_path.read_text()
            lines = score_text.splitlines()
            if len(lines) < 2:
                raise ValueError('incomplete benchmark score')
            score = float(lines[0])
            before = frame_path.stat()
            frame_text = frame_path.read_text()
            after = frame_path.stat()
            if ((before.st_size, before.st_mtime_ns) != (after.st_size, after.st_mtime_ns)
                    or score_path.read_text() != score_text):
                raise ValueError('benchmark results changed during capture')
            frame_log = analyze_frame_text(frame_text)
            if not math.isfinite(score) or score <= 0 or abs(frame_log['mean_instantaneous_fps']-score) > .02:
                raise ValueError('inconsistent benchmark score and frame log')
            # The application can immediately truncate and rewrite its originals
            # again. Preserve the validated bytes, not a subsequent file read.
            (output/'BenchmarkResults.snapshot.txt').write_text(score_text)
            (output/'BenchmarkResultsPerFrame.snapshot.csv').write_text(frame_text)
            return dict(passed=True, benchmark_fps=score, details=lines[1:], frame_log=frame_log)
        except (FileNotFoundError, ValueError):
            if time.monotonic() >= deadline:
                raise TimeoutError('no consistent benchmark result snapshot')
            time.sleep(.05)


if __name__ == '__main__':
    import json
    import sys
    print(json.dumps(collect_benchmark_result(Path(sys.argv[1]), Path(sys.argv[2]))))
