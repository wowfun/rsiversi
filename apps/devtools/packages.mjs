import {wireDescriptor, validProtocolIdentity} from './wire-descriptors.mjs';
import {createHash, createPrivateKey, createPublicKey, sign} from 'node:crypto';
import {readFile, writeFile, mkdir, readdir, lstat, open} from 'node:fs/promises';
import {constants} from 'node:fs';
import {spawnSync} from 'node:child_process';
import {resolve, dirname, join, relative, isAbsolute} from 'node:path';
import {fileURLToPath} from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const maximumFileBytes = 4 * 1024 ** 3, maximumStoreBytes = 16 * 1024 ** 3;
async function copyPayload(source, destination, executable) {
  const input = await open(source,constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  try {
    const output = await open(destination,'wx',executable ? 0o700 : 0o600);
    try {
      let copied = 0;
      for await (const chunk of input.createReadStream({autoClose:false,highWaterMark:64*1024})) {
        copied += chunk.length;
        if (copied > maximumFileBytes) throw new Error('Package file exceeds capacity');
        let offset = 0;
        while (offset < chunk.length) {
          const {bytesWritten} = await output.write(chunk,offset,chunk.length-offset);
          if (!bytesWritten) throw new Error('Package copy made no progress');
          offset += bytesWritten;
        }
      }
      await output.sync();
    } finally { await output.close(); }
  } finally { await input.close(); }
}
async function fileDigest(path, limit = maximumFileBytes) {
  const before = await lstat(path);
  if (!before.isFile() || before.isSymbolicLink() || before.size > limit) throw new Error(`Invalid bounded file: ${path}`);
  const file = await open(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  try {
    const opened = await file.stat();
    if (!opened.isFile() || opened.dev !== before.dev || opened.ino !== before.ino) throw new Error('Package input changed');
    const digest = createHash('sha256');
    let bytes = 0;
    for await (const chunk of file.createReadStream({autoClose:false,highWaterMark:64*1024})) {
      bytes += chunk.length;
      if (bytes > limit) throw new Error('Package file exceeds capacity');
      digest.update(chunk);
    }
    const after = await file.stat();
    if (bytes !== opened.size || after.size !== opened.size || after.mtimeMs !== opened.mtimeMs || after.ctimeMs !== opened.ctimeMs)
      throw new Error('Package input changed during hashing');
    return {bytes,digest:digest.digest('hex')};
  } finally { await file.close(); }
}
const platforms = new Map([['win32', 'windows'], ['linux', 'linux'], ['darwin', 'macos']]);
const architectures = new Map([['x64', 'x86_64'], ['arm64', 'aarch64']]);
const target = `${platforms.get(process.platform)}-${architectures.get(process.arch)}`;
const identifier = value => typeof value === 'string' && /^[a-z0-9_-][a-z0-9_.-]{0,127}$/.test(value) && !value.endsWith('.');
const digest = value => typeof value === 'string' && /^[a-f0-9]{64}$/.test(value);
const portable = path => typeof path === 'string' && path.length > 0 && path.length <= 1024 && path.split('/').length <= 128
  && /^[\x00-\x7f]*$/.test(path) && path.split('/').every(part => part.length > 0 && part.length <= 200
    && !part.startsWith('.') && !part.endsWith('.') && !part.endsWith(' ')
    && !/[\x00-\x1f\\:*?"<>|]/.test(part) && !/^(?:con|prn|aux|nul|conin\$|conout\$|com[1-9]|lpt[1-9])(?:\.|$)/i.test(part));
const fields = (value, names) => value && typeof value === 'object' && !Array.isArray(value)
  && Object.keys(value).length === names.length && names.every(name => Object.hasOwn(value, name));
export function parseManifest(bytes) {
  const source = bytes.toString('utf8'), value = JSON.parse(source), stack = [];
  // JSON.parse discards duplicate keys and integer token spelling; Serde refuses
  // both duplicate fields and floating-point tokens for the manifest's integers.
  for (const token of source.matchAll(/"(?:\\.|[^"\\])*"|[{}\[\]]|(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)/g)) {
    const spelling = token[0];
    if (spelling === '{') stack.push(new Set());
    else if (spelling === '[') stack.push(null);
    else if (spelling === '}' || spelling === ']') stack.pop();
    else if (token[1] !== undefined && !/^(0|[1-9]\d*)$/.test(spelling)) throw new Error('Invalid package integer token');
    else if (spelling.startsWith('"') && source.slice(token.index + spelling.length).trimStart().startsWith(':')) {
      const key = JSON.parse(spelling), keys = stack.at(-1);
      if (keys.has(key)) throw new Error('Invalid package duplicate field');
      keys.add(key);
    }
  }
  validateManifest(value);
  return value;
}
export function validateManifest(manifest) {
  const fail = () => { throw new Error('Invalid package manifest'); };
  if (!fields(manifest,['format','name','version','target','dependencies','files','exports','requires'])
      || manifest.format !== 2 || !identifier(manifest.name) || typeof manifest.version !== 'string' || !/^[!-~]{1,128}$/.test(manifest.version)
      || !/^(portable|(?:windows|linux|macos)-(?:x86_64|aarch64))$/.test(manifest.target)
      || !Array.isArray(manifest.files) || manifest.files.length < 1 || manifest.files.length > 4096
      || !Array.isArray(manifest.dependencies) || manifest.dependencies.length > 128
      || !Array.isArray(manifest.requires) || manifest.requires.length > 128
      || !manifest.exports || typeof manifest.exports !== 'object' || Array.isArray(manifest.exports) || Object.keys(manifest.exports).length > 128) fail();
  const paths = new Set(); let total = 0;
  for (const file of manifest.files) {
    if (!fields(file,['path','bytes','digest','executable']) || !portable(file.path) || !digest(file.digest)
        || typeof file.executable !== 'boolean' || !Number.isSafeInteger(file.bytes) || file.bytes < 0 || file.bytes > maximumFileBytes
        || paths.has(file.path.toLowerCase())) fail();
    paths.add(file.path.toLowerCase()); total += file.bytes;
  }
  if (total > maximumStoreBytes) fail();
  for (const file of manifest.files) {
    const components = file.path.toLowerCase().split('/');
    for (let end = 1; end < components.length; end++) if (paths.has(components.slice(0,end).join('/'))) fail();
  }
  for (const [name, item] of Object.entries(manifest.exports)) {
    if (!identifier(name) || !fields(item,['kind','path']) || !portable(item.path)
        || !['web_assets','executable','application_profile','service_profile','native_addon'].includes(item.kind)) fail();
    if (!manifest.files.some(file => item.kind === 'web_assets' ? file.path.startsWith(item.path+'/')
      : file.path === item.path && (item.kind !== 'executable' || file.executable))) fail();
  }
  const dependencies = new Set(), interfaces = new Set();
  for (const dependency of manifest.dependencies) {
    if (!fields(dependency,['name','digest']) || !identifier(dependency.name) || !digest(dependency.digest)
        || dependency.name === manifest.name || dependencies.has(dependency.name)) fail();
    dependencies.add(dependency.name);
  }
  for (const required of manifest.requires) {
    if (!fields(required,['name','version']) || !validProtocolIdentity(required.name, required.version) || interfaces.has(required.name)) fail();
    interfaces.add(required.name);
  }
}
async function bounded(path, limit = 1024 * 1024) {
  const stat = await lstat(path);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size > limit) throw new Error(`Invalid bounded file: ${path}`);
  const bytes = await readFile(path);
  if (bytes.length > limit) throw new Error(`File grew beyond limit: ${path}`);
  return bytes;
}
async function descriptor(directory) {
  return wireDescriptor(join(directory, 'wire-contract.json'));
}
async function webRequirements() {
  const cargo = spawnSync('cargo', ['metadata', '--locked', '--format-version', '1',
    '--filter-platform', 'wasm32-unknown-unknown'], {cwd:root, encoding:'utf8', maxBuffer:32 * 1024 * 1024});
  if (cargo.status !== 0) throw cargo.error ?? new Error(cargo.stderr);
  const metadata = JSON.parse(cargo.stdout);
  const entry = metadata.packages.find(pkg => pkg.name === 'rsi-web');
  if (!entry) throw new Error('Missing Web package');
  const requirements = new Map();
  for (const pkg of hostInterfacePackages(metadata, entry)) {
    const value = await descriptor(dirname(pkg.manifest_path));
    if (requirements.has(value.name)) throw new Error(`Duplicate Host interface: ${value.name}`);
    requirements.set(value.name, value);
  }
  return [...requirements.values()].sort((a,b) => a.name.localeCompare(b.name));
}
export function hostInterfacePackages(metadata, entry) {
  const declared = entry.metadata?.rsi?.['host-interface-packages'];
  if (!Array.isArray(declared) || !declared.length || declared.length > 128 ||
      declared.some(name => typeof name !== 'string' || !/^[a-z0-9][a-z0-9-]{0,127}$/.test(name)) ||
      new Set(declared).size !== declared.length) throw new Error('Invalid declared Host interfaces');
  const packages = new Map(metadata.packages.map(pkg => [pkg.id, pkg]));
  const nodes = new Map(metadata.resolve.nodes.map(node => [node.id, node]));
  const pending = [entry.id], seen = new Set();
  while (pending.length) {
    const id = pending.pop();
    if (seen.has(id)) continue;
    seen.add(id);
    for (const dep of nodes.get(id)?.deps ?? []) {
      if (dep.dep_kinds.some(kind => kind.kind === null)) pending.push(dep.pkg);
    }
  }
  return declared.map(name => {
    const matches = [...seen].filter(id => id !== entry.id).map(id => packages.get(id))
      .filter(pkg => pkg && !pkg.source && pkg.name === name);
    if (matches.length !== 1) throw new Error(`Host interface must name one WASM normal dependency: ${name}`);
    return matches[0];
  });
}

export async function assemble(kind, output, input) {
  if (!['core','web','desktop','ssh-helper'].includes(kind) || !isAbsolute(output) || !isAbsolute(input))
    throw new Error('assemble requires core|web|desktop|ssh-helper and absolute output/input paths');
  if (kind === 'ssh-helper' && process.platform !== 'linux') throw new Error('SSH helper publication requires Linux');
  if (!platforms.has(process.platform) || !architectures.has(process.arch)) throw new Error('Unsupported native target');
  const version = process.env.RSI_RELEASE_VERSION ?? '0.0.1-local';
  if (!/^[!-~]{1,128}$/.test(version)) throw new Error('Invalid package version');
  const manifest = {format:2,name:kind,version,target:kind === 'web' ? 'portable' : target,
    dependencies:[],files:[],exports:{},requires:[]};
  const copies = [];
  if (kind === 'web') {
    if (!(await lstat(input)).isDirectory()) throw new Error('Web input must be a directory');
    const names = (await readdir(input)).sort();
    if (names.length > 4096 || !names.includes('rsi-build.json')) throw new Error('Invalid Web bundle');
    for (const name of names) copies.push([join(input,name), `assets/${name}`, false]);
    manifest.exports.web = {kind:'web_assets',path:'assets'};
    manifest.requires = await webRequirements();
  } else {
    const filename = `${kind === 'core' ? 'rsi' : `rsi-${kind}`}${process.platform === 'win32' ? '.exe' : ''}`;
    copies.push([input, filename, true]);
    manifest.exports[kind] = {kind:'executable',path:filename};
    if (kind === 'ssh-helper') manifest.requires.push(await descriptor(join(root,'crates/rsi-ssh/protocol')));
    if (kind === 'desktop') {
      manifest.requires.push(await descriptor(join(root,'crates/rsi/web-control')));
    }
  }
  let total = 0;
  // All inputs are inspected before creating the new output directory.
  for (const [source,path,executable] of copies) {
    if (!portable(path)) throw new Error(`Invalid package path: ${path}`);
    if (kind === 'ssh-helper' && (await lstat(source)).size > 128 * 1024 * 1024)
      throw new Error('SSH helper exceeds its 128 MiB transport bound; use a release build or an explicitly stripped artifact');
    const content = await fileDigest(source);
    total += content.bytes;
    if (total > maximumStoreBytes) throw new Error('Package exceeds capacity');
    manifest.files.push({path,...content,executable});
  }
  await mkdir(output); // Never reuse an existing publication.
  for (const [source,path,executable] of copies) {
    await mkdir(dirname(join(output,path)),{recursive:true});
    await copyPayload(source,join(output,path),executable);
  }
  // Recheck copied bytes: a producer changing its output during assembly fails.
  await verifyFiles(output,manifest);
  await writeFile(join(output,'package.json'),JSON.stringify(manifest),{flag:'wx'});
  return manifest;
}
async function verifyFiles(directory, manifest) {
  validateManifest(manifest);
  let total = 0;
  const seen = new Set();
  for (const file of manifest.files) {
    if (!portable(file.path) || seen.has(file.path.toLowerCase()) || !Number.isSafeInteger(file.bytes)
      || file.bytes < 0 || file.bytes > maximumFileBytes || !/^[a-f0-9]{64}$/.test(file.digest))
      throw new Error('Invalid package file declaration');
    seen.add(file.path.toLowerCase());
    let parent = directory;
    for (const component of file.path.split('/').slice(0,-1)) {
      parent = join(parent,component);
      if (!(await lstat(parent)).isDirectory() || (await lstat(parent)).isSymbolicLink())
        throw new Error('Package parent is not a regular directory');
    }
    const content = await fileDigest(join(directory,file.path),file.bytes);
    if (content.bytes !== file.bytes || content.digest !== file.digest) throw new Error('Package file digest differs');
    total += content.bytes;
    if (total > maximumStoreBytes) throw new Error('Package exceeds capacity');
  }
}
export async function signRepository(directory, name, sequence, pem, expectedKey, now = Math.floor(Date.now()/1000)) {
  if (!identifier(name) || !Number.isSafeInteger(sequence) || sequence < 1)
    throw new Error('Invalid repository name or sequence');
  const key = createPrivateKey(pem);
  if (key.asymmetricKeyType !== 'ed25519') throw new Error('Repository signing requires Ed25519');
  const publicKey = createPublicKey(key).export({type:'spki',format:'der'}).subarray(-32).toString('hex');
  if (publicKey !== expectedKey) throw new Error('Signing key differs from pinned repository key');
  const packages = Object.create(null), manifests = [];
  let directories = 0;
  async function visit(path,depth=0) {
    if (depth > 3 || manifests.length > 128 * 8 || ++directories > 4096) throw new Error('Publication bounds');
    const names = await readdir(path);
    if (names.length > 4096) throw new Error('Publication directory bounds');
    if (names.includes('package.json')) {
      const bytes = await bounded(join(path,'package.json'));
      const manifest = parseManifest(bytes);
      if (!identifier(manifest.name)
        || !/^(portable|(?:windows|linux|macos)-(?:x86_64|aarch64))$/.test(manifest.target))
        throw new Error('Invalid package selection');
      await verifyFiles(path,manifest);
      const selected = packages[manifest.name] ??= Object.create(null);
      if (selected[manifest.target]) throw new Error('Duplicate package target');
      const location = relative(directory,join(path,'package.json')).replaceAll('\\','/');
      if (!portable(location)) throw new Error('Invalid published manifest location');
      selected[manifest.target] = {digest:hash(bytes),manifest:location};
      manifests.push(manifest);
      return;
    }
    for (const name of names.sort()) {
      const child = join(path,name), stat = await lstat(child);
      if (stat.isSymbolicLink()) throw new Error('Publication refuses links');
      if (stat.isDirectory()) await visit(child,depth+1);
    }
  }
  await visit(directory);
  if (manifests.length === 0 || Object.keys(packages).length > 128) throw new Error('Repository package bounds');
  for (const manifest of manifests) for (const dependency of manifest.dependencies) {
    const selection = packages[dependency.name]?.[manifest.target] ?? packages[dependency.name]?.portable;
    if (!selection || selection.digest !== dependency.digest) throw new Error('Publication dependency is not exact');
  }
  const payload = Buffer.from(JSON.stringify({format:1,name,sequence,issued:now,expires:now+7*86400,packages}));
  if (payload.length > 1024 * 1024) throw new Error('Repository payload exceeds capacity');
  const envelope = {payload:payload.toString('hex'),signature:sign(null,payload,key).toString('hex')};
  await writeFile(join(directory,'repository.json'),JSON.stringify(envelope),{flag:'wx'});
  return envelope;
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [operation,...args] = process.argv.slice(2);
  try {
    if (operation === 'assemble' && args.length === 3) await assemble(...args);
    else if (operation === 'sign' && args.length === 3) await signRepository(args[0],args[1],Number(args[2]),
      process.env.RSI_REPOSITORY_SIGNING_KEY,process.env.RSI_PACKAGE_REPOSITORY_KEY);
    else throw new Error('Usage: packages.mjs assemble KIND OUTPUT INPUT | sign ROOT NAME SEQUENCE');
  } catch (error) { console.error(error.message); process.exitCode=1; }
}
