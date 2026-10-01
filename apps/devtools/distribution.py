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
import contextlib
import fcntl
import time
import sys


def digest(data):
    return hashlib.sha256(data).hexdigest()


def source_identity(info):
    # Reads may update atime; identity, content timestamps and permissions may not change.
    return tuple(getattr(info, field) for field in (
        'st_dev', 'st_ino', 'st_size', 'st_mtime_ns', 'st_ctime_ns', 'st_mode'))


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
            if target.is_symlink() and os.readlink(target) == link:
                pass
            else:
                if target.exists() or target.is_symlink(): target.unlink()
                target.symlink_to(link)
            records[name] = {'kind': 'symlink', 'target': link}
        elif stat.S_ISREG(info.st_mode):
            with path.open('rb') as source:
                if source_identity(os.fstat(source.fileno())) != source_identity(info):
                    raise ValueError(f"source changed while opening: {name}")
                data = source.read()
            executable = bool(info.st_mode & 0o111)
            mode = 0o555 if executable else 0o444
            if target.is_symlink(): target.unlink()
            if not target.exists() or target.read_bytes() != data or stat.S_IMODE(target.stat().st_mode) != mode:
                if target.exists(): target.chmod(0o600)
                target.write_bytes(data)
                target.chmod(mode)
            records[name] = {'kind': 'file', 'sha256': digest(data), 'bytes': len(data), 'executable': executable}
        else:
            raise ValueError(f"source is not a regular file or captured symlink: {name}")
    for name, info in before.items():
        after = (root / name).lstat()
        # Access times are allowed to change when a captured file is read.
        if source_identity(info) != source_identity(after):
            raise ValueError(f"source changed during capture: {name}")
    return records


def verify_capture(root, records):
    # These build outputs are not source inputs. Never follow captured symlinks.
    generated = {'node_modules', 'target', '__pycache__'}
    for directory, folders, files in os.walk(root, followlinks=False):
        base = Path(directory)
        links = [name for name in folders if (base / name).is_symlink()]
        folders[:] = [name for name in folders if name not in generated and name not in links
                      and (base / name).relative_to(root).as_posix() != 'apps/desktop/gen']
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
        elif stat.S_IMODE(path.stat().st_mode) != (0o555 if record['executable'] else 0o444):
            raise ValueError(f"captured source mode changed: {name}")


def put(path, data):
    if path.exists() and path.read_bytes() == data: return
    if path.exists(): path.chmod(0o600)
    path.write_bytes(data)


@contextlib.contextmanager
def build_lock(cache):
    """Source capture, compilation, publication and collection share one owner."""
    with (cache / 'build.lock').open('a+b') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        yield


def collect(cache):
    """Keep current, previous, active leases and the newest failed diagnostic."""
    generations = cache / 'generations'
    if not generations.exists(): return []
    current = (cache / 'current').resolve() if (cache / 'current').exists() else None
    completed = sorted((p for p in generations.iterdir() if (p / 'bundle/receipt.json').is_file()), key=lambda p: p.name, reverse=True)
    retained = {p for p in completed[:2]}
    if current: retained.add(current.parent)
    failed = sorted((p for p in generations.iterdir() if not (p / 'bundle/receipt.json').is_file()), key=lambda p: p.name, reverse=True)
    if failed: retained.add(failed[0])
    removed = []
    for generation in [*completed, *failed]:
        if generation in retained: continue
        lock = generation / 'bundle/.rsi-generation.lock'
        with contextlib.ExitStack() as stack:
            if lock.exists():
                handle = stack.enter_context(lock.open('rb'))
                try: fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BlockingIOError: continue
            shutil.rmtree(generation)
            removed.append(generation.name)
    return removed


def refresh(root, source, names, previous):
    source.mkdir(parents=True, exist_ok=True)
    for name in sorted(set(previous) - set(names), reverse=True):
        path = source / name
        if path.exists() or path.is_symlink(): path.unlink()
    return capture(root, source, names)


