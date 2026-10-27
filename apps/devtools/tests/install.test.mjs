import assert from 'node:assert/strict';
import { test } from 'node:test';
import { spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { copyFile, mkdtemp, mkdir, readFile, readdir, rm, stat, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
import { buildEnvironment, parseEnvironmentDeclaration, parse, publishPackage } from '../build.mjs';
import { assemble } from '../packages.mjs';

const root = fileURLToPath(new URL('../../../', import.meta.url));
const windows = process.platform === 'win32';
const uninstaller = process.env.RSI_TEST_UNINSTALLER ?? join(root, 'target/debug', windows ? 'rsi-uninstall.exe' : 'rsi-uninstall');
let gitBashProgram;
function gitBash() {
  if (gitBashProgram) return gitBashProgram;
  const gitPaths = spawnSync('where.exe', ['git.exe'], {encoding:'utf8', timeout:10_000});
  assert.equal(gitPaths.status, 0, 'Git for Windows must be installed for installer tests');
  for (const git of gitPaths.stdout.trim().split(/\r?\n/)) {
    for (const candidate of [resolve(dirname(git), '../bin/bash.exe'), resolve(dirname(git), '../../bin/bash.exe')]) {
      if (!existsSync(candidate)) continue;
      const probe = spawnSync(candidate, ['--noprofile', '--norc', '-c', 'uname -s'], {encoding:'utf8', timeout:10_000});
      if (probe.status === 0 && /^(MINGW|MSYS)/.test(probe.stdout)) return gitBashProgram = candidate;
    }
  }
  assert.fail('Could not locate Git for Windows Bash beside git.exe; do not use the WSL bash launcher');
}
function gitBashInstaller(args, cwd, env = process.env, script = join(root, 'scripts/install.sh')) {
  return spawnSync(gitBash(), ['--noprofile', '--norc', script.replaceAll('\\', '/'), ...args],
    {cwd, env, encoding:'utf8', timeout:60_000});
}
function gitBashCommand(command, args, cwd, env = process.env) {
  return spawnSync(gitBash(), ['--noprofile', '--norc', '-c', command, 'installer-test', ...args],
    {cwd, env, encoding:'utf8', timeout:60_000});
}
function gitPath(path) {
  const converted = gitBashCommand('cygpath -au -- "$1"', [path], root);
  assert.equal(converted.status, 0, converted.stderr);
  return converted.stdout.trim();
}
function installer(args, cwd, env = process.env) {
  if ((args.includes('-Binary') || args.includes('--binary')) && !args.includes('-Uninstaller') && !args.includes('--uninstaller')) args = [...args, windows ? '-Uninstaller' : '--uninstaller', uninstaller];
  const program = windows ? 'powershell.exe' : 'bash';
  const command = windows
    ? ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', join(root, 'scripts/install.ps1'), ...args]
    : [join(root, 'scripts/install.sh'), ...args];
  return spawnSync(program, command, {cwd, env, encoding:'utf8', timeout:60_000});
}

function assertConciseRefusal(result, path) {
  assert.equal(result.status, windows ? 1 : 2, result.stderr);
  assert(result.stderr.includes(path), result.stderr);
  assert.doesNotMatch(result.stderr, /Preparing release build|Building rsi|Using existing binary|Updating user PATH|CategoryInfo|FullyQualifiedErrorId|\+\s+throw/);
}

test('PowerShell launchers emit UTF-8 even with an inherited CP936 console', {skip:!windows}, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'rsi encoding '));
  try {
    const prefix = join(directory, "中文 ' prefix");
    await mkdir(join(prefix, 'bin'), {recursive:true});
    await writeFile(join(prefix, 'bin/rsi.exe'), 'existing');
    for (const name of ['install', 'uninstall']) {
      const target = name === 'install' ? prefix : join(directory, '空目录');
      const command = '[Console]::OutputEncoding=[Text.Encoding]::GetEncoding(936); & $env:RSI_TEST_SCRIPT -Prefix $env:RSI_TEST_PREFIX; exit $LASTEXITCODE';
      const result = spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-OutputFormat', 'Text', '-EncodedCommand', Buffer.from(command, 'utf16le').toString('base64')],
        {encoding:'utf8', env:{...process.env, RSI_TEST_SCRIPT:join(root, `scripts/${name}.ps1`), RSI_TEST_PREFIX:target}, timeout:30_000});
      assert.equal(result.status, name === 'install' ? 1 : 0, result.stderr);
      assert((result.stdout + result.stderr).includes(target), result.stdout + result.stderr);
    }
  } finally {await rm(directory, {recursive:true,force:true});}
});

