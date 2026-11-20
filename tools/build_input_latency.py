#!/usr/bin/env python3
"""Build private diagnostic tools; never install compositor packages on the host."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import urllib.request

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--proton', type=Path, required=True)
    parser.add_argument('--output', type=Path, default=ROOT/'build/input-latency')
    parser.add_argument('--sway', type=Path, help='use an existing compositor instead of extracting a private package')
    args = parser.parse_args()
    output = args.output.resolve(); output.mkdir(parents=True, exist_ok=True)
    generated = output/'generated'; generated.mkdir(exist_ok=True)
    for name in ['wlr-screencopy-unstable-v1', 'virtual-keyboard-unstable-v1']:
        source = ROOT/'tools/input-latency/protocols'/(name+'.xml')
        for operation, suffix in [('client-header', '-client-protocol.h'), ('private-code', '-protocol.c')]:
            subprocess.run(['wayland-scanner', operation, str(source), str(generated/(name+suffix))], check=True)
    flags = subprocess.check_output(['pkg-config', '--cflags', '--libs', 'wayland-client', 'xkbcommon'], text=True).split()
    subprocess.run(['cc', '-O2', '-Wall', '-Wextra', '-Werror', '-Wno-misleading-indentation', '-I'+str(generated),
                    str(ROOT/'tools/input-latency/observer.c'),
                    *[str(generated/(name+'-protocol.c')) for name in ['wlr-screencopy-unstable-v1', 'virtual-keyboard-unstable-v1']],
                    '-o', str(output/'observer'), *flags], check=True)
    package = None
    if args.sway:
        sway = args.sway.resolve(strict=True)
    elif shutil.which('sway'):
        sway = Path(shutil.which('sway')).resolve()
    else:
        # Pacman's print mode is read-only. The host repository supplies both
        # checksum and URL; extraction and execution stay in this build tree.
        record = subprocess.check_output(['pacman', '-Sp', '--print-format', '%h %l', 'sway'], text=True).strip().splitlines()
        if len(record)!=1:
            raise ValueError('provide --sway: this host also needs compositor dependencies')
        expected, url = record[0].split(maxsplit=1)
        if len(expected)!=64 or not url.startswith('https://'):
            raise ValueError('invalid package identity')
        archive = output/'sway.pkg.tar.zst'
        if not archive.exists() or hashlib.sha256(archive.read_bytes()).hexdigest()!=expected:
            with urllib.request.urlopen(url, timeout=60) as source, archive.open('wb') as destination:
                shutil.copyfileobj(source, destination)
        if hashlib.sha256(archive.read_bytes()).hexdigest()!=expected:
            raise ValueError('private compositor checksum mismatch')
        package = dict(url=url, sha256=expected)
        root = output/'sway-root'; root.mkdir(exist_ok=True)
        subprocess.run(['tar', '-xf', str(archive), '--no-same-owner', '-C', str(root)], check=True)
        sway = root/'usr/bin/sway'
    subprocess.run(['python3', str(ROOT/'tools/build_proton_workload.py'), '--proton', str(args.proton.resolve()),
                    '--output', str(output/'fixture-link'), '--target-dir', str(output/'windows-target')], check=True)
    fixture = output/'windows-target/x86_64-pc-windows-msvc/release/legion-proton-workload.exe'
    metadata = dict(schema=1, compositor=str(sway), private_package=package,
                    compositor_version=subprocess.check_output([str(sway), '--version'], text=True).strip(),
                    compositor_sha256=hashlib.sha256(sway.read_bytes()).hexdigest(),
                    fixture_sha256=hashlib.sha256(fixture.read_bytes()).hexdigest(),
                    observer_sha256=hashlib.sha256((output/'observer').read_bytes()).hexdigest(),
                    source_sha256={str(path.relative_to(ROOT)):hashlib.sha256(path.read_bytes()).hexdigest()
                                   for path in [ROOT/'tools/input-latency/observer.c',
                                                ROOT/'tools/build_input_latency.py',
                                                *sorted((ROOT/'tools/input-latency/protocols').glob('*.xml')),
                                                *sorted((ROOT/'crates/proton-workload/src').glob('*')),
                                                ROOT/'crates/proton-workload/Cargo.toml']
                                   if path.is_file()})
    (output/'build.json').write_text(json.dumps(metadata, indent=2)+'\n')
    print(json.dumps(metadata, indent=2))


if __name__ == '__main__':
    main()