def main():
    parser = argparse.ArgumentParser(prog='rsi-app-tools dist', description=__doc__)
    parser.add_argument('kind', choices=['web', 'desktop', 'gc'])
    parser.add_argument('output', nargs='?', type=Path)
    parser.add_argument('--debug', action='store_true')
    args = parser.parse_args()
    if platform.system() != 'Linux': parser.error('paired publication currently requires Linux or WSL')
    root = Path.cwd().resolve()
    if not (root / 'apps/devtools/Cargo.toml').is_file(): parser.error('run from the repository root')
    cache = root / 'target/rsi-app'; cache.mkdir(parents=True, exist_ok=True)
    with build_lock(cache):
        if args.kind == 'gc':
            if args.output or args.debug: parser.error('gc takes no output or profile')
            print(json.dumps({'event': 'application-gc', 'removed': collect(cache)})); return
        build(root, cache, args, parser)


def build(root, cache, args, parser):
    managed = args.output is None
    if args.output and (not args.output.is_absolute() or args.output.exists()): parser.error('output must be a new absolute directory')
    output = args.output or cache / 'generations' / str(time.time_ns())
    output.mkdir(parents=True)
    try:
        return build_captured(root, cache, args, output, managed)
    except BaseException as failure:
        # Keep a bounded failure category even if capture or version probing failed
        # before a compiler log could be opened. Never serialize environment values.
        (output / 'failure.json').write_text(json.dumps({'stage': 'paired-build', 'error_type': type(failure).__name__}) + '\n')
        if managed:
            try:
                collect(cache)
            except OSError as cleanup:
                print(json.dumps({'event': 'failed-build-gc-failed', 'error_type': type(cleanup).__name__}), file=sys.stderr)
        raise


def helper_target(native):
    cpu = native.split('-')[0]
    if cpu not in {'x86_64', 'aarch64'} or '-linux-' not in native:
        raise ValueError('SSH helper publication requires a same-CPU Linux x86_64 or aarch64 host')
    return cpu + '-unknown-linux-musl'


