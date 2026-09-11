"""Capture current source bytes once, then build a paired Linux distribution."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import stat
import subprocess


def digest(data):
    return hashlib.sha256(data).hexdigest()


def capture(root, destination, names):
    """Do not resolve symlinks, omit dirty files, or silently follow changing input."""
    before = {name: (root / name).lstat() for name in names}
    records = {}
    for name in names:
        path = root / name
        target = destination / name
        target.parent.mkdir(parents=True, exist_ok=True)
        info = before[name]
        if stat.S_ISLNK(info.st_mode):
            link = os.readlink(path)
            if Path(link).is_absolute() or not (path.parent / link).resolve().is_relative_to(root):
                raise ValueError(f"source symlink escapes capture: {name}")
            target.symlink_to(link)
            records[name] = {'kind': 'symlink', 'target': link}
        elif stat.S_ISREG(info.st_mode):
            with path.open('rb') as source:
                if os.fstat(source.fileno()) != info:
                    raise ValueError(f"source changed while opening: {name}")
                data = source.read()
            target.write_bytes(data)
            executable = bool(info.st_mode & 0o111)
            target.chmod(0o555 if executable else 0o444)
            records[name] = {'kind': 'file', 'sha256': digest(data), 'bytes': len(data), 'executable': executable}
        else:
            raise ValueError(f"source is not a regular file or captured symlink: {name}")
    for name, info in before.items():
        after = (root / name).lstat()
        # Access times are allowed to change when a captured file is read.
        if any(getattr(info, field) != getattr(after, field) for field in ('st_dev', 'st_ino', 'st_size', 'st_mtime_ns', 'st_ctime_ns', 'st_mode')):
            raise ValueError(f"source changed during capture: {name}")
    return records


def verify_capture(root, records):
    # These build outputs are not source inputs. Never follow captured symlinks.
    generated = {'node_modules', 'target', '__pycache__'}
    for directory, folders, files in os.walk(root, followlinks=False):
        base = Path(directory)
        links = [name for name in folders if (base / name).is_symlink()]
        folders[:] = [name for name in folders if name not in generated and name not in links
                      and (base / name).relative_to(root).as_posix() != 'crates/rsi/desktop/gen']
        for name in [*files, *links]:
            relative = (base / name).relative_to(root).as_posix()
            if relative not in records:
                raise ValueError(f"uncaptured source added: {relative}")
    for name, record in records.items():
        path = root / name
        if record['kind'] == 'symlink':
            if not path.is_symlink() or os.readlink(path) != record['target']:
                raise ValueError(f"captured symlink changed: {name}")
        elif path.is_symlink() or digest(path.read_bytes()) != record['sha256']:
            raise ValueError(f"captured source bytes changed: {name}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    parser.add_argument('--debug', action='store_true')
    args = parser.parse_args()
    if platform.system() != 'Linux':
        parser.error('this distribution target requires Linux')
    if not args.output.is_absolute() or args.output.exists():
        parser.error('output must be a new absolute directory')
    root = Path.cwd().resolve()
    names = sorted(set(os.fsdecode(item) for item in subprocess.check_output(
        ['git', 'ls-files', '--cached', '--others', '--exclude-standard', '-z'], cwd=root).split(b'\0') if item))
    # Deleted tracked files are intentionally absent from the current source tree.
    names = [name for name in names if (root / name).exists() or (root / name).is_symlink()]
    args.output.mkdir(parents=True)
    source = args.output / 'source'; source.mkdir()
    records = capture(root, source, names)
    profile = 'debug' if args.debug else 'release'
    flags = [] if args.debug else ['--release']
    build_environment = {key: os.environ[key] for key in (
        'PATH', 'HOME', 'CARGO_HOME', 'RUSTUP_HOME', 'CARGO_BUILD_JOBS',
        'HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'NO_PROXY',
        'http_proxy', 'https_proxy', 'all_proxy', 'no_proxy',
        'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'RSI_WASM_BINDGEN',
    ) if key in os.environ}
    def version(command):
        return subprocess.check_output(command, cwd=source, env=build_environment, text=True).strip()
    rustc = version(['rustc', '-vV'])
    cargo = version(['cargo', '-V'])
    target = next(line.removeprefix('host: ') for line in rustc.splitlines() if line.startswith('host: '))
    toolchain = {'node': version(['node', '--version']), 'npm': version(['npm', '--version']),
                 'gtk3': version(['pkg-config', '--modversion', 'gtk+-3.0']),
                 'webkitgtk41': version(['pkg-config', '--modversion', 'webkit2gtk-4.1'])}
    manifest = {'format': 1, 'files': records, 'rustc': rustc, 'cargo': cargo,
                'document_toolchain': toolchain,
                'target': target, 'profile': profile, 'packages': ['rsi', 'rsi-desktop'],
                'default_features': True, 'features': [],
                'flags': {key: build_environment.get(key, '') for key in ('RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS')}}
    manifest_bytes = (json.dumps(manifest, sort_keys=True, separators=(',', ':')) + '\n').encode()
    if len(manifest_bytes) > 16 * 1024 * 1024:
        raise ValueError('build family manifest exceeds 16 MiB')
    manifest_path = args.output / 'build-family.json'; manifest_path.write_bytes(manifest_bytes); manifest_path.chmod(0o444)
    build_environment.update(RSI_BUILD_FAMILY_MANIFEST=str(manifest_path), CARGO_TARGET_DIR=str(args.output / 'target'))
    bundle = args.output / 'bundle'; bundle.mkdir()
    with (args.output / 'build.log').open('w') as log:
        def run(command, cwd=source):
            print(json.dumps({'event': 'build-step', 'command': command[:4]}), flush=True)
            subprocess.run(command, cwd=cwd, env=build_environment, stdout=log, stderr=subprocess.STDOUT, check=True)
        run(['cargo', 'build', '--locked', '--target', target, '-p', 'rsi', '-p', 'rsi-desktop', '--bins', *flags])
        run(['npm', 'ci', '--ignore-scripts', '--no-audit', '--no-fund'], source / 'plugins/rsi/web')
        run(['node', 'plugins/rsi/web/build.mjs', str(bundle / 'assets'), *(['--dev'] if args.debug else [])])
    verify_capture(source, records)
    artifacts = {}
    for name in ('rsi', 'rsi-desktop'):
        shutil.copy2(args.output / 'target' / target / profile / name, bundle / name)
        artifacts[name] = digest((bundle / name).read_bytes())
    for path in sorted((bundle / 'assets').iterdir()):
        if path.is_file(): artifacts['assets/' + path.name] = digest(path.read_bytes())
    (bundle / 'receipt.json').write_text(json.dumps({'format': 1, 'family_sha256': digest(manifest_bytes), 'target': target, 'profile': profile, 'artifacts': artifacts}, indent=2) + '\n')
    shutil.copy2(manifest_path, bundle / 'build-family.json')
    print(json.dumps({'event': 'desktop-distribution-built', 'bundle': str(bundle), 'family_sha256': digest(manifest_bytes)}))


if __name__ == '__main__':
    main()
