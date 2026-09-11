import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('dist_desktop', Path(__file__).parents[1] / 'dist-desktop.py')
dist = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dist)


class FrozenSource(unittest.TestCase):
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
            generated = output / 'crates/rsi/desktop/gen/schemas'
            generated.mkdir(parents=True)
            (generated / 'acl-manifests.json').write_text('{}')
            dist.verify_capture(output, records)
            (output / 'crates/rsi/desktop/injected.rs').write_text('not a generated schema')
            with self.assertRaisesRegex(ValueError, 'uncaptured source added'):
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


if __name__ == '__main__': unittest.main()