test('occupied installation paths are rejected before Cargo, source inspection or filesystem effects', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'rsi preflight '));
  try {
    const checkout = join(directory, 'checkout'), scripts = join(checkout, 'scripts');
    const tools = join(directory, 'tools'), target = join(directory, 'target'), home = join(directory, 'home');
    await mkdir(scripts, {recursive:true}); await mkdir(tools); await mkdir(home);
    await mkdir(join(checkout, 'apps/devtools'), {recursive:true});
    for (const name of ['install.ps1', 'install.sh']) await copyFile(join(root, 'scripts', name), join(scripts, name));
    await copyFile(join(root, 'apps/devtools/build-environment.txt'), join(checkout, 'apps/devtools/build-environment.txt'));
    const marker = join(target, 'cargo-called');
    if (windows) {
      await copyFile(process.execPath, join(tools, 'cargo.exe'));
      await writeFile(join(checkout, 'build'), 'const fs=require("node:fs"),p=require("node:path"),t=process.env.CARGO_TARGET_DIR;fs.mkdirSync(t,{recursive:true});fs.writeFileSync(p.join(t,"cargo-called"),"called");process.exitCode=23;');
    } else {
      await writeFile(join(tools, 'cargo'), '#!/bin/sh\nmkdir -p -- "$CARGO_TARGET_DIR"\nprintf called > "$CARGO_TARGET_DIR/cargo-called"\nexit 23\n', {mode:0o700});
    }
    await writeFile(join(home, '.profile'), '# preserve profile\n');
    await writeFile(join(home, '.bashrc'), '# preserve bashrc\n');
    const userPath = () => spawnSync('powershell.exe', ['-NoProfile', '-Command', '[Environment]::GetEnvironmentVariable("Path", "User")'], {encoding:'utf8'}).stdout;
    const beforePath = windows ? userPath() : null;
    const environment = {...process.env, PATH:tools+(windows ? ';' : ':')+process.env.PATH,
      HOME:home, LOCALAPPDATA:join(directory, 'local-app-data'), CARGO_TARGET_DIR:target};
    const cases = ['file', 'directory', 'link', 'dangling-link', 'parent-file', 'bin-file', 'parent-link'];
    for (const kind of cases) {
      const prefix = join(directory, kind+" 中文 ' $value & ; `"), bin = join(prefix, 'bin');
      const destination = join(bin, windows ? 'rsi.exe' : 'rsi');
      let conflict = destination;
      if (kind === 'parent-link') {
        const external = join(directory, 'parent-link-target');
        await mkdir(external); await symlink(external,prefix,windows?'junction':'dir'); conflict=prefix;
      } else if (kind === 'parent-file') {
        await writeFile(prefix, 'parent sentinel'); conflict = prefix;
      } else if (kind === 'bin-file') {
        await mkdir(prefix); await writeFile(bin, 'bin sentinel'); conflict = bin;
      } else {
        await mkdir(bin, {recursive:true});
        if (kind === 'file') await writeFile(destination, 'installed sentinel');
        else if (kind === 'directory') await mkdir(destination);
        else {
          const linked = join(directory, kind+'-target');
          if (kind === 'link') await mkdir(linked);
          await symlink(linked, destination, windows ? 'junction' : 'dir');
        }
      }
      for (const explicitBinary of [false, true]) {
        const args = windows ? ['-Prefix', prefix] : ['--prefix', prefix];
        if (explicitBinary) args.push(windows ? '-Binary' : '--binary', join(directory, 'missing-source'), windows ? '-Uninstaller' : '--uninstaller', uninstaller);
        const result = spawnSync(windows ? 'powershell.exe' : 'bash', windows
          ? ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', join(scripts, 'install.ps1'), ...args]
          : [join(scripts, 'install.sh'), ...args], {cwd:directory, env:environment, encoding:'utf8', timeout:20_000});
        assertConciseRefusal(result, conflict);
        if(kind==='parent-link')assert.match(result.stderr,/link[\s\S]*resolved directory/i);
        else assert.match(result.stderr, /another.*prefix/i);
        if (kind === 'file') {
          assert.match(result.stderr, /already installed[\s\S]*core upgrade --local[\s\S]*package\.json/);
          assert.equal(await readFile(destination, 'utf8'), 'installed sentinel');
        } else assert.doesNotMatch(result.stderr, /core upgrade --local/);
        await assert.rejects(stat(marker), {code:'ENOENT'});
        await assert.rejects(stat(join(checkout, 'target')), {code:'ENOENT'});
      }
      if (kind === 'parent-file' || kind === 'bin-file') assert.equal(await readFile(conflict, 'utf8'), kind === 'parent-file' ? 'parent sentinel' : 'bin sentinel');
      else if(kind==='parent-link') assert.deepEqual(await readdir(join(directory,'parent-link-target')),[]);
      else assert.deepEqual(await readdir(bin), [windows ? 'rsi.exe' : 'rsi']);
    }
    assert.equal(await readFile(join(home, '.profile'), 'utf8'), '# preserve profile\n');
    assert.equal(await readFile(join(home, '.bashrc'), 'utf8'), '# preserve bashrc\n');
    assert.deepEqual((await readdir(home)).sort(), ['.bashrc', '.profile']);
    await assert.rejects(stat(join(environment.LOCALAPPDATA, 'RSIversi')), {code:'ENOENT'});
    if (windows) assert.equal(userPath(), beforePath);
  } finally { await rm(directory, {recursive:true, force:true}); }
});
test('minimal installation preserves the executable, refuses replacement and does not open product homes', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'rsi install '));
  try {
    const prefix = join(directory, "prefix 中文 ' $value & ; `");
    const args = windows ? ['-Prefix', prefix, '-Binary', uninstaller, '-NoPath']
      : ['--prefix', prefix, '--binary', uninstaller, '--no-path'];
    const first = installer(args, directory);
    assert.equal(first.status, 0, first.stderr);
    assert.match(first.stderr, /\[rsi\] Preparing installation[\s\S]*Using existing binary[\s\S]*Skipping PATH update/);
    assert.doesNotMatch(first.stderr, /Building rsi|Updating user PATH/);
    const installed = join(prefix, 'bin', windows ? 'rsi.exe' : 'rsi');
    const digest = bytes => createHash('sha256').update(bytes).digest('hex');
    assert.equal(digest(await readFile(installed)), digest(await readFile(uninstaller)));
    const again = installer(args, directory);
    assert.notEqual(again.status, 0);
    assert.match(again.stderr, /already installed/);
    assertConciseRefusal(again, installed);
    const upgrade = again.stderr.split(/\r?\n/).find(line => line.trimStart().startsWith(windows ? '& ' : '') && line.includes(' core upgrade --local '));
    assert(upgrade, again.stderr);
    await copyFile(process.execPath, installed);
    // Execute the displayed command against the installed Node fixture to prove
    // its quoting reaches this installation with literal arguments.
    await writeFile(join(directory, 'core'), 'console.log(JSON.stringify({executable:process.execPath,args:process.argv.slice(2)}));');
    const guidance = spawnSync(windows ? 'powershell.exe' : 'bash', windows
      ? ['-NoProfile', '-Command', upgrade] : ['--noprofile', '--norc', '-c', upgrade],
      {cwd:directory, env:process.env, encoding:'utf8', timeout:10_000});
    assert.equal(guidance.status, 0, guidance.stderr);
    assert.deepEqual(JSON.parse(guidance.stdout), {executable:installed, args:['upgrade', '--local', 'ABSOLUTE_CORE_PACKAGE']});
    assert.equal(digest(await readFile(installed)), digest(await readFile(process.execPath)));
    await assert.rejects(stat(join(prefix, 'config')), {code:'ENOENT'});
    await assert.rejects(stat(join(prefix, 'state')), {code:'ENOENT'});
  } finally { await rm(directory, {recursive:true, force:true}); }
});
test('installer help and retired UI grammar never start a build', () => {
  const help = installer([windows ? '-Help' : '--help'], root);
  assert.equal(help.status, 0, help.stderr);
  assert.match(help.stdout, /Install rsi/);
  assert.match(help.stdout, /first installation[\s\S]*before building/);
  assert.doesNotMatch(help.stderr, /\[rsi\]/);
  const old = installer(['web'], root);
  assert.notEqual(old.status, 0);
  assert.doesNotMatch(old.stderr, /Compiling|Core build failed/);
  assert.match(old.stderr, windows ? /PositionalParameterNotFound/ : /Usage:/);
});

