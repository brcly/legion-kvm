"""Warm labels require copied bytes and positive loader evidence."""
from pathlib import Path
import tempfile
import unittest
from shader_cache import copy_seed, export_gpu_cache, load_evidence, manifest, cache_metadata


class ShaderCacheTests(unittest.TestCase):
    def test_seed_is_private_and_content_identity_tracks_changes(self):
        with tempfile.TemporaryDirectory() as directory:
            source=Path(directory)/'source';source.mkdir()
            (source/'shader.cache.write').write_bytes(b'cached shader')
            (source/'nvidia').mkdir();(source/'nvidia/driver.bin').write_bytes(b'driver')
            target=Path(directory)/'private'
            copied=copy_seed(source,target)
            self.assertEqual(manifest(source),manifest(target))
            (target/'shader.cache.write').write_bytes(b'new shader')
            self.assertNotEqual(manifest(target)['sha256'],copied['sha256'])
            self.assertEqual(manifest(source)['sha256'],copied['sha256'])

    def test_empty_or_linked_seeds_cannot_qualify(self):
        with tempfile.TemporaryDirectory() as directory:
            source=Path(directory)/'source';source.mkdir()
            with self.assertRaises(ValueError):copy_seed(source,Path(directory)/'private')
            (source/'link').symlink_to('/dev/null')
            with self.assertRaises(ValueError):manifest(source)

    def test_warm_requires_mapping_and_no_stale_cache_discard(self):
        self.assertFalse(load_evidence('Attempting to load disk cache')['verified'])
        self.assertFalse(load_evidence('Failed to map read-only cache: shader.cache.')['verified'])
        self.assertTrue(load_evidence('Mapping read-only cache took 1.23 ms.')['verified'])
        self.assertFalse(load_evidence('Mapping read-only cache took 1.23 ms.\nRead-only cache is out of date, discarding it.')['verified'])

    def test_export_excludes_font_and_proton_state(self):
        with tempfile.TemporaryDirectory() as directory:
            source=Path(directory)/'cache';source.mkdir()
            (source/'vkd3d-proton.game.cache.write').write_bytes(b'pipeline')
            (source/'nvidia').mkdir();(source/'nvidia/shader.bin').write_bytes(b'compiled')
            (source/'fontconfig').mkdir();(source/'fontconfig/font').write_bytes(b'font')
            (source/'protonfixes').mkdir();(source/'protonfixes/log').write_bytes(b'prefix state')
            exported=export_gpu_cache(source,Path(directory)/'seed')
            self.assertEqual({row['path'] for row in exported['files']},
                             {'vkd3d-proton.game.cache.write','nvidia/shader.bin'})

    def test_rejected_cache_does_not_fail_a_successful_launch_or_qualify_as_warm(self):
        report={'passed':True}
        report.update(cache_metadata({'sha256':'test seed'},
            'Read-only cache is out of date, discarding it.'))
        self.assertTrue(report['passed'])
        self.assertTrue(report['shader_cache_unverified_seed'])
        self.assertFalse(report['shader_cache_load']['verified'])
        self.assertFalse(cache_metadata(None,'')['shader_cache_unverified_seed'])


if __name__=='__main__':unittest.main()
