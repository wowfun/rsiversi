import assert from 'node:assert/strict';
import { test } from 'node:test';
import { spawnSync } from 'node:child_process';
import { mkdtemp, mkdir, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
import { buildEnvironment, parseEnvironmentDeclaration, parse, publishPackage } from '../build.mjs';
import { assemble } from '../packages.mjs';

const root = fileURLToPath(new URL('../../../', import.meta.url));
const windows = process.platform === 'win32';
function installer(args, cwd, env = process.env) {
  const program = windows ? 'powershell.exe' : 'bash';
  const command = windows
    ? ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', join(root, 'scripts/rsiversi.ps1'), ...args]
    : [join(root, 'scripts/rsiversi.sh'), ...args];
  return spawnSync(program, command, {cwd, env, encoding:'utf8', timeout:60_000});
}
test('minimal installation preserves the executable, refuses replacement and does not open product homes', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'rsi install '));
  try {
    const prefix = join(directory, "prefix with ' quote");
    const args = windows ? ['-Prefix', prefix, '-Binary', process.execPath, '-NoPath']
      : ['--prefix', prefix, '--binary', process.execPath, '--no-path'];
    const first = installer(args, directory);
    assert.equal(first.status, 0, first.stderr);
    const installed = join(prefix, 'bin', windows ? 'rsi.exe' : 'rsi');
    const digest = bytes => createHash('sha256').update(bytes).digest('hex');
    assert.equal(digest(await readFile(installed)), digest(await readFile(process.execPath)));
    const again = installer(args, directory);
    assert.notEqual(again.status, 0);
    assert.match(again.stderr, /already installed/);
    assert.equal(digest(await readFile(installed)), digest(await readFile(process.execPath)));
    await assert.rejects(stat(join(prefix, 'config')), {code:'ENOENT'});
    await assert.rejects(stat(join(prefix, 'state')), {code:'ENOENT'});
  } finally { await rm(directory, {recursive:true, force:true}); }
});
test('installer help and retired UI grammar never start a build', () => {
  const help = installer([windows ? '-Help' : '--help'], root);
  assert.equal(help.status, 0, help.stderr);
  assert.match(help.stdout, /Install only rsi/);
  const old = installer(['web'], root);
  assert.notEqual(old.status, 0);
  assert.doesNotMatch(old.stderr, /Compiling|Core build failed/);
  assert.match(old.stderr, windows ? /PositionalParameterNotFound/ : /Usage:/);
});

test('Windows publication refuses a directory created during the version probe', {skip:!windows}, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'rsi install race '));
  try {
    const binary = join(directory, 'probe.exe'), prefix = join(directory, 'prefix');
    const destination = join(prefix, 'bin', 'rsi.exe');
    const build = spawnSync('powershell.exe', ['-NoProfile', '-Command',
      'Add-Type -TypeDefinition \'public class Probe { public static void Main() { System.IO.Directory.CreateDirectory(System.Environment.GetEnvironmentVariable("RSI_INSTALL_COLLISION")); } }\' -OutputType ConsoleApplication -OutputAssembly $env:RSI_INSTALL_PROBE'],
      {env:{...process.env, RSI_INSTALL_PROBE:binary}, encoding:'utf8', timeout:30_000});
    assert.equal(build.status, 0, build.stderr);
    const result = installer(['-Prefix', prefix, '-Binary', binary, '-NoPath'], directory,
      {...process.env, RSI_INSTALL_COLLISION:destination});
    assert.notEqual(result.status, 0);
    assert((await stat(destination)).isDirectory());
    const {readdir} = await import('node:fs/promises');
    assert.deepEqual(await readdir(destination), []);
    assert.deepEqual(await readdir(join(prefix, 'bin')), ['rsi.exe']);
  } finally {await rm(directory, {recursive:true, force:true});}
});
test('POSIX PATH setup quotes the chosen prefix without evaluating its contents', {skip:windows}, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'rsi path '));
  try {
    const home = join(directory, 'home');
    await mkdir(home);
    await writeFile(join(home, '.profile'), '# existing\n');
    const prefix = join(directory, "quote ' $(touch UNEXPECTED)");
    const result = installer(['--binary', process.execPath, '--prefix', prefix], directory,
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
    const one = await assemble('core', first, input);
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
    const two = await assemble('core', second, input);
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
    const three = await assemble('core', third, input);
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
    const script = join(scripts, 'rsiversi.sh');
    await writeFile(script, await readFile(join(root, 'scripts/rsiversi.sh')));
    const declarations = join(devtools, 'build-environment.txt');
    await writeFile(declarations, 'PATH\nProgramFiles(x86)\nHOME=build-home\n');
    await writeFile(join(tools, 'cargo'), '#!/usr/bin/python3\nimport os,pathlib,shutil\np=pathlib.Path(os.environ["CARGO_TARGET_DIR"])\n(p/"release").mkdir(parents=True)\n(p/"sdk").write_text(os.environ["ProgramFiles(x86)"])\nshutil.copy2("/bin/true",p/"release/rsi")\n', {mode:0o700});
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
    const source=await readFile(join(root,'scripts/rsiversi.ps1'),'utf8');
    const line=source.split('\n').find(line=>line.includes('if ($declaration '));
    const expression=line.split('if (')[1].split(") { throw")[0];
    const script='$cases=ConvertFrom-Json $env:RSI_GRAMMAR_CASES; foreach($item in $cases){$declaration=$item.declaration; $rejected='+expression+'; if($rejected -eq $item.valid){throw "Declaration grammar diverged"}}';
    const result=spawnSync('powershell.exe',['-NoProfile','-Command',script],{encoding:'utf8',env:{...process.env,RSI_GRAMMAR_CASES:JSON.stringify(cases)}});
    assert.equal(result.status,0,result.stderr);
  } else {
    const source=await readFile(join(root,'scripts/rsiversi.sh'),'utf8');
    const condition=source.split('\n').find(line=>line.includes('[[ "$declaration" =~'));
    for(const item of cases) {
      const result=spawnSync('bash',['-c','declaration=$RSI_GRAMMAR_CASE; '+condition],{encoding:'utf8',env:{...process.env,RSI_GRAMMAR_CASE:item.declaration}});
      assert.equal(result.status===0,item.valid,item.declaration);
    }
  }
});