test('Git Bash delegates defaults, paths and exit status to the adjacent Windows installer', {skip:!windows}, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'rsi bash dispatch '));
  try {
    const scripts = join(directory, "checkout 中文 ' $value", 'scripts'), tools = join(directory, 'tools');
    await mkdir(scripts, {recursive:true}); await mkdir(tools);
    const script = join(scripts, 'install.sh');
    await copyFile(join(root, 'scripts/install.sh'), script);
    await writeFile(join(tools, 'powershell.exe'), '#!/usr/bin/env bash\nprintf "%s\\0" "$MSYS2_ARG_CONV_EXCL" "$RUSTUP_TOOLCHAIN" "$@"\nprintf "fixture stderr\\n" >&2\nexit 37\n');
    const command = 'tools=$(cygpath -au -- "$1"); shift; PATH="$tools:/usr/bin" exec /usr/bin/bash "$@"';
    const environment = {...process.env, MSYS2_ARG_CONV_EXCL:'caller-setting', RUSTUP_TOOLCHAIN:'fixture-toolchain'};
    const run = args => gitBashCommand(command, [tools, script.replaceAll('\\', '/'), ...args], directory, environment);
    const defaults = run([]);
    assert.equal(defaults.status, 37, defaults.stderr);
    assert.equal(defaults.stderr, '[rsi] Starting Windows installer...\nfixture stderr\n');
    const invocation = ['*', 'fixture-toolchain', '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass',
      '-File', join(scripts, 'install.ps1')];
    assert.deepEqual(defaults.stdout.split('\0').slice(0, -1), invocation);
    const explicit = run(['--prefix', "relative 中文 ' $value", '--binary', './input.exe', '--no-path']);
    assert.equal(explicit.status, 37, explicit.stderr);
    assert.deepEqual(explicit.stdout.split('\0').slice(0, -1), [...invocation,
      '-Prefix', join(directory, "relative 中文 ' $value"), '-Binary', join(directory, 'input.exe'), '-NoPath']);
  } finally { await rm(directory, {recursive:true, force:true}); }
});

