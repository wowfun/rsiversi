// Explicit source producer. Installation and runtime selection belong to rsi.
import { spawn } from 'node:child_process';
import { mkdir, mkdtemp, lstat, readFile, readdir, rename, rm, writeFile } from 'node:fs/promises';
import { dirname, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { assemble } from './packages.mjs';

const root = fileURLToPath(new URL('../../', import.meta.url));
const usage = 'Usage: node apps/devtools/build.mjs core|web|desktop|ssh-helper ABSOLUTE_PACKAGE [--debug] [--replace]';
export function parse(args) {
  if (args.some(arg => ['--help', '-h'].includes(arg))) return null;
  const [kind, output, ...rest] = args;
  if (!['core', 'web', 'desktop', 'ssh-helper'].includes(kind) || !output || !isAbsolute(output))
    throw new Error(usage);
  let debug = false, replace = false;
  for (let index = 0; index < rest.length; index++) {
    if (rest[index] === '--debug' && !debug) debug = true;
    else if (rest[index] === '--replace' && !replace) replace = true;
    else throw new Error(usage);
  }
  if (kind === 'ssh-helper' && process.platform !== 'linux') throw new Error('Build the SSH helper on Linux.');
  return {kind, output, debug, replace};
}
export function parseEnvironmentDeclaration(declaration) {
  const match = /^([A-Za-z_][A-Za-z0-9_()]*)(?:=([a-z0-9_-]+(?:\/[a-z0-9_-]+)*))?$/.exec(declaration);
  if (!match) throw new Error('Invalid build environment declaration.');
  const [, key, suffix] = match;
  return {key, suffix};
}
export async function buildEnvironment(original = process.env) {
  const directory = join(root, 'target/rsi-build');
  const environment = {};
  const valueOf = key => original[key] ?? (process.platform === 'win32'
    ? original[Object.keys(original).find(name => name.toUpperCase() === key.toUpperCase())] : undefined);
  for (const declaration of (await readFile(join(root, 'apps/devtools/build-environment.txt'), 'utf8')).trim().split(/\r?\n/)) {
    const {key, suffix} = parseEnvironmentDeclaration(declaration);
    if (suffix) {
      environment[key] = join(directory, suffix);
      await mkdir(environment[key], {recursive:true});
    } else if (valueOf(key) !== undefined) environment[key] = valueOf(key);
  }
  const home = valueOf('USERPROFILE') ?? valueOf('HOME');
  if (!environment.CARGO_HOME && home) environment.CARGO_HOME = join(home, '.cargo');
  if (!environment.RUSTUP_HOME && home) environment.RUSTUP_HOME = join(home, '.rustup');
  environment.CI = 'true';
  return environment;
}
function run(program, args, environment) {
  return new Promise((resolveRun, reject) => {
    const child = spawn(program, args, {cwd:root, env:environment, stdio:'inherit'});
    const forward = signal => { if (process.platform !== 'win32') child.kill(signal); };
    const interrupt = () => forward('SIGINT'), terminate = () => forward('SIGTERM');
    process.on('SIGINT', interrupt); process.on('SIGTERM', terminate);
    child.once('error', reject);
    child.once('exit', (code, signal) => {
      process.off('SIGINT', interrupt); process.off('SIGTERM', terminate);
      code === 0 ? resolveRun() : reject(new Error(`${program} failed (${signal ?? code})`));
    });
  });
}
export async function build(options) {
  const {kind, output, debug, replace} = options;
  try {
    const info = await lstat(output);
    if (!replace || !info.isDirectory() || info.isSymbolicLink()) throw new Error('Output must be a new or explicitly replaceable ordinary directory.');
  }
  catch (error) { if (error.code !== 'ENOENT') throw error; }
  await mkdir(dirname(output), {recursive:true});
  const target = resolve(root, process.env.CARGO_TARGET_DIR ?? 'target');
  const environment = await buildEnvironment();
  const profile = debug ? 'debug' : 'release';
  let input, temporary;
  try {
    temporary = await mkdtemp(join(dirname(output), '.rsi-package-build-'));
    if (kind === 'web') {
      input = join(temporary, 'assets');
      await mkdir(input);
      await run(process.execPath, [join(root, 'apps/web/build.mjs'), input, ...(debug ? ['--dev'] : [])], environment);
    } else {
      const packageName = {core:'rsi-cli', desktop:'rsi-desktop', 'ssh-helper':'rsi-ssh-helper-app'}[kind];
      const name = {core:'rsi', desktop:'rsi-desktop', 'ssh-helper':'rsi-ssh-helper'}[kind];
      const triple = kind === 'ssh-helper'
        ? ({x64:'x86_64-unknown-linux-musl', arm64:'aarch64-unknown-linux-musl'}[process.arch]) : null;
      if (kind === 'ssh-helper' && !triple) throw new Error('Unsupported SSH helper CPU.');
      await run('cargo', ['build', '--locked', '-p', packageName, ...(debug ? [] : ['--release']),
        ...(kind === 'core' ? ['-p', 'rsi-uninstall'] : []),
        ...(triple ? ['--target', triple] : [])], environment);
      input = join(target, ...(triple ? [triple] : []), profile, name + (process.platform === 'win32' ? '.exe' : ''));
    }
    const stage = join(temporary, 'package');
    const uninstaller = kind === 'core' ? join(target, profile, 'rsi-uninstall' + (process.platform === 'win32' ? '.exe' : '')) : undefined;
    const manifest = await assemble(kind, stage, input, uninstaller);
    await publishPackage(stage, output, manifest, replace);
    return manifest;
  } finally {
    if (temporary) await rm(temporary, {recursive:true, force:true});
  }
}
// Only the complete candidate's declared files are replaced. Unrelated files
// and obsolete output files are never recursively deleted. The ordinary Host
// admits the manifest after command completion, into its immutable package store.
export async function publishPackage(stage, output, manifest, replace = false) {
  const owner = {format:1, name:manifest.name, target:manifest.target};
  const marker = join(output, '.rsi-source-package.json');
  try { await mkdir(output); }
  catch (error) {
    if (error.code !== 'EEXIST' || !replace) throw error;
    const info = await lstat(output);
    if (!info.isDirectory() || info.isSymbolicLink()) throw new Error('Output must be an ordinary directory.');
    // The owner marker also recovers an interrupted first publication before
    // package.json existed. It contains no executable paths or cleanup authority.
    let receiptPath = marker;
    try { await lstat(receiptPath); }
    catch (error) { if (error.code !== 'ENOENT') throw error; receiptPath = join(output, 'package.json'); }
    if ((await readdir(output)).length !== 0) {
      const receipt = await lstat(receiptPath);
      if (!receipt.isFile() || receipt.isSymbolicLink() || receipt.size > 1024 * 1024) throw new Error('Output must be a bounded package.');
      const previous = JSON.parse(await readFile(receiptPath, 'utf8'));
      if (previous.format !== (receiptPath === marker ? 1 : 2) || previous.name !== manifest.name || previous.target !== manifest.target)
        throw new Error('Output belongs to a different package.');
    }
  }
  try { await writeFile(marker, JSON.stringify(owner), {flag:'wx'}); }
  catch (error) { if (error.code !== 'EEXIST') throw error; }
  for (const file of [...manifest.files, {path:'package.json'}]) {
    let parent = output;
    for (const component of file.path.split('/').slice(0, -1)) {
      parent = join(parent, component);
      try { await mkdir(parent); } catch (error) { if (error.code !== 'EEXIST') throw error; }
      const info = await lstat(parent);
      if (!info.isDirectory() || info.isSymbolicLink()) throw new Error('Package output parent is not an ordinary directory.');
    }
    await rename(join(stage, file.path), join(output, file.path));
  }
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const options = parse(process.argv.slice(2));
    if (options) console.log(JSON.stringify(await build(options)));
    else console.log(usage);
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
