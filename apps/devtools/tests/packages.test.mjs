import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp, writeFile, readFile, rm, mkdir, truncate, stat} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join, resolve, dirname, basename} from 'node:path';
import {generateKeyPairSync, verify, createHash} from 'node:crypto';
import {assemble, signRepository, hostInterfacePackages, validateManifest, parseManifest} from '../packages.mjs';

async function cleanup(root) {
  const absolute = resolve(root);
  assert.equal(dirname(absolute),resolve(tmpdir()));
  assert.ok(basename(absolute).startsWith('rsi-package-'));
  await rm(absolute,{recursive:true,force:true});
}
test('independent native package and pinned signing preserve exact executable bytes', async () => {
  const root = await mkdtemp(join(tmpdir(),'rsi-package-'));
  try {
    const source = join(root,'input'), repository = join(root,'repository');
    await writeFile(source,'fixture executable');
    await mkdir(repository);
    const manifest = await assemble('core',join(repository,'core'),source);
    const {privateKey,publicKey} = generateKeyPairSync('ed25519');
    const pem = privateKey.export({type:'pkcs8',format:'pem'});
    const pin = publicKey.export({type:'spki',format:'der'}).subarray(-32).toString('hex');
    await assert.rejects(signRepository(repository,'fixture',1,pem,'0'.repeat(64)),/pinned/);
    const envelope = await signRepository(repository,'fixture',1,pem,pin,1000);
    const payload = Buffer.from(envelope.payload,'hex');
    assert.ok(verify(null,payload,publicKey,Buffer.from(envelope.signature,'hex')));
    const index = JSON.parse(payload);
    const bytes = await readFile(join(repository,'core','package.json'));
    assert.equal(index.packages.core[manifest.target].digest,createHash('sha256').update(bytes).digest('hex'));
    assert.deepEqual(manifest.requires,[]);
    await assert.rejects(assemble('core',join(repository,'core'),source),/exist/i);
    await writeFile(join(repository,'core',manifest.files[0].path),'tampered');
    await assert.rejects(signRepository(repository,'fixture',2,pem,pin),/digest/);
  } finally { await cleanup(root); }
});

test('Desktop dependency must match the published Web manifest exactly', async () => {
  const root = await mkdtemp(join(tmpdir(),'rsi-package-'));
  try {
    const directory = join(root,'web');
    await mkdir(directory);
    const content = Buffer.from('file');
    const file = {path:'asset',bytes:4,digest:createHash('sha256').update(content).digest('hex'),executable:false};
    await writeFile(join(directory,'asset'),content);
    const web = {format:1,name:'web',target:'portable',version:'1',files:[file],dependencies:[],exports:{},requires:[]};
    await writeFile(join(directory,'package.json'),JSON.stringify(web));
    await writeFile(join(root,'desktop-input'),'native');
    const desktop = await assemble('desktop',join(root,'desktop'),join(root,'desktop-input'),join(directory,'package.json'));
    assert.equal(desktop.requires[0].name,'rsi.web.control');
    web.version = '2';
    await writeFile(join(directory,'package.json'),JSON.stringify(web));
    const {privateKey,publicKey} = generateKeyPairSync('ed25519');
    await assert.rejects(signRepository(root,'fixture',1,privateKey.export({type:'pkcs8',format:'pem'}),
      publicKey.export({type:'spki',format:'der'}).subarray(-32).toString('hex')),/dependency/);
  } finally { await cleanup(root); }
});

test('SSH helper is an independent Linux package with its own required interface', async () => {
  const root = await mkdtemp(join(tmpdir(),'rsi-package-'));
  try {
    const input = join(root,'helper');
    await writeFile(input,'fixture helper');
    if (process.platform !== 'linux') {
      await assert.rejects(assemble('ssh-helper',join(root,'package'),input),/requires Linux/);
      return;
    }
    const manifest = await assemble('ssh-helper',join(root,'package'),input);
    assert.equal(manifest.requires.length,1);
    assert.equal(manifest.requires[0].name,'rsi.ssh.helper');
    assert.deepEqual(manifest.dependencies,[]);
    assert.equal(manifest.exports['ssh-helper'].path,'rsi-ssh-helper');
    await truncate(input,128 * 1024 * 1024 + 1);
    const rejected = join(root,'oversized');
    await assert.rejects(assemble('ssh-helper',rejected,input),/128 MiB transport bound/);
    await assert.rejects(stat(rejected),{code:'ENOENT'});
  } finally { await cleanup(root); }
});

test('Web requires declared Host interfaces without promoting auxiliary codecs or development dependencies', () => {
  const entry = {id:'worker',name:'rsi-web',metadata:{rsi:{'host-interface-packages':['host-api']}}};
  const api = {id:'api',name:'host-api',source:null};
  const auxiliary = {id:'helper',name:'native-helper',source:null};
  const development = {id:'test',name:'test-api',source:null};
  const edge = (pkg,kind=null) => ({pkg,dep_kinds:[{kind}]});
  const metadata = {packages:[entry,api,auxiliary,development],resolve:{nodes:[
    {id:'worker',deps:[edge('api'),edge('test','dev')]},
    {id:'api',deps:[edge('helper')]}, {id:'helper',deps:[]}, {id:'test',deps:[]},
  ]}};
  assert.deepEqual(hostInterfacePackages(metadata,entry),[api]);
  for (const declared of [['test-api'],['missing-api'],['host-api','host-api'],[]]) {
    entry.metadata.rsi['host-interface-packages']=declared;
    assert.throws(()=>hostInterfacePackages(metadata,entry),/Host interface/);
  }
});

test('publisher and consumer share manifest acceptance cases', async () => {
  const cases = JSON.parse(await readFile(new URL('../../../crates/rsi/packages/manifest-conformance.json', import.meta.url)));
  for (const sample of cases) {
    if (sample.valid) assert.doesNotThrow(() => validateManifest(sample.manifest), sample.name);
    else assert.throws(() => validateManifest(sample.manifest), /Invalid package/, sample.name);
  }
});

test('signing refuses JSON forms rejected by Serde integer and duplicate-field admission', async () => {
  const [sample] = JSON.parse(await readFile(new URL('../../../crates/rsi/packages/manifest-conformance.json', import.meta.url)));
  const valid = JSON.stringify(sample.manifest);
  assert.doesNotThrow(() => parseManifest(Buffer.from(valid)));
  assert.throws(() => parseManifest(Buffer.from(valid.replace('"format":1', '"format":1.0'))), /integer/);
  assert.throws(() => parseManifest(Buffer.from(valid.replace('"format":1', '"format":1,"format":1'))), /duplicate/);
});

test('protocol identities follow the shared Rust corpus independently of package names', async () => {
  const {validProtocolIdentity} = await import('../wire-descriptors.mjs');
  const cases = JSON.parse(await readFile(new URL('../../../crates/rsi-api/protocol/descriptor-conformance.json',import.meta.url),'utf8'));
  for (const item of cases) assert.equal(validProtocolIdentity(item.name,item.version),item.valid,JSON.stringify(item));
});
