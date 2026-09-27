import importlib.util
from pathlib import Path
import tempfile
import unittest
import fcntl
import os
import json
from types import SimpleNamespace
import subprocess
import sys
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('dist_desktop', Path(__file__).parents[1] / 'distribution.py')
dist = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dist)


class FrozenSource(unittest.TestCase):
    def test_invalid_distribution_command_names_the_public_tool(self):
        result = subprocess.run([sys.executable, str(Path(__file__).parents[1] / 'distribution.py'), 'bogus'], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn('usage: rsi-app-tools dist', result.stderr)
        self.assertNotIn('usage: distribution.py', result.stderr)

    def test_exact_bytes_modes_and_internal_symlinks_survive_freeze(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / 'source'; root.mkdir()
            output = Path(temporary) / 'capture'; output.mkdir()
            (root / 'dirty.rs').write_bytes(b'dirty\x00bytes\n')
            (root / 'untracked script').write_bytes(b'#!/bin/sh\nprintf literal\n')
            (root / 'untracked script').chmod(0o755)
            (root / 'alias').symlink_to('dirty.rs')
            records = dist.capture(root, output, ['dirty.rs', 'untracked script', 'alias'])
            self.assertEqual((output / 'dirty.rs').read_bytes(), b'dirty\x00bytes\n')
            self.assertTrue(records['untracked script']['executable'])
            self.assertEqual(records['alias']['target'], 'dirty.rs')
            dist.verify_capture(output, records)
            (root / 'dirty.rs').write_bytes(b'new work after freezing')
            dist.verify_capture(output, records)
            (output / 'dirty.rs').chmod(0o644)
            (output / 'dirty.rs').write_bytes(b'changed frozen source')
            with self.assertRaisesRegex(ValueError, 'captured source bytes changed'):
                dist.verify_capture(output, records)

    def test_tauri_generated_schemas_are_build_outputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / 'source'; root.mkdir()
            output = Path(temporary) / 'capture'; output.mkdir()
            (root / 'file.rs').write_text('original')
            records = dist.capture(root, output, ['file.rs'])
            generated = output / 'apps/desktop/gen/schemas'
            generated.mkdir(parents=True)
            (generated / 'acl-manifests.json').write_text('{}')
            dist.verify_capture(output, records)
            (output / 'apps/desktop/injected.rs').write_text('not a generated schema')
            with self.assertRaisesRegex(ValueError, 'uncaptured source added'):
                dist.verify_capture(output, records)

    def test_changed_frozen_executable_mode_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / 'source'; root.mkdir()
            output = Path(temporary) / 'capture'; output.mkdir()
            (root / 'script').write_text('#!/bin/sh\n')
            (root / 'script').chmod(0o755)
            records = dist.capture(root, output, ['script'])
            (output / 'script').chmod(0o444)
            with self.assertRaisesRegex(ValueError, 'captured source mode changed'):
                dist.verify_capture(output, records)

    def test_new_source_after_capture_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / 'source'; root.mkdir()
            output = Path(temporary) / 'capture'; output.mkdir()
            (root / 'file.rs').write_text('original')
            records = dist.capture(root, output, ['file.rs'])
            (output / 'added.rs').write_text('unrecorded compiler input')
            with self.assertRaisesRegex(ValueError, 'uncaptured source added'):
                dist.verify_capture(output, records)

    def test_external_symlinks_reject_and_ignored_targets_remain_absent(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / 'source'; root.mkdir()
            output = Path(temporary) / 'capture'; output.mkdir()
            (Path(temporary) / 'outside').write_text('not an input')
            (root / 'escape').symlink_to('../outside')
            with self.assertRaisesRegex(ValueError, 'escapes capture'):
                dist.capture(root, output, ['escape'])
            (root / 'omitted').write_text('not selected')
            (root / 'alias').symlink_to('omitted')
            records = dist.capture(root, output, ['alias'])
            self.assertTrue((output / 'alias').is_symlink())
            self.assertFalse((output / 'alias').exists())
            self.assertFalse((output / 'omitted').exists())
            dist.verify_capture(output, records)

    def test_concurrent_source_change_invalidates_the_whole_capture(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / 'source'; root.mkdir()
            output = Path(temporary) / 'capture'; output.mkdir()
            (root / 'file').write_text('original')
            original = Path.write_bytes
            def change_after_copy(path, data):
                result = original(path, data)
                if path == output / 'file': original(root / 'file', b'concurrent change')
                return result
            with patch.object(Path, 'write_bytes', change_after_copy):
                with self.assertRaisesRegex(ValueError, 'source changed during capture'):
                    dist.capture(root, output, ['file'])



class PublicationLifecycle(unittest.TestCase):
    def test_concurrent_producer_waits_for_exclusive_build_owner(self):
        with tempfile.TemporaryDirectory() as temporary:
            cache = Path(temporary)
            script = '''import importlib.util, pathlib, sys
spec = importlib.util.spec_from_file_location('distribution', sys.argv[1])
module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
print('waiting', flush=True)
with module.build_lock(pathlib.Path(sys.argv[2])): print('entered', flush=True)
'''
            with dist.build_lock(cache):
                child = subprocess.Popen([sys.executable, '-c', script, str(Path(dist.__file__)), str(cache)], stdout=subprocess.PIPE, text=True)
                self.assertEqual(child.stdout.readline().strip(), 'waiting')
                with self.assertRaises(subprocess.TimeoutExpired): child.wait(timeout=0.1)
            output, _ = child.communicate(timeout=5)
            self.assertEqual(child.returncode, 0)
            self.assertEqual(output.strip(), 'entered')

    def test_unchanged_capture_preserves_inode_and_timestamp_and_removes_deleted_sources(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / 'source'; root.mkdir()
            cache = Path(temporary) / 'capture'
            (root / 'keep').write_text('stable')
            (root / 'remove').write_text('old')
            first = dist.refresh(root, cache, ['keep', 'remove'], {})
            before = (cache / 'keep').stat()
            (root / 'remove').unlink()
            second = dist.refresh(root, cache, ['keep'], first)
            self.assertEqual(before.st_mtime_ns, (cache / 'keep').stat().st_mtime_ns)
            self.assertEqual(before.st_ino, (cache / 'keep').stat().st_ino)
            self.assertFalse((cache / 'remove').exists())
            dist.verify_capture(cache, second)

    def test_collection_preserves_current_previous_active_and_latest_failure(self):
        with tempfile.TemporaryDirectory() as temporary:
            cache = Path(temporary)
            for name in ['001', '002', '003', '004', '005', '006']:
                bundle = cache / 'generations' / name / 'bundle'; bundle.mkdir(parents=True)
                (bundle / '.rsi-generation.lock').touch()
                if name < '005': (bundle / 'receipt.json').write_text('{}')
            (cache / 'current').symlink_to('generations/004/bundle')
            with (cache / 'generations/001/bundle/.rsi-generation.lock').open('rb') as lease:
                fcntl.flock(lease, fcntl.LOCK_SH)
                self.assertEqual(set(dist.collect(cache)), {'002', '005'})
                self.assertTrue((cache / 'generations/001').exists())
            self.assertEqual(dist.collect(cache), ['001'])
            self.assertEqual({p.name for p in (cache / 'generations').iterdir()}, {'003', '004', '006'})

    def test_repeated_failed_managed_builds_keep_only_latest_failure_and_live_generations(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            cache = root / 'target/rsi-app'
            current = cache / 'generations/100/bundle'; current.mkdir(parents=True)
            (current / 'receipt.json').write_text('{}')
            (cache / 'current').symlink_to('generations/100/bundle')
            args = SimpleNamespace(kind='web', output=None, debug=True)
            with patch.object(dist, 'build_captured', side_effect=RuntimeError('fixture failure')), patch.object(dist.time, 'time_ns', side_effect=[200, 300, 400]):
                for _ in range(3):
                    with dist.build_lock(cache), self.assertRaisesRegex(RuntimeError, 'fixture failure'):
                        dist.build(root, cache, args, None)
            self.assertEqual({p.name for p in (cache / 'generations').iterdir()}, {'100', '400'})
            self.assertEqual(os.readlink(cache / 'current'), 'generations/100/bundle')
            self.assertTrue((cache / 'generations/400/failure.json').is_file())

    def test_failed_build_never_replaces_current(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); (root / 'Cargo.toml').write_text('[workspace]')
            cache = root / 'target/rsi-app'; (cache / 'generations/old/bundle').mkdir(parents=True)
            (cache / 'generations/old/bundle/receipt.json').write_text('{}')
            (cache / 'current').symlink_to('generations/old/bundle')
            args = SimpleNamespace(kind='web', output=None, debug=True)
            def output(command, **kwargs):
                if command[0] == 'git': return b'Cargo.toml\0'
                if command[:2] == ['rustc', '-vV']: return 'host: x86_64-unknown-linux-gnu\n'
                return 'test-tool-version'
            with patch.object(dist.subprocess, 'check_output', output), patch.object(dist.subprocess, 'run', side_effect=subprocess.CalledProcessError(1, ['cargo'])):
                with self.assertRaises(subprocess.CalledProcessError): dist.build(root, cache, args, None)
            self.assertEqual(os.readlink(cache / 'current'), 'generations/old/bundle')
            self.assertEqual([p.name for p in (cache / 'generations').iterdir() if (p / 'bundle/receipt.json').exists()], ['old'])
            failures = list((cache / 'generations').glob('*/failure.json'))
            self.assertEqual(len(failures), 1)
            self.assertEqual(json.loads(failures[0].read_text())['error_type'], 'CalledProcessError')


if __name__ == '__main__': unittest.main()