test('Git Bash installs native executables with relative, MSYS and Windows paths and Windows defaults', {skip:!windows}, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'rsi bash install '));
  try {
    const binary = join(directory, "binary 中文 ' $name (x) &.exe");
    await copyFile(uninstaller, binary);
    const digest = bytes => createHash('sha256').update(bytes).digest('hex');
    const expected = digest(await readFile(binary));
    const cases = [
      {prefix:"relative 中文 ' $() & ; `", binary:binary},
      {prefix:gitPath(join(directory, 'MSYS prefix')), binary:binary.replaceAll('\\', '/')},
      {prefix:join(directory, 'Windows prefix'), binary:gitPath(binary)},
      {prefix:null, binary:binary},
    ];
    for (const item of cases) {
      const environment = {...process.env, LOCALAPPDATA:join(directory, 'local-app-data')};
      const args = ['--binary', item.binary, '--uninstaller', uninstaller, '--no-path'];
      if (item.prefix !== null) args.push('--prefix', item.prefix);
      const first = gitBashInstaller(args, directory, environment);
      assert.equal(first.status, 0, first.stderr);
      const prefix = item.prefix === null ? join(environment.LOCALAPPDATA, 'RSIversi')
        : item.prefix.startsWith('/') ? join(directory, 'MSYS prefix') : resolve(directory, item.prefix);
      const installed = join(prefix, 'bin/rsi.exe');
      assert.equal(digest(await readFile(installed)), expected);
      const again = gitBashInstaller(args, directory, environment);
      assert.equal(again.status, 1, again.stderr);
      assert.match(again.stderr, /already installed/);
      assert.equal(digest(await readFile(installed)), expected);
      await assert.rejects(stat(join(prefix, 'config')), {code:'ENOENT'});
      await assert.rejects(stat(join(prefix, 'state')), {code:'ENOENT'});
    }
  } finally { await rm(directory, {recursive:true, force:true}); }
});

