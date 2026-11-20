#!/usr/bin/env python3
"""Interleave native/Venus/nvkvm owned Vulkan workloads; this does not measure game FPS."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import random
import statistics
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--build', type=Path, default=ROOT/'build/nvkvm-pv-display')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--repetitions', type=int, default=3)
    args = parser.parse_args()
    if not 1 <= args.repetitions <= 10:
        parser.error('--repetitions must be 1..10')
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    runtime = ROOT/'build/runtime'
    gpu = Path('/sys/class/drm/renderD128/device')
    expected_vendor = int((gpu/'vendor').read_text(), 16)
    expected_device = int((gpu/'device').read_text(), 16)
    if expected_vendor != 0x10de:
        raise ValueError('this NVIDIA study expects NVIDIA at /dev/dri/renderD128')
    fixture = ROOT/'build/target/release/legion-workload-probe'
    config = json.loads((runtime/'guest/workload.json').read_text())
    config.update(warmup_ms=1000, sample_ms=3000)
    template = config['cases'][0]
    config['cases'] = [dict(template, name=name, gpu_iterations=iterations)
                       for name, iterations in [('light1080', 1), ('shader64', 64), ('shader256', 256)]]
    config_path = output/'config.json'
    config_path.write_text(json.dumps(config, indent=2)+'\n')
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(('VK_', 'VN_', 'NVKVM_', 'LEGION_', 'MANGOHUD', 'DXVK_', 'VKD3D_', 'WINE', 'PROTON_'))
           and key not in ('LD_PRELOAD', 'LD_LIBRARY_PATH')}
    env.update(VK_DRIVER_FILES='/usr/share/vulkan/icd.d/nvidia_icd.json',
               VK_LOADER_LAYERS_DISABLE='~implicit~', LEGION_STARTUP_UI='log')
    order = []
    for repetition in range(args.repetitions):
        modes = ['native', 'venus', 'nvkvm']
        random.Random(repetition).shuffle(modes)
        order += [(mode, repetition) for mode in modes]
    records = []
    for mode, repetition in order:
        trial = output/f'{mode}-{repetition}'
        print(trial.name, flush=True)
        if mode == 'nvkvm':
            command = ['python3', str(ROOT/'tools/probe_nvkvm.py'), '--build', str(args.build.resolve()),
                       '--output', str(trial), '--config', str(config_path), '--cpus', str(os.cpu_count())]
            result_path = trial/'workload-result.json'
        else:
            trial.mkdir()
            result_path = trial/'workload-result.json'
            command = [str(fixture), str(config_path), str(result_path)]
            if mode == 'venus':
                manifest = json.loads((runtime/'runtime.json').read_text())
                command = [str(runtime/'bin/legion'), 'drm-native-probe', '--gpu-backend', 'venus',
                           '--qemu', str(runtime/'bin/legion-qemu'), '--qemu-data', str(runtime/'share/qemu'),
                           '--kernel', str(runtime/'guest/kernel'), '--init', str(runtime/'guest/init'),
                           '--userspace', str(runtime/'guest/usr'), '--virtiofsd', str(runtime/'bin/legion-virtiofsd'),
                           '--output', str(trial/'vm'), '--memory-mib', '4096',
                           '--render-node', '/dev/dri/renderD128', '--workload', str(fixture),
                           '--workload-config', str(config_path)]
                for module in manifest['modules']:
                    command += ['--module', str(runtime/module)]
                result_path = trial/'vm/frames/workload-result.json'
        (output/(trial.name+'-command.json')).write_text(json.dumps(command, indent=2)+'\n')
        with (output/(trial.name+'-launch.log')).open('w') as log:
            subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT, check=True, timeout=130)
        result = json.loads(result_path.read_text())
        if result['gpu_verified'] is not True:
            raise ValueError('fixture correctness check failed')
        for case in result['cases']:
            if case['gpu']['vendor_id'] != expected_vendor or case['gpu']['device_id'] != expected_device:
                raise ValueError('unexpected GPU; selected hardware differs between transports')
            records.append(dict(mode=mode, repetition=repetition, name=case['case']['name'],
                                fps=case['frames_per_second'], result=str(result_path),
                                checksum=case['gpu_known_answer_hash']))
    summary = []
    for case in config['cases']:
        rows = [row for row in records if row['name'] == case['name']]
        if len({row['checksum'] for row in rows}) != 1:
            raise ValueError('rendering checksum differs between transports')
        medians = {mode: statistics.median(row['fps'] for row in rows if row['mode'] == mode)
                   for mode in ('native', 'venus', 'nvkvm')}
        summary.append(dict(name=case['name'], median_fps=medians,
                            venus_native_ratio=medians['venus']/medians['native'],
                            nvkvm_native_ratio=medians['nvkvm']/medians['native']))
    with fixture.open('rb') as stream:
        fixture_hash = hashlib.file_digest(stream, 'sha256').hexdigest()
    report = dict(schema=1, scope='offscreen synthetic Vulkan, no presentation or Proton',
                  fixture_sha256=fixture_hash, expected_gpu=dict(vendor=expected_vendor, device=expected_device),
                  records=records, summary=summary)
    (output/'report.json').write_text(json.dumps(report, indent=2)+'\n')
    print(json.dumps(summary, indent=2))


if __name__ == '__main__':
    main()
