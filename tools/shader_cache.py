"""Content identities for private benchmark shader-cache seeds and exports."""
import hashlib
import json
from pathlib import Path
import shutil


def manifest(directory):
    directory = Path(directory).resolve(strict=True)
    if not directory.is_dir():
        raise ValueError('shader cache must be a directory')
    files = []
    for path in sorted(directory.rglob('*')):
        if path.is_symlink() or not (path.is_dir() or path.is_file()):
            raise ValueError('shader cache contains a link or special file')
        if path.is_file():
            digest = hashlib.sha256()
            with path.open('rb') as source:
                for chunk in iter(lambda: source.read(1024*1024), b''):
                    digest.update(chunk)
            files.append(dict(path=path.relative_to(directory).as_posix(),
                              bytes=path.stat().st_size, sha256=digest.hexdigest()))
    identity = hashlib.sha256(json.dumps(files, sort_keys=True, separators=(',', ':')).encode()).hexdigest()
    return dict(files=files, file_count=len(files), bytes=sum(row['bytes'] for row in files), sha256=identity)


def copy_seed(source, destination):
    before = manifest(source)
    if not before['bytes']:
        raise ValueError('shader cache seed is empty')
    shutil.copytree(source, destination, dirs_exist_ok=True)
    if manifest(destination) != before or manifest(source) != before:
        raise ValueError('shader cache seed copy changed contents')
    return dict(source=str(Path(source).resolve()), **before)


def export_gpu_cache(source, destination):
    source, destination = Path(source), Path(destination)
    destination.mkdir()
    if (source/'nvidia').is_dir():
        shutil.copytree(source/'nvidia', destination/'nvidia')
    for path in source.glob('vkd3d-proton.*'):
        if path.is_file():
            shutil.copyfile(path, destination/path.name)
    exported = manifest(destination)
    if not exported['bytes']:
        raise ValueError('GPU shader cache export is empty')
    return exported


def load_evidence(log):
    mapped = [line for line in log.splitlines() if 'Mapping read-only cache took ' in line]
    discarded = [line for line in log.splitlines() if 'Read-only cache is out of date, discarding it.' in line]
    return dict(verified=bool(mapped) and not discarded, mapped=mapped, discarded=discarded)


def cache_metadata(seed, log):
    evidence = load_evidence(log)
    # Cache reuse is advisory for launching. The study separately requires
    # positive evidence before assigning a warmed label to its measurements.
    return dict(shader_cache_seed=seed, shader_cache_load=evidence,
                shader_cache_unverified_seed=bool(seed and not evidence['verified']))