for (const caseAliases of [false, true]) test(`Git Bash source builds preserve Cargo inputs${caseAliases ? ' with inherited case aliases' : ''}`, {skip:!windows}, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'rsi bash source '));
  try {
    const checkout = join(directory, "checkout 中文 ' quote"), scripts = join(checkout, 'scripts');
    const tools = join(directory, 'tools'), fallback = join(directory, 'unused-tools'), target = caseAliases ? join(checkout, 'relative target') : join(directory, 'custom target');
    await mkdir(scripts, {recursive:true}); await mkdir(tools); await mkdir(fallback);
    await mkdir(join(checkout, 'apps/devtools'), {recursive:true});
    for (const name of ['install.sh', 'install.ps1', 'uninstall.ps1']) await copyFile(join(root, 'scripts', name), join(scripts, name));
    await copyFile(join(root, 'apps/devtools/build-environment.txt'), join(checkout, 'apps/devtools/build-environment.txt'));
    // Node stands in for cargo.exe and executes this fixture's `build` file.
    await copyFile(process.execPath, join(tools, 'cargo.exe'));
    await writeFile(join(fallback, 'cargo.exe'), 'This later PATH entry must never run.');
    await writeFile(join(checkout, 'build'), `
const fs = require('node:fs'), path = require('node:path');
const target = process.env.CARGO_TARGET_DIR;
process.stdout.write('fixture build stdout\\n');
process.stderr.write('fixture build stderr\\n');
${caseAliases ? '' : 'Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 16000);'}
fs.mkdirSync(path.join(target, 'release'), {recursive:true});
fs.copyFileSync(${JSON.stringify(uninstaller)}, path.join(target, 'release/rsi.exe'));
fs.copyFileSync(${JSON.stringify(uninstaller)}, path.join(target, 'release/rsi-uninstall.exe'));
fs.writeFileSync(path.join(target, 'build-inputs.json'), JSON.stringify({
  args:process.argv.slice(2), cwd:process.cwd(), target,
  cargo:process.env.CARGO_HOME, rustup:process.env.RUSTUP_HOME,
  toolchain:process.env.RUSTUP_TOOLCHAIN, home:process.env.HOME, ci:process.env.CI,
  credential:process.env.DEEPSEEK_API_KEY ?? null,
  proxy:process.env.HTTP_PROXY, noProxy:process.env.NO_PROXY,
  undeclared:process.env.RSI_INSTALL_CASE ?? null,
}));
`);
    const environment = {...process.env, LOCALAPPDATA:join(directory, 'local-app-data'),
      CARGO_TARGET_DIR:caseAliases ? 'relative target' : gitPath(target), CARGO_HOME:gitPath(join(directory, 'cargo home')),
      RUSTUP_HOME:gitPath(join(directory, 'rustup home')), RUSTUP_TOOLCHAIN:'fixture-toolchain',
      DEEPSEEK_API_KEY:'fixture-secret'};
    const command = 'tools=$(cygpath -au -- "$1"); fallback=$(cygpath -au -- "$2"); shift 2; export PATH="$tools:$fallback:$PATH"; '
      + 'unset http_proxy no_proxy rsi_install_case; export HTTP_PROXY=http://upper.invalid NO_PROXY=upper.invalid RSI_INSTALL_CASE=upper; '
      + (caseAliases ? 'export http_proxy=http://lower.invalid no_proxy=lower.invalid rsi_install_case=lower; ' : '')
      + 'exec "$@"';
    const selected = gitBashCommand(command, [tools, fallback, 'powershell.exe', '-NoProfile', '-NonInteractive', '-Command',
      'ConvertTo-Json -Compress -InputObject @([Environment]::GetEnvironmentVariable("HTTP_PROXY"), [Environment]::GetEnvironmentVariable("NO_PROXY"))'], directory, environment);
    assert.equal(selected.status, 0, selected.stderr);
    const result = gitBashCommand(command,
      [tools, fallback, '/usr/bin/bash', join(scripts, 'install.sh').replaceAll('\\', '/'), '--no-path'], directory, environment);
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /fixture build stdout/);
    assert.match(result.stderr, /fixture build stderr/);
    assert.match(result.stderr, /\[rsi\] Preparing release build[\s\S]*Building rsi[\s\S]*Build completed in \d+s[\s\S]*Skipping PATH update/);
    if (!caseAliases) assert.match(result.stderr, /Still building rsi \(\d+s elapsed\)/);
    const inputs = JSON.parse(await readFile(join(target, 'build-inputs.json'), 'utf8'));
    assert.deepEqual(inputs.args, ['--locked', '--release', '-p', 'rsi-cli', '-p', 'rsi-uninstall']);
    assert.equal(inputs.cwd, checkout);
    assert.equal(resolve(checkout, inputs.target), target);
    assert.equal(resolve(inputs.cargo), join(directory, 'cargo home'));
    assert.equal(resolve(inputs.rustup), join(directory, 'rustup home'));
    assert.equal(inputs.toolchain, 'fixture-toolchain');
    assert.equal(inputs.home, join(checkout, 'target/rsi-build/build-home'));
    assert.equal(inputs.ci, 'true');
    assert.equal(inputs.credential, null);
    assert.deepEqual([inputs.proxy, inputs.noProxy], JSON.parse(selected.stdout));
    assert.equal(inputs.undeclared, null);
    const installed = join(environment.LOCALAPPDATA, 'RSIversi/bin/rsi.exe');
    assert.deepEqual(await readFile(installed), await readFile(uninstaller));
    if (caseAliases) {
      await writeFile(join(checkout, 'build'), 'process.stdout.write("fixture stdout\\n".repeat(8192), () => process.stderr.write("fixture stderr\\n".repeat(8192), () => { process.exitCode = 23; }));');
      const failedPrefix = join(directory, 'failed install');
      const failed = gitBashCommand(command,
        [tools, fallback, '/usr/bin/bash', join(scripts, 'install.sh').replaceAll('\\', '/'), '--prefix', failedPrefix, '--no-path'], directory, environment);
      assert.equal(failed.status, 1, failed.error?.message);
      assert.equal(failed.stdout, 'fixture stdout\n'.repeat(8192));
      assert(failed.stderr.includes('fixture stderr\n'.repeat(8192)));
      assert.match(failed.stderr, /Core build failed; nothing was installed/);
      assert.doesNotMatch(failed.stderr, /Build completed|Installing rsi|Verifying executable/);
      await assert.rejects(stat(failedPrefix), {code:'ENOENT'});
    }
  } finally { await rm(directory, {recursive:true, force:true}); }
});

