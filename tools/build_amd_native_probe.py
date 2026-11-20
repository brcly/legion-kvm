#!/usr/bin/env python3
"""Build isolated RADV/AMDGPU native-context components with Venus disabled."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

from build import container_security
from build_venus_probe import source

ROOT = Path(__file__).resolve().parents[1]


def run(command, log):
    with log.open('w') as stream:
        subprocess.run([str(arg) for arg in command], check=True, stdout=stream, stderr=subprocess.STDOUT)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, default=ROOT/'build/amd-native')
    parser.add_argument('--jobs', type=int, default=4)
    args = parser.parse_args()
    if not 1 <= args.jobs <= 16:
        parser.error('--jobs must be 1..16')
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    records = json.loads((ROOT/'config/sources.json').read_text())
    for archive, directory in [('mesa-26.2.6.tar.xz', 'mesa-source'),
                               ('virglrenderer-1.3.0.tar.gz', 'renderer-source')]:
        record = records[archive]
        source(ROOT/'build/cache'/archive, record['url'], record['sha256'], output/directory)
    # RADV 26 requires newer AMDGPU headers than Debian's build image. Reuse
    # Legion's already-pinned portable libdrm package in this private prefix.
    package = 'libdrm-2.4.134-1-x86_64.pkg.tar.zst'
    drm_record = json.loads((ROOT/'config/host-packages.json').read_text())[package]
    archive = ROOT/'build/cache/host-abi'/package
    with archive.open('rb') as stream:
        if hashlib.file_digest(stream, 'sha256').hexdigest() != drm_record['sha256']:
            raise ValueError('private libdrm checksum mismatch')
    drm = output/'libdrm'
    drm.mkdir(exist_ok=True)
    run(['bsdtar', '-xf', archive, '-C', drm], output/'libdrm-extract.log')
    for path in (drm/'usr/lib/pkgconfig').glob('*.pc'):
        path.write_text(path.read_text().replace('prefix=/usr', 'prefix=/research/libdrm/usr'))
    container = ['podman', 'run', '--rm', '--userns=keep-id', *container_security('podman'),
                 '--user', f'{os.getuid()}:{os.getgid()}', '-e', 'HOME=/tmp',
                 '-e', 'PKG_CONFIG_PATH=/research/libdrm/usr/lib/pkgconfig',
                 '-v', f'{output}:/research:rw', 'localhost/legion-build:1']
    options = ['--buildtype=release', '--prefix=/usr', '--libdir=lib/x86_64-linux-gnu', '--wrap-mode=nodownload',
               '-Dvulkan-drivers=amd', '-Damdgpu-virtio=true', '-Dgallium-drivers=', '-Dplatforms=x11,wayland',
               '-Dllvm=disabled', '-Dglx=disabled', '-Degl=disabled', '-Dgbm=disabled',
               '-Dopengl=false', '-Dgles1=disabled', '-Dgles2=disabled', '-Dbuild-tests=false',
               '-Dvideo-codecs=', '-Dvalgrind=disabled', '-Dlibunwind=disabled', '-Dlmsensors=disabled']
    configure = ['meson', 'setup', '/research/mesa-build', '/research/mesa-source', *options]
    if (output/'mesa-build/build.ninja').exists():
        configure.append('--reconfigure')
    run([*container, *configure], output/'mesa-configure.log')
    run([*container, 'ninja', '-C', '/research/mesa-build', '-j', args.jobs], output/'mesa-build.log')
    run([*container, 'meson', 'install', '-C', '/research/mesa-build', '--destdir', '/research/stage', '--no-rebuild'], output/'mesa-install.log')
    configure = ['meson', 'setup', output/'renderer-build', output/'renderer-source',
                 '--buildtype=release', '--prefix='+str(output/'renderer-prefix'), '--libdir=lib',
                 '--wrap-mode=nodownload', '-Dvenus=false', '-Ddrm-renderers=amdgpu-experimental',
                 '-Dtests=false', '-Dplatforms=egl']
    if (output/'renderer-build/build.ninja').exists():
        configure.append('--reconfigure')
    run(configure, output/'renderer-configure.log')
    run(['ninja', '-C', output/'renderer-build', '-j', args.jobs], output/'renderer-build.log')
    run(['meson', 'install', '-C', output/'renderer-build', '--no-rebuild'], output/'renderer-install.log')
    driver = output/'stage/usr/lib/x86_64-linux-gnu/libvulkan_radeon.so'
    renderer = output/'renderer-prefix/lib/libvirglrenderer.so.1'
    def digest(path):
        with path.open('rb') as stream:
            return hashlib.file_digest(stream, 'sha256').hexdigest()
    record = dict(schema=1, backend='amdgpu_drm_native_context', venus=False, mesa='26.2.6',
                  renderer='1.3.0', libdrm=drm_record, mesa_options=options, sources={name: records[name] for name in
                    ['mesa-26.2.6.tar.xz', 'virglrenderer-1.3.0.tar.gz']},
                  guest_driver_sha256=digest(driver), renderer_sha256=digest(renderer))
    (output/'build.json').write_text(json.dumps(record, indent=2)+'\n')
    print('Guest RADV:', driver)
    print('Host renderer:', renderer)


if __name__ == '__main__':
    main()