def build_captured(root, cache, args, output, managed):
    work = cache / 'build'; work.mkdir(exist_ok=True)
    source = work / 'source'
    manifest_path = work / 'build-family.json'
    previous = json.loads(manifest_path.read_bytes()).get('files', {}) if manifest_path.exists() else {}
    names = sorted(set(os.fsdecode(item) for item in subprocess.check_output(
        ['git', 'ls-files', '--cached', '--others', '--exclude-standard', '-z'], cwd=root).split(b'\0') if item))
    names = [name for name in names if (root / name).exists() or (root / name).is_symlink()]
    records = refresh(root, source, names, previous)
    profile = 'debug' if args.debug else 'release'
    flags = [] if args.debug else ['--release']
    environment = {key: os.environ[key] for key in (
        'PATH', 'CARGO_HOME', 'RUSTUP_HOME', 'CARGO_BUILD_JOBS',
        'HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'NO_PROXY',
        'http_proxy', 'https_proxy', 'all_proxy', 'no_proxy',
        'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'RSI_WASM_BINDGEN',
    ) if key in os.environ}
    # Build caches are shared explicitly; private runtime HOME never owns downloads.
    for key, suffix in [('CARGO_HOME', '.cargo'), ('RUSTUP_HOME', '.rustup')]:
        if key not in environment and os.environ.get('HOME'): environment[key] = str(Path(os.environ['HOME']) / suffix)
    build_home = cache / 'build-home'; build_home.mkdir(exist_ok=True)
    environment.update(HOME=str(build_home), XDG_CACHE_HOME=str(cache / 'tool-cache'),
                       COREPACK_HOME=str(cache / 'corepack'), npm_config_store_dir=str(cache / 'pnpm-store'),
                       CI='true')
    def version(command): return subprocess.check_output(command, cwd=source, env=environment, text=True).strip()
    rustc = version(['rustc', '-vV']); cargo = version(['cargo', '-V'])
    target = next(line.removeprefix('host: ') for line in rustc.splitlines() if line.startswith('host: '))
    musl_target = helper_target(target)
    musl_cc = os.environ.get('RSI_MUSL_CC', 'musl-gcc')
    if 'RSI_MUSL_CC' in os.environ and not Path(musl_cc).is_absolute():
        raise ValueError('RSI_MUSL_CC must be an absolute compiler path')
    musl_cc = shutil.which(musl_cc, path=environment.get('PATH'))
    if not musl_cc:
        raise ValueError('missing musl-gcc or explicit RSI_MUSL_CC')
    environment['CARGO_TARGET_' + musl_target.upper().replace('-', '_') + '_LINKER'] = musl_cc
    environment['CC_' + musl_target.replace('-', '_')] = musl_cc
    helper = {'target': musl_target, 'profile': 'release', 'compiler': version([musl_cc, '--version']),
              'compiler_sha256': digest(Path(musl_cc).read_bytes())}
    toolchain = {'node': version(['node', '--version']), 'pnpm': version(['pnpm', '--version']),
                 'wasm_bindgen': version([environment.get('RSI_WASM_BINDGEN', 'wasm-bindgen'), '--version'])}
    if args.kind == 'desktop':
        toolchain.update(gtk3=version(['pkg-config', '--modversion', 'gtk+-3.0']), webkitgtk41=version(['pkg-config', '--modversion', 'webkit2gtk-4.1']))
    packages = ['rsi-cli', *(['rsi-desktop'] if args.kind == 'desktop' else []), 'rsi-web', 'rsi-ssh-helper-app']
    manifest = {'format': 1, 'files': records, 'rustc': rustc, 'cargo': cargo,
                'document_toolchain': toolchain, 'target': target, 'worker_target': 'wasm32-unknown-unknown',
                'profile': profile, 'helper': helper, 'packages': packages, 'default_features': True, 'features': [],
                'flags': {key: environment.get(key, '') for key in ('RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS')}}
    manifest_bytes = (json.dumps(manifest, sort_keys=True, separators=(',', ':')) + '\n').encode()
    if len(manifest_bytes) > 16 * 1024 * 1024: raise ValueError('build family manifest exceeds 16 MiB')
    put(manifest_path, manifest_bytes)
    family = digest(manifest_bytes)
    environment.update(RSI_BUILD_FAMILY_MANIFEST=str(manifest_path), RSI_BUILD_FAMILY_SHA256=family,
                       CARGO_TARGET_DIR=str(work / 'target'))
    bundle = output / 'bundle'; bundle.mkdir()
    if managed: (bundle / '.rsi-generation.lock').touch()
    verify_capture(source, records)
    with (output / 'build.log').open('w') as log:
        def run(command, cwd=source):
            print(json.dumps({'event': 'build-step', 'command': command[:5], 'log': str(output / 'build.log')}), flush=True)
            subprocess.run(command, cwd=cwd, env=environment, stdout=log, stderr=subprocess.STDOUT, check=True)
        native = ['rsi-cli', *(['rsi-desktop'] if args.kind == 'desktop' else [])]
        run(['cargo', 'build', '--locked', '--target', target, *[part for package in native for part in ['-p', package]], '--bins', *flags])
        run(['cargo', 'build', '--locked', '--target', musl_target, '-p', 'rsi-ssh-helper-app', '--release'])
        run(['pnpm', 'install', '--frozen-lockfile', '--ignore-scripts', '--store-dir', str(cache / 'pnpm-store')], source / 'apps/web')
        run(['node', 'apps/web/build.mjs', str(bundle / 'assets'), *(['--dev'] if args.debug else [])])
    verify_capture(source, records)
    artifacts = {}
    for name in ['rsi', *(['rsi-desktop'] if args.kind == 'desktop' else [])]:
        shutil.copy2(work / 'target' / target / profile / name, bundle / name)
        (bundle / name).chmod(0o755)
        artifacts[name] = digest((bundle / name).read_bytes())
    shutil.copy2(work / 'target' / musl_target / 'release/rsi-ssh-helper', bundle / 'rsi-ssh-helper')
    (bundle / 'rsi-ssh-helper').chmod(0o755)
    artifacts['rsi-ssh-helper'] = digest((bundle / 'rsi-ssh-helper').read_bytes())
    for path in sorted((bundle / 'assets').iterdir()):
        if path.is_file(): artifacts['assets/' + path.name] = digest(path.read_bytes())
    (bundle / 'build-family.json').write_bytes(manifest_bytes)
    (bundle / 'receipt.json').write_text(json.dumps({'format': 1, 'family_sha256': family, 'target': target, 'profile': profile,
        'kind': args.kind, 'mode': 'published', 'helper_target': musl_target, 'artifacts': artifacts}, indent=2) + '\n')
    if managed:
        temporary = cache / 'current.next'
        temporary.unlink(missing_ok=True)
        temporary.symlink_to(bundle.relative_to(cache))
        temporary.replace(cache / 'current')
        collect(cache)
    print(json.dumps({'event': 'application-distribution-built', 'bundle': str(bundle), 'family_sha256': family}), flush=True)


if __name__ == '__main__':
    main()