test('Git Bash diagnoses missing tools and rejects invalid arguments without building', {skip:!windows}, async () => {
  const script = join(root, 'scripts/install.sh').replaceAll('\\', '/');
  const command = 'uname() { printf "MSYS_NT-fixture\\n"; }; export -f uname; PATH="$1" exec /usr/bin/bash "$2" "${@:3}"';
  for (const flag of ['--help', '-h']) {
    const help = gitBashCommand(command, ['', script, flag], root);
    assert.equal(help.status, 0, help.stderr);
    assert.match(help.stdout, /Install rsi/);
    assert.doesNotMatch(help.stderr, /\[rsi\]/);
  }
  for (const args of [['web'], ['--prefix'], ['--binary'], ['--unknown']]) {
    const invalid = gitBashCommand(command, ['', script, ...args], root);
    assert.equal(invalid.status, 2, invalid.stderr);
    assert.match(invalid.stderr, /Usage:/);
  }
  for (const [path, tool] of [['', 'cygpath'], ['/usr/bin', 'powershell.exe']]) {
    const missing = gitBashCommand(command, [path, script], root);
    assert.equal(missing.status, 2, missing.stderr);
    assert.match(missing.stderr, new RegExp(`requires ${tool.replace('.', '\\.')}`));
  }
});

