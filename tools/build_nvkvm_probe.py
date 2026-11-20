#!/usr/bin/env python3
"""Build pinned nvkvm-pv research artifacts without installing host drivers or tools."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import urllib.request

from build import container_security
from package_runtime import libraries

ROOT = Path(__file__).resolve().parents[1]
REVISION = '39972874d22f5ba19e39827488b089e80a0c17f4'
REPOSITORY = 'https://github.com/reindertpelsma/nvkvm-pv.git'


def run(command, log, cwd=None, env=None):
    with log.open('w') as stream:
        subprocess.run([str(arg) for arg in command], cwd=cwd, env=env,
                       stdout=stream, stderr=subprocess.STDOUT, check=True)


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def extends(previous, current):
    return list(previous.items()) == list(current.items())[:len(previous)]


def apply_once(patch, tree, log):
    command = ['patch', '--batch', '--force', '--fuzz=0', '-p1', '-i', patch]
    reverse = subprocess.run([str(x) for x in command]+['--reverse', '--dry-run'],
                             cwd=tree, capture_output=True)
    if reverse.returncode:
        run([*command, '--dry-run'], log, cwd=tree)
        run(command, log, cwd=tree)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, default=ROOT/'build/nvkvm-pv-display')
    parser.add_argument('--upstream-only', action='store_true', help='omit Legion compatibility patches')
    parser.add_argument('--jobs', type=int, default=4)
    parser.add_argument('--dbus-display', action='store_true', help='build the isolated D-Bus display experiment')
    args = parser.parse_args()
    if not 1 <= args.jobs <= 16:
        parser.error('--jobs must be between 1 and 16')
    base = args.output.resolve()
    base.mkdir(parents=True, exist_ok=True)
    build_env = dict(os.environ)
    if args.dbus_display:
        # Host lacks gdbus-codegen on some distributions. Use the build image's
        # Python generator privately, without installing anything on the host.
        codegen = base/'codegen-lib'
        codegen.mkdir(exist_ok=True)
        run(['podman', 'run', '--rm', '--userns=keep-id', *container_security('podman'),
             '--user', f'{os.getuid()}:{os.getgid()}', '-v', f'{codegen}:/out:rw',
             'localhost/legion-build:1', 'cp', '-r', '/usr/share/glib-2.0/codegen', '/out/'],
            base/'codegen-stage.log')
        helper_bin = base/'codegen-bin'
        helper_bin.mkdir(exist_ok=True)
        helper = helper_bin/'gdbus-codegen'
        helper.write_text('#!/usr/bin/env python3\nimport sys\nsys.path.insert(0, '+repr(str(codegen))+')\nfrom codegen.codegen_main import codegen_main\ncodegen_main()\n')
        helper.chmod(0o755)
        build_env['PATH'] = str(helper_bin)+os.pathsep+build_env.get('PATH', '')
    source = base/'source'
    if not source.exists():
        run(['git', 'init', source], base/'clone.log')
        run(['git', '-C', source, 'fetch', '--depth=1', REPOSITORY, REVISION], base/'fetch.log')
        run(['git', '-C', source, 'checkout', '--detach', 'FETCH_HEAD'], base/'checkout.log')
    actual = subprocess.check_output(['git', '-C', source, 'rev-parse', 'HEAD'], text=True).strip()
    if actual != REVISION or subprocess.check_output(['git', '-C', source, 'diff', 'HEAD']):
        raise ValueError('nvkvm source must be the clean pinned revision')
    patches = [] if args.upstream_only else sorted((ROOT/'tools/nvkvm-patches').glob('*.patch'))
    patch_identity = {path.name: sha(path) for path in patches}
    qemu_patches = [] if args.upstream_only else sorted((ROOT/'tools/nvkvm-qemu-patches').glob('*.patch'))
    qemu_patch_identity = {path.name: sha(path) for path in qemu_patches}
    if patches:
        overlay = base/'patched-source'
        stamp = base/'patched-source.json'
        identity = dict(revision=REVISION, patches=patch_identity)
        if not stamp.exists() or json.loads(stamp.read_text()) != identity:
            if overlay.exists():
                previous = json.loads(stamp.read_text()) if stamp.exists() else {}
                if previous.get('revision') != REVISION or not extends(previous.get('patches', {}), patch_identity):
                    raise ValueError('existing patches changed; use a new --output directory')
                pending = patches[len(previous['patches']):]
            else:
                snapshot = base/'upstream-source.tar'
                run(['git', '-C', source, 'archive', 'HEAD', '--output', snapshot], base/'archive.log')
                overlay.mkdir()
                with tarfile.open(snapshot) as archive_file:
                    archive_file.extractall(overlay, filter='data')
                pending = patches
            for path in pending:
                apply_once(path, overlay, base/('local-'+path.stem+'.log'))
            stamp.write_text(json.dumps(identity, indent=2)+'\n')
        source = overlay
    run(['make', '-C', source/'src/stub', '-j'+str(args.jobs)], base/'stub-build.log')
    run(['make', '-C', source/'src/broker', '-j'+str(args.jobs), 'nvkvm-display-broker'], base/'broker-build.log')
    run(['make', '-C', source/'tests/unit', '-j'+str(args.jobs)], base/'unit-build.log')
    run(['timeout', '60', 'make', '-C', source/'tests/unit', 'check'], base/'unit-check.log')
    record = json.loads((ROOT/'config/sources.json').read_text())['qemu-11.1.2.tar.xz']
    archive = base/'qemu-11.1.2.tar.xz'
    cached = ROOT/'build/cache'/archive.name
    if not archive.exists():
        if cached.exists():
            shutil.copy2(cached, archive)
        else:
            with urllib.request.urlopen(record['url'], timeout=120) as src, archive.open('wb') as dst:
                shutil.copyfileobj(src, dst)
    if sha(archive) != record['sha256']:
        raise ValueError('QEMU archive checksum mismatch')
    tree = base/'qemu-11.1.2'
    recipe = dict(nvkvm_revision=REVISION, qemu_archive_sha256=record['sha256'],
                  preparation_schema=1, local_patches=patch_identity, qemu_patches=qemu_patch_identity,
                  dbus_display=args.dbus_display)
    marker = base/'prepared.json'
    prepared = json.loads(marker.read_text()) if marker.exists() else {}
    compatible = (prepared.get('nvkvm_revision') == REVISION
                  and prepared.get('qemu_archive_sha256', prepared.get('qemu_sha256')) == record['sha256']
                  and prepared.get('preparation_schema', 1) == 1
                  and prepared.get('dbus_display', False) == args.dbus_display
                  and prepared.get('local_patches', {}) == patch_identity
                  and prepared.get('qemu_patches', {}) == qemu_patch_identity)
    if not compatible:
        if tree.exists():
            if (prepared.get('nvkvm_revision') != REVISION
                or prepared.get('dbus_display', False) != args.dbus_display
                or prepared.get('qemu_archive_sha256') != record['sha256']
                or not extends(prepared.get('local_patches', {}), patch_identity)
                or not extends(prepared.get('qemu_patches', {}), qemu_patch_identity)):
                raise ValueError('existing QEMU preparation changed; use a new --output directory')
            new_tree = False
        else:
            new_tree = True
        if new_tree:
            with tarfile.open(archive) as tar:
                # The release includes an unused EDK2 X11 symlink to a host absolute
                # path. Exclude it; all other members use Python's safe data filter.
                members = [member for member in tar
                           if not member.issym() or not member.linkname.startswith('/')]
                tar.extractall(base, members=members, filter='data')
            for patch in sorted((source/'patches').glob('*.patch')):
                run(['patch', '--batch', '--fuzz=0', '-p1', '-i', patch],
                    base/(patch.stem+'.log'), cwd=tree)
        pending_qemu = qemu_patches if new_tree else qemu_patches[len(prepared.get('qemu_patches', {})):]
        for patch in pending_qemu:
            apply_once(patch, tree, base/('qemu-local-'+patch.stem+'.log'))
        destination = tree/'hw/misc'
        for pattern in ('*.c', '*.h'):
            for path in (source/'src/qemu').glob(pattern):
                text = re.sub(r'"\.\./\.\./src/(?:common|abi)/(\w+\.h)"',
                              r'"nvkvm_inc/\1"', path.read_text())
                (destination/path.name).write_text(text)
        inc = destination/'nvkvm_inc'
        inc.mkdir(exist_ok=True)
        for directory in ('src/common', 'src/abi'):
            for path in (source/directory).glob('*.h'):
                (inc/path.name).write_text(path.read_text().replace(
                    '#include <linux/types.h>', '#include "linux_types_compat.h"'))
        shutil.copy2(source/'src/qemu/nvkvm_linux_types.h', inc/'linux_types_compat.h')
        shutil.copy2(source/'src/stub/nvkvm_stub_bin.h', destination/'nvkvm_stub_bin.h')
        if new_tree or not (tree/'build/config-host.mak').exists():
            run(['./configure', '--target-list=x86_64-softmmu', '--enable-kvm', '--disable-werror',
                 '--enable-sdl', '--disable-gtk', '--enable-opengl', '--disable-virglrenderer',
                 '--disable-vnc', '--disable-docs', '--disable-tools', '--disable-guest-agent',
                 '--disable-download', '--extra-cflags=-DNVKVM_STUB_EMBEDDED',
                 *(['--enable-dbus-display', '--enable-gio'] if args.dbus_display else [])],
                base/'qemu-configure.log', cwd=tree, env=build_env)
        marker.write_text(json.dumps(recipe, indent=2)+'\n')
    run(['ninja', '-C', tree/'build', '-j'+str(args.jobs), 'qemu-system-x86_64'],
        base/'qemu-build.log', env=build_env)
    devices = subprocess.check_output([tree/'build/qemu-system-x86_64', '-device', 'help'], text=True)
    if 'virtio-nvgpu-pci' not in devices or 'nvkvm-gpu' not in devices:
        raise ValueError('built QEMU is missing the research devices')
    # Existing kernel output refers to /build/sources: use its original toolchain
    # and mount path. Only the guest module is built; nothing is loaded on the host.
    container = ['podman', 'run', '--rm', '--userns=keep-id', *container_security('podman'),
                 '--user', f'{os.getuid()}:{os.getgid()}',
                 '-v', f'{ROOT / "build"}:/build:rw', '-v', f'{base}:/research:rw', '-v', f'{ROOT}:/src:ro',
                 'localhost/legion-build:1']
    run([*container, 'make', '-C', '/research/'+source.name+'/src/guest', 'KDIR=/build/kernel',
         '-j'+str(args.jobs)], base/'guest-build.log')
    target = '/build/'+str(base.relative_to(ROOT/'build'))+'/rust-target' if base.is_relative_to(ROOT/'build') else '/research/rust-target'
    rust = [*container[:-1], '-e', 'HOME=/build/home', '-e', 'CARGO_HOME=/build/cargo',
            '-e', 'CARGO_TARGET_DIR='+target, container[-1]]
    run([*rust, 'cargo', 'test', '--locked', '-p', 'legion-guest-compositor', '-j'+str(args.jobs)],
        base/'compositor-test.log')
    run([*rust, 'cargo', 'build', '--locked', '--release', '-p', 'legion-guest-compositor', '-j'+str(args.jobs)],
        base/'compositor-build.log')
    (base/'fixtures').mkdir(exist_ok=True)
    run([*container, 'gcc', '-O2', '-Wall', '-Wextra', '-Werror',
         '-Wl,--export-dynamic', '/src/tools/nvkvm_sparse_probe.c',
         '-o', '/research/fixtures/sparse-probe', '-lvulkan'], base/'sparse-build.log')
    run([*container, 'gcc', '-shared', '-fPIC', '-O2', '-Wall', '-Wextra', '-Werror',
         '/src/tools/nvkvm_ioctl_profile.c', '-o', '/research/fixtures/ioctl-profile.so'],
        base/'ioctl-profile-build.log')
    run([*container, 'musl-gcc', '-static', '-O2', '-pthread', '-Wall', '-Wextra', '-Werror',
         '/src/tools/nvkvm_poll_lifetime_probe.c', '-o', '/research/fixtures/poll-lifetime-probe'],
        base/'poll-lifetime-build.log')
    run([*container, 'gcc', '-O2', '-Wall', '-Wextra', '-Werror',
         '/src/tools/nvkvm_sync_caps.c', '-o', '/research/fixtures/sync-caps', '-lvulkan'],
        base/'sync-caps-build.log')
    run([*container, 'gcc', '-O2', '-Wall', '-Wextra', '-Werror',
         '/src/tools/nvkvm_sync_file_probe.c', '-o', '/research/fixtures/sync-file-probe',
         '-pthread', '-lvulkan'], base/'sync-file-build.log')
    run([*container, 'gcc', '-O2', '-Wall', '-Wextra', '-Werror',
         '/src/tools/nvkvm_memory_probe.c', '-o', '/research/fixtures/memory-probe',
         '-lvulkan'], base/'memory-build.log')
    for name in ('gbmflip', 'gbmgl_present'):
        run([*container, 'gcc', '-O2', '-I/usr/include/libdrm',
             '/research/'+source.name+'/tests/perf/apps/'+name+'.c',
             '-o', '/research/fixtures/'+name, '-lgbm', '-ldrm', '-lEGL', '-lGLESv2'],
            base/(name+'-build.log'))
    version = subprocess.check_output(['nvidia-smi', '--query-gpu=driver_version',
                                      '--format=csv,noheader'], text=True).splitlines()[0].strip()
    env = dict(os.environ, SYSLIB='/usr/lib', NVKVM_DRIVER_VERSION=version)
    run(['bash', source/'scripts/make_host_bundle.sh', base], base/'host-bundle.log', env=env)
    bundle = base/f'host-libs-{version}'
    for name, path in libraries(list(bundle.glob('*.so.*'))).items():
        if not (bundle/name).exists():
            shutil.copy2(path, bundle/name)
    for path in Path('/usr/lib').glob('*.so*'):
        if path.is_symlink() and (bundle/path.resolve().name).is_file() and not (bundle/path.name).exists():
            (bundle/path.name).symlink_to(path.resolve().name)
    # This is a driver-independent open-source helper. A workstation package
    # may contain unconditional AVX-512 even without an ELF ISA requirement note.
    # Use the same portable Debian helper as Legion's build toolchain.
    helper = bundle/'libnvidia-egl-gbm.so.1.1.2'
    run([*container, 'cp', '/usr/lib/x86_64-linux-gnu/libnvidia-egl-gbm.so.1.1.2',
         '/research/'+bundle.name+'/'+helper.name], base/'portable-gbm.log')
    alias = bundle/'libnvidia-egl-gbm.so.1'
    alias.unlink(missing_ok=True)
    alias.symlink_to(helper.name)
    (bundle/'gbm').mkdir(exist_ok=True)
    gbm_backend = bundle/'gbm/nvidia-drm_gbm.so'
    gbm_backend.unlink(missing_ok=True)
    gbm_backend.symlink_to('../libnvidia-allocator.so.'+version)
    icd = json.loads(Path('/usr/share/vulkan/icd.d/nvidia_icd.json').read_text())
    icd['ICD']['library_path'] = f'/nvidia/libGLX_nvidia.so.{version}'
    (bundle/'icd.json').write_text(json.dumps(icd, indent=2)+'\n')
    run(['bash', source/'tools/abi_derive.sh', '--tags', version, '--jobs', '1',
         '--work', base/'abi', '--keep'], base/'abi.log')
    recipe.update(driver=version, guest_module_sha256=sha(source/'src/guest/nvkvm-guest.ko'),
                  qemu_binary_sha256=sha(tree/'build/qemu-system-x86_64'),
                  builder_sha256=sha(Path(__file__)), portable_gbm_sha256=sha(helper),
                  build_image=subprocess.check_output(['podman', 'image', 'inspect',
                      'localhost/legion-build:1', '--format', '{{.Id}}'], text=True).strip())
    recipe['compositor_source_sha256'] = hashlib.sha256(b''.join(
        str(path.relative_to(ROOT)).encode()+b'\0'+path.read_bytes()
        for path in sorted((ROOT/'crates/guest-compositor').rglob('*.rs')))).hexdigest()
    recipe['compositor_sha256'] = sha(base/'rust-target/release/legion-guest-compositor')
    recipe['sparse_probe_source_sha256'] = sha(ROOT/'tools/nvkvm_sparse_probe.c')
    recipe['sparse_probe_sha256'] = sha(base/'fixtures/sparse-probe')
    for fixture, filename in (('memory_probe', 'memory-probe'), ('ioctl_profile', 'ioctl-profile.so'),
                              ('poll_lifetime_probe', 'poll-lifetime-probe'),
                              ('sync_caps', 'sync-caps'),
                              ('sync_file_probe', 'sync-file-probe')):
        recipe[fixture+'_source_sha256'] = sha(ROOT/'tools'/('nvkvm_'+fixture+'.c'))
        recipe[fixture+'_sha256'] = sha(base/'fixtures'/filename)
    recipe['broker_binary_sha256'] = sha(source/'src/broker/nvkvm-display-broker')
    (base/'build.json').write_text(json.dumps(recipe, indent=2)+'\n')
    print('Research artifacts:', base)


if __name__ == '__main__':
    main()
