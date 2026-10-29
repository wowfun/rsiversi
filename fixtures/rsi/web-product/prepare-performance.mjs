import {readFile,writeFile,mkdir,cp,symlink,readdir} from 'node:fs/promises';
import {resolve,join,dirname} from 'node:path';
import {createHash} from 'node:crypto';
import {performanceEntrypoint} from './performance-input.mjs';
const args=process.argv.slice(2);
if(args.length!==2||!process.env.RSI_WEB_ASSETS) throw new Error('Pass baseline document source and a new output directory; set RSI_WEB_ASSETS');
const [baseline,directory]=args.map(path=>resolve(path));
await mkdir(directory,{recursive:false});
const root=resolve('apps/web'), scene=await readFile('fixtures/rsi/web-product/performance-scene.js','utf8'), legacyScene=await readFile('fixtures/rsi/web-product/performance-legacy-scene.js','utf8');
const {createRequire}=await import('node:module');
const require=createRequire(join(root,'package.json'));
const {build}=await import((await import('node:url')).pathToFileURL(require.resolve('vite')).href);
const digest=bytes=>createHash('sha256').update(bytes).digest('hex');
async function treeHashes(root, relative='') {
  const hashes={};
  for(const entry of (await readdir(join(root,relative),{withFileTypes:true})).sort((a,b)=>a.name.localeCompare(b.name))) {
    if(entry.name==='node_modules') continue;
    const path=join(relative,entry.name);
    if(entry.isDirectory()) Object.assign(hashes,await treeHashes(root,path));
    else if(entry.isFile()) hashes[path]=digest(await readFile(join(root,path)));
    else throw new Error(`Performance input must be a regular file or directory: ${path}`);
  }
  return hashes;
}
const generated=join(directory,'generated-assets');
await cp(process.env.RSI_WEB_ASSETS,generated,{recursive:true});
const sharedInputs = await Promise.all([
  'apps/devtools/wire-descriptors.mjs',
  'apps/dev-control/wire-contract.json',
  'apps/dev-control/conformance.json',
  'crates/rsi/desktop-bridge/wire-contract.json',
  'crates/rsi/web-assets/bundle.json',
].map(async path => [path, await readFile(resolve(path))]));
// Instrumented documents deliberately carry no product pairing receipt.
await (await import('node:fs/promises')).rm(join(generated,'rsi-build.json'),{force:true});
const hashes={},sources={},outputs={};
for(const [variant,input] of [['baseline',baseline],['current',root]]) {
  const sourceRoot=join(directory,`${variant}-source`),source=join(sourceRoot,'apps/web'), output=join(directory,variant);
  await cp(input,source,{recursive:true,filter:path=>!path.split(/[\\/]/).includes('node_modules')});
  for (const [path, bytes] of sharedInputs) {
    const target=join(sourceRoot,path);
    await mkdir(dirname(target),{recursive:true});
    await writeFile(target,bytes);
  }
  await symlink(join(root,'node_modules'),join(source,'node_modules'),process.platform==='win32'?'junction':'dir');
  sources[variant]=await treeHashes(sourceRoot);
  const {bytes:original,identity}=await performanceEntrypoint(source,variant);
  hashes[variant]=identity;
  if (variant==='baseline') {
    // Frozen pre-refactor adapter only. Never adapt the current production source.
    const app=original.toString('utf8').replace('export function initialize() {','export async function initialize() {');
    const start=app.lastIndexOf('\nif (nativeDocument) {');
    if(start<0) throw new Error('Frozen baseline bootstrap marker changed');
    await writeFile(join(source,'app.js'),app.slice(0,start)+legacyScene+'\n}\n');
  } else {
    await writeFile(join(source,'performance-scene.js'),scene);
    await writeFile(join(source,'src/main.tsx'),"import '../performance-scene.js';\n");
  }
  await cp(generated,output,{recursive:true});
  await build({root:source,configFile:join(source,'vite.config.mjs'),build:{outDir:output,emptyOutDir:false}});
  outputs[variant]=await treeHashes(output);
}
await writeFile(join(directory,'instrumentation.json'),JSON.stringify({format:2,source_entrypoints:hashes,source_files_sha256:sources,generated_assets_sha256:await treeHashes(generated),output_files_sha256:outputs,build_pnpm_lock_sha256:digest(await readFile(join(root,'pnpm-lock.yaml'))),node:process.version,scene_sha256:digest(scene),legacy_scene_sha256:digest(legacyScene),boundary:'Document projection and persistence; same frozen generated assets and isolated browser engine. No product startup, Rust frame, Host, transport or provider timing.'},null,2));