test('publication refuses a directory created during the version probe and cleans its stage', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'rsi install race '));
  try {
    const binary = join(directory, windows ? 'probe.exe' : 'probe'), prefix = join(directory, 'prefix');
    const destination = join(prefix, 'bin', windows ? 'rsi.exe' : 'rsi');
    if (windows) {
      const source = 'public class Probe { public static void Main() { if (System.Environment.GetEnvironmentVariable("RSI_PROBE_SECRET") != null) System.Environment.Exit(29); System.IO.Directory.CreateDirectory('+JSON.stringify(destination)+'); System.Console.WriteLine("{\\\"interface\\\":1,\\\"record\\\":1,\\\"transaction\\\":2}"); } }';
      const build = spawnSync('powershell.exe', ['-NoProfile', '-Command',
        'Add-Type -TypeDefinition $env:RSI_INSTALL_PROBE_SOURCE -OutputType ConsoleApplication -OutputAssembly $env:RSI_INSTALL_PROBE'],
        {env:{...process.env, RSI_INSTALL_PROBE:binary, RSI_INSTALL_PROBE_SOURCE:source}, encoding:'utf8', timeout:30_000});
      assert.equal(build.status, 0, build.stderr);
    } else await writeFile(binary, `#!/bin/sh
[ -z "\${RSI_PROBE_SECRET+x}" ] || exit 29
mkdir -p -- '${destination.replaceAll("'", "'\\''")}'
echo '{"interface":1,"record":1,"transaction":2}'
`, {mode:0o700});
    const result = installer(windows ? ['-Prefix', prefix, '-Binary', binary, '-NoPath']
      : ['--prefix', prefix, '--binary', binary, '--no-path'], directory,
      {...process.env, RSI_PROBE_SECRET:'must not reach a candidate probe'});
    assert.equal(result.status, 1, result.stderr);
    assert(result.stderr.includes(destination), result.stderr);
    assert.match(result.stderr, /another.*prefix/i);
    assert.doesNotMatch(result.stderr, /CategoryInfo|FullyQualifiedErrorId|core upgrade --local|Updating user PATH/);
    assert((await stat(destination)).isDirectory());
    assert.deepEqual(await readdir(destination), []);
    assert.deepEqual((await readdir(join(prefix, 'bin'))).filter(name => !name.startsWith('.rsi-')), [windows ? 'rsi.exe' : 'rsi']);
  } finally {await rm(directory, {recursive:true, force:true});}
});
test('POSIX PATH setup quotes the chosen prefix without evaluating its contents', {skip:windows}, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'rsi path '));
  try {
    const home = join(directory, 'home');
    await mkdir(home);
    await writeFile(join(home, '.profile'), '# existing\n');
    const prefix = join(directory, "quote ' $(touch UNEXPECTED)");
    const result = installer(['--binary', uninstaller, '--prefix', prefix], directory,
      {...process.env, HOME:home});
    assert.equal(result.status, 0, result.stderr);
    const shell = spawnSync('sh', ['-c', '. "$HOME/.profile"; command -v rsi'],
      {cwd:directory, env:{...process.env, HOME:home}, encoding:'utf8'});
    assert.equal(shell.status, 0, shell.stderr);
    assert.equal(shell.stdout.trim(), join(prefix, 'bin/rsi'));
    await assert.rejects(stat(join(directory, 'UNEXPECTED')), {code:'ENOENT'});
    assert.match(await readFile(join(home, '.profile'), 'utf8'), /^# existing/);
  } finally { await rm(directory, {recursive:true, force:true}); }
});
test('independent producer rejects UI aliases and invalid dependency selections before effects', () => {
  assert.throws(() => parse(['tui', root]));
  assert.throws(() => parse(['core', 'relative']));
  assert.equal(parse(['desktop', root]).kind, 'desktop');
  assert.throws(() => parse(['web', root, '--web-package', root]));
  assert.throws(() => parse(['web', root, '--debug', '--debug']));
  assert.deepEqual(parse(['desktop', root, '--debug']),
    {kind:'desktop', output:root, debug:true, replace:false});
});
test('source publication replaces only its own package and preserves unrelated output files', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'rsi producer '));
  try {
    const input = join(directory, 'input'), output = join(directory, 'output');
    await writeFile(input, 'first');
    const first = join(directory, 'first');
    const one = await assemble('core', first, input, input);
    // A crash immediately after mkdir must not wedge the next --replace.
    await mkdir(output);
    await writeFile(join(output, 'unknown'), 'preserve');
    await assert.rejects(publishPackage(first, output, one, true), {code:'ENOENT'});
    assert.equal(await readFile(join(output, 'unknown'), 'utf8'), 'preserve');
    await rm(join(output, 'unknown'));
    await publishPackage(first, output, one, true);
    await writeFile(join(output, 'keep'), 'user data');
    await writeFile(input, 'second');
    const second = join(directory, 'second');
    const two = await assemble('core', second, input, input);
    await assert.rejects(publishPackage(second, output, two), {code:'EEXIST'});
    await assert.rejects(publishPackage(second, output, {...two, name:'foreign'}, true), /different package/);
    assert.equal(await readFile(join(output, one.files[0].path), 'utf8'), 'first');
    await publishPackage(second, output, two, true);
    assert.equal(await readFile(join(output, two.files[0].path), 'utf8'), 'second');
    assert.equal(await readFile(join(output, 'keep'), 'utf8'), 'user data');
    assert.deepEqual(JSON.parse(await readFile(join(output, 'package.json'), 'utf8')), two);
    // Simulate interruption before the first manifest reached the output.
    await rm(join(output, 'package.json'));
    const third = join(directory, 'third');
    const three = await assemble('core', third, input, input);
    await publishPackage(third, output, three, true);
    assert.deepEqual(JSON.parse(await readFile(join(output, 'package.json'), 'utf8')), three);
  } finally { await rm(directory, {recursive:true, force:true}); }
});
test('build subprocesses receive declared tool inputs and a private home, without runtime credentials', async () => {
  const environment = await buildEnvironment({...process.env,
    DEEPSEEK_API_KEY:'fixture-secret', RSI_REPOSITORY_SIGNING_KEY:'fixture-secret',
    RSI_PACKAGE_REPOSITORY_NAME:'release', RSI_PACKAGE_REPOSITORY_URL:'https://packages.example/',
    RSI_PACKAGE_REPOSITORY_KEY:'00'.repeat(32)});
  assert.equal(environment.DEEPSEEK_API_KEY, undefined);
  assert.equal(environment.RSI_REPOSITORY_SIGNING_KEY, undefined);
  assert.equal(environment.RSI_PACKAGE_REPOSITORY_NAME, 'release');
  assert.equal(environment.RSI_PACKAGE_REPOSITORY_URL, 'https://packages.example/');
  assert.equal(environment.RSI_PACKAGE_REPOSITORY_KEY, '00'.repeat(32));
  assert.notEqual(environment.HOME, process.env.HOME);
  assert.match(environment.HOME, /rsi-build[/\\]build-home$/);
  assert.equal(environment.PATH, process.env.PATH);
});

