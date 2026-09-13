import {readFile,writeFile,mkdir,cp,symlink,readdir} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {createHash} from 'node:crypto';
const args=process.argv.slice(2);
if(args.length!==2||!process.env.RSI_WEB_ASSETS) throw new Error('Pass baseline document source and a new output directory; set RSI_WEB_ASSETS');
const [baseline,directory]=args.map(path=>resolve(path));
await mkdir(directory,{recursive:false});
const root=resolve('plugins/rsi/web'), scene=await readFile('fixtures/rsi/web-product/performance-scene.js','utf8');
const {build}=await import(join(root,'node_modules/vite/dist/node/index.js'));
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
const hashes={},sources={},outputs={};
for(const [variant,input] of [['baseline',baseline],['current',root]]) {
  const source=join(directory,`${variant}-source`), output=join(directory,variant);
  await cp(input,source,{recursive:true,filter:path=>!path.split('/').includes('node_modules')});
  await symlink(join(root,'node_modules'),join(source,'node_modules'),'dir');
  sources[variant]=await treeHashes(source);
  let app=await readFile(join(source,'app.js'),'utf8');
  hashes[variant]=createHash('sha256').update(app).digest('hex');
  app=app.replace('export function initialize() {','export async function initialize() {');
  const start=app.lastIndexOf('if (location.protocol === "rsi:") {');
  if(start<0) throw new Error('Native bootstrap marker changed; review instrumentation');
  app=app.slice(0,start)+scene+'\n}\n';
  await writeFile(join(source,'app.js'),app);
  await cp(generated,output,{recursive:true});
  await build({root:source,configFile:join(source,'vite.config.mjs'),build:{outDir:output,emptyOutDir:false}});
  outputs[variant]=await treeHashes(output);
}
await writeFile(join(directory,'instrumentation.json'),JSON.stringify({format:1,source_app_sha256:hashes,source_files_sha256:sources,generated_assets_sha256:await treeHashes(generated),output_files_sha256:outputs,build_package_lock_sha256:digest(await readFile(join(root,'package-lock.json'))),node:process.version,scene_sha256:digest(scene),boundary:'Document projection and persistence; same frozen generated assets and native Host. No Rust frame, transport or provider timing.'},null,2));