test('POSIX source installer admits SDK environment names and rejects malformed declarations', {skip:windows}, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'rsi source env '));
  try {
    const scripts = join(directory, 'scripts'), tools = join(directory, 'tools');
    const devtools = join(directory, 'apps/devtools'), target = join(directory, 'target');
    await mkdir(scripts); await mkdir(tools); await mkdir(devtools, {recursive:true});
    const script = join(scripts, 'install.sh');
    await writeFile(script, await readFile(join(root, 'scripts/install.sh')));
    await writeFile(join(scripts,'uninstall.sh'),await readFile(join(root,'scripts/uninstall.sh')));
    const declarations = join(devtools, 'build-environment.txt');
    await writeFile(declarations, 'PATH\nCARGO_TARGET_DIR\nProgramFiles(x86)\nHOME=build-home\n');
    await writeFile(join(tools, 'cargo'), '#!/usr/bin/python3\nimport os,pathlib,shutil\np=pathlib.Path(os.environ["CARGO_TARGET_DIR"])\n(p/"release").mkdir(parents=True)\n(p/"sdk").write_text(os.environ["ProgramFiles(x86)"])\nshutil.copy2('+JSON.stringify(uninstaller)+',p/"release/rsi")\nshutil.copy2('+JSON.stringify(uninstaller)+',p/"release/rsi-uninstall")\n', {mode:0o700});
    const environment = {...process.env, PATH:tools+':/usr/bin:/bin', HOME:directory, CARGO_TARGET_DIR:target, 'ProgramFiles(x86)':'fixture-sdk'};
    const first = spawnSync('bash', [script, '--prefix', join(directory,'installed'), '--no-path'], {env:environment,encoding:'utf8'});
    assert.equal(first.status, 0, first.stderr);
    assert.equal((await readFile(join(target,'sdk'),'utf8')).trim(), 'fixture-sdk');
    await writeFile(declarations, 'HOME=../escape\n');
    const invalid = spawnSync('bash', [script, '--prefix', join(directory,'invalid'), '--no-path'], {env:environment,encoding:'utf8'});
    assert.notEqual(invalid.status, 0); assert.match(invalid.stderr, /Invalid build environment declaration/);
    await assert.rejects(stat(join(directory,'invalid')), {code:'ENOENT'});
  } finally { await rm(directory, {recursive:true,force:true}); }
});

test('producer and native installer declaration grammars match the shared corpus', async () => {
  const cases=JSON.parse(await readFile(new URL('../build-environment-conformance.json',import.meta.url),'utf8'));
  for (const item of cases) {
    if(item.valid) assert.doesNotThrow(()=>parseEnvironmentDeclaration(item.declaration),item.declaration);
    else assert.throws(()=>parseEnvironmentDeclaration(item.declaration),undefined,item.declaration);
  }
  if(windows) {
    const source=await readFile(join(root,'scripts/install.ps1'),'utf8');
    const line=source.split('\n').find(line=>line.includes('if ($declaration '));
    const expression=line.split('if (')[1].split(") { throw")[0];
    const script='$cases=ConvertFrom-Json $env:RSI_GRAMMAR_CASES; foreach($item in $cases){$declaration=$item.declaration; $rejected='+expression+'; if($rejected -eq $item.valid){throw "Declaration grammar diverged"}}';
    const result=spawnSync('powershell.exe',['-NoProfile','-Command',script],{encoding:'utf8',env:{...process.env,RSI_GRAMMAR_CASES:JSON.stringify(cases)}});
    assert.equal(result.status,0,result.stderr);
  } else {
    const source=await readFile(join(root,'scripts/install.sh'),'utf8');
    const condition=source.split('\n').find(line=>line.includes('[[ "$declaration" =~'));
    for(const item of cases) {
      const result=spawnSync('bash',['-c','declaration=$RSI_GRAMMAR_CASE; '+condition],{encoding:'utf8',env:{...process.env,RSI_GRAMMAR_CASE:item.declaration}});
      assert.equal(result.status===0,item.valid,item.declaration);
    }
  }
});

test('explicit empty paths are rejected before installing or building',async()=>{
  const directory=await mkdtemp(join(tmpdir(),'rsi empty path '));
  try {
    for(const option of ['Prefix','Binary']) {
      const result=installer(windows?['-'+option,'','-NoPath']:['--'+option.toLowerCase(),'','--no-path'],directory);
      assert.equal(result.status,2,result.stderr);assert.doesNotMatch(result.stderr,/Building rsi/);
      if(windows){const bash=gitBashInstaller(['--'+option.toLowerCase(),'','--no-path'],directory);assert.equal(bash.status,2,bash.stderr);assert.doesNotMatch(bash.stderr,/Starting Windows installer/)}
    }
  }finally{await rm(directory,{recursive:true,force:true})}
});
