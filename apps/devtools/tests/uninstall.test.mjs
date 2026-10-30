import assert from 'node:assert/strict';
import {test} from 'node:test';
import {spawnSync} from 'node:child_process';
import {mkdtemp,mkdir,writeFile,readFile,rm,readdir,realpath,symlink,chmod} from 'node:fs/promises';
import {existsSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,resolve,dirname} from 'node:path';
import {fileURLToPath} from 'node:url';

const root=fileURLToPath(new URL('../../../',import.meta.url));
const temporaryRoot=await realpath(tmpdir());
const windows=process.platform==='win32';
const tool=process.env.RSI_TEST_UNINSTALLER??join(root,'target/debug',windows?'rsi-uninstall.exe':'rsi-uninstall');
const suffix=windows?'ps1':'sh';
function script(name,args,cwd) {
  return spawnSync(windows?'powershell.exe':'bash',windows?['-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',join(root,`scripts/${name}.ps1`),...args]:[join(root,`scripts/${name}.sh`),...args],{cwd,encoding:'utf8',timeout:60_000});
}
const flag=name=>windows?'-'+name:'--'+name.replace(/[A-Z]/g,(letter,index)=>(index?'-':'')+letter.toLowerCase());
async function fixture() {
  const directory=await mkdtemp(join(temporaryRoot,'rsi uninstall 中文 '));
  const prefix=join(directory,"prefix ' & $value"),config=join(directory,'config'),state=join(directory,'state'),cache=join(directory,'cache');
  for(const path of [config,state,cache])await mkdir(path);
  const args=[flag('Prefix'),prefix,flag('ConfigHome'),config,flag('StateHome'),state,flag('CacheHome'),cache,flag('NoPath')];
  const installed=script('install',[flag('Prefix'),prefix,flag('Binary'),tool,flag('Uninstaller'),tool,flag('NoPath')],directory);
  assert.equal(installed.status,0,installed.stderr);
  return {directory,prefix,config,state,cache,args,core:join(prefix,'bin',windows?'rsi.exe':'rsi'),control:join(prefix,'libexec/rsi')};
}
async function clean(f){await rm(f.directory,{recursive:true,force:true});}

test('native uninstall launcher previews without mutations, preserves data and can run twice',async()=>{
  const f=await fixture();try{
    await writeFile(join(f.config,'rsi.toml'),'authored');await mkdir(join(f.state,'agent'));await writeFile(join(f.state,'agent/data'),'session');
    const record=await readFile(join(f.control,'install.json'));
    const dry=script('uninstall',[...f.args,flag('DryRun')],f.directory);
    assert.equal(dry.status,0,dry.stderr);assert.match(dry.stdout,/Homes are shared/);
    assert.deepEqual(await readFile(join(f.control,'install.json')),record);
    assert(!existsSync(join(f.state,'session-host')));
    const removed=script('uninstall',[...f.args,flag('Yes')],f.directory);
    assert.equal(removed.status,0,removed.stderr);assert(!existsSync(f.core));assert(!existsSync(join(f.control,'install.json')));
    assert.equal(await readFile(join(f.config,'rsi.toml'),'utf8'),'authored');assert.equal(await readFile(join(f.state,'agent/data'),'utf8'),'session');
    const again=script('uninstall',f.args,f.directory);assert.equal(again.status,0,again.stderr);assert.match(again.stdout,/homes were not cleaned/);
    const late=script('uninstall',[...f.args,flag('Purge'),flag('Yes')],f.directory);assert.notEqual(late.status,0);assert.match(late.stderr,/prebuilt/i);
    const purge=script('uninstall',[...f.args,flag('Uninstaller'),tool,flag('Purge'),flag('Yes')],f.directory);
    assert.equal(purge.status,0,purge.stderr);assert(!existsSync(join(f.state,'agent')));assert(existsSync(join(f.config,'rsi.toml')));
  }finally{await clean(f);}
});
test('external tool removes a corrupt main without executing it and force handles invalid records',async()=>{
  const f=await fixture();try{
    await writeFile(f.core,'not executable');
    const dry=script('uninstall',[...f.args,flag('DryRun')],f.directory);assert.equal(dry.status,0,dry.stderr);
    await writeFile(join(f.control,'install.json'),'broken record');
    const refused=script('uninstall',[...f.args,flag('Uninstaller'),tool,flag('Yes')],f.directory);assert.notEqual(refused.status,0);assert(existsSync(f.core));
    const removed=script('uninstall',[...f.args,flag('Uninstaller'),tool,flag('Force'),flag('Yes')],f.directory);
    assert.equal(removed.status,0,removed.stderr);assert(!existsSync(f.core));
  }finally{await clean(f);}
});
test('purge refuses unknown backup authority before deleting the main',async()=>{
  const f=await fixture();try{
    await writeFile(join(f.config,'.import-unknown-backup-credentials'),'secret');
    const result=script('uninstall',[...f.args,flag('Purge'),flag('Force'),flag('Yes')],f.directory);
    assert.notEqual(result.status,0);assert.match(result.stderr,/unverified backup/);assert(existsSync(f.core));
    assert.doesNotMatch(result.stderr,/CategoryInfo|FullyQualifiedErrorId/);
  }finally{await clean(f);}
});
test('descendant junction or symlink removal never deletes its external target',async()=>{
  const f=await fixture();try{
    const outside=join(f.directory,'outside');await mkdir(outside);await writeFile(join(outside,'keep'),'outside');
    await mkdir(join(f.state,'agent'));await symlink(outside,join(f.state,'agent/link'),windows?'junction':'dir');
    const result=script('uninstall',[...f.args,flag('Purge'),flag('Yes')],f.directory);
    assert.equal(result.status,0,result.stderr);assert.equal(await readFile(join(outside,'keep'),'utf8'),'outside');
  }finally{await clean(f);}
});
test('pending upgrade is never bypassed by force',async()=>{
  const f=await fixture();try{
    await writeFile(join(f.prefix,'bin/.rsi-core-upgrade.pending.json'),'unknown');
    const result=script('uninstall',[...f.args,flag('Force'),flag('Yes')],f.directory);
    assert.notEqual(result.status,0);assert.match(result.stderr,/recover/);assert(existsSync(f.core));
  }finally{await clean(f);}
});
test('uninstall protocol drift refuses before any effect',()=>{
  const result=spawnSync(tool,['uninstall','--interface-version','999'],{encoding:'utf8'});
  assert.notEqual(result.status,0);assert.match(result.stderr,/interface-version 1/);
});
test('installed launcher works without the source checkout',async()=>{
  const f=await fixture();try{
    const path=join(f.control,`uninstall.${suffix}`);
    const args=f.args.slice(2);
    const result=spawnSync(windows?'powershell.exe':'bash',windows?['-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',path,...args,flag('Yes')]:[path,...args,flag('Yes')],{cwd:f.directory,encoding:'utf8',timeout:60_000});
    assert.equal(result.status,0,result.stderr);assert(!existsSync(f.core));
  }finally{await clean(f);}
});


test('reinstallation during removal reports the retry action before invoking Cargo',async()=>{
  const f=await fixture();try{
    const path=join(f.control,'install.json');const record=JSON.parse(await readFile(path,'utf8'));
    record.phase='uninstalling';await writeFile(path,JSON.stringify(record));
    const result=script('install',[flag('Prefix'),f.prefix,flag('NoPath')],f.directory);
    assert.notEqual(result.status,0);assert.match(result.stderr,/Finish uninstall/);
    assert.doesNotMatch(result.stderr,/core upgrade|Building rsi|CategoryInfo/);assert(existsSync(f.core));
  }finally{await clean(f);}
});

test('Git Bash uninstall forwards all five native paths and flags', {skip:!windows},async()=>{
  const f=await fixture();try{
    const where=spawnSync('where.exe',['git.exe'],{encoding:'utf8'});assert.equal(where.status,0);
    const candidates=where.stdout.trim().split(/\r?\n/).flatMap(path=>[resolve(dirname(path),'../bin/bash.exe'),resolve(dirname(path),'../../bin/bash.exe')]);
    const bash=candidates.find(existsSync);assert(bash);
    const convert=path=>{const result=spawnSync(bash,['--noprofile','--norc','-c','cygpath -au -- "$1"','test',path],{encoding:'utf8'});assert.equal(result.status,0,result.stderr);return result.stdout.trim();};
    const args=['--prefix',convert(f.prefix),'--uninstaller',convert(tool),'--config-home',convert(f.config),'--state-home',convert(f.state),'--cache-home',convert(f.cache),'--no-path','--yes','--purge'];
    const result=spawnSync(bash,['--noprofile','--norc',join(root,'scripts/uninstall.sh').replaceAll('\\','/'),...args],{cwd:f.directory,encoding:'utf8',timeout:60_000});
    assert.equal(result.status,0,result.stderr);assert(!existsSync(f.core));
    for(const path of [f.config,f.state,f.cache])assert(result.stdout.includes(path),result.stdout);
  }finally{await clean(f);}
});


test('corrupt installed tool record explains external recovery without a JSON stack', {skip:!windows}, async()=>{
  const f=await fixture();try{
    await writeFile(join(f.control,'install.json'),'broken');
    const result=script('uninstall',[...f.args,flag('Yes')],f.directory);
    assert.notEqual(result.status,0);
    assert.match(result.stderr,/trusted external -Uninstaller FILE and -Force/);
    assert.doesNotMatch(result.stderr,/Invalid JSON|CategoryInfo|FullyQualifiedErrorId/);
    assert(existsSync(f.core));
  }finally{await clean(f);}
});


test('purge without noninteractive consent refuses before mutation',async()=>{
  const f=await fixture();try{
    await mkdir(join(f.state,'agent'));await writeFile(join(f.state,'agent/data'),'keep');
    const result=script('uninstall',[...f.args,flag('Purge')],f.directory);
    assert.notEqual(result.status,0);assert.match(result.stderr,/requires --yes/);
    assert(existsSync(f.core));assert(!existsSync(join(f.state,'session-host')));
    assert.equal(await readFile(join(f.state,'agent/data'),'utf8'),'keep');
  }finally{await clean(f);}
});

test('POSIX PATH removal preserves user edits, shared bins and linked startup files',{skip:windows},async()=>{
  for(const mode of ['exact','shared','edited','linked','readonly']) {
    const f=await fixture();try{
      const home=join(f.directory,'shell-home');await mkdir(home);
      const recordPath=join(f.control,'install.json'), record=JSON.parse(await readFile(recordPath,'utf8'));
      const escaped=join(f.prefix,'bin').replaceAll("'", "'\\''");
      const insertion=`\n# rsi-install:${record.id}\nexport PATH='${escaped}':"$PATH"\n# rsi-install:${record.id}:end\n`;
      const rc=join(home,'.profile'), outside=join(f.directory,'external-profile');
      const content='authored\n'+insertion;
      await writeFile(rc,mode==='edited'?content.replace('export PATH','export  PATH'):content);
      if(mode==='shared')await writeFile(join(f.prefix,'bin/other'),'keep');
      if(mode==='linked'){await writeFile(outside,content);await rm(rc);await symlink(outside,rc);}
      if(mode==='readonly')await chmod(rc,0o444);
      record.path_edits=[{target:'.profile',insertion,applied:true}];
      await writeFile(recordPath,JSON.stringify(record));
      const args=f.args.filter(arg=>arg!==flag('NoPath'));
      const result=spawnSync('bash',[join(root,'scripts/uninstall.sh'),...args,'--yes'],{cwd:f.directory,env:{...process.env,HOME:home},encoding:'utf8',timeout:60_000});
      assert.equal(result.status,0,result.stderr);assert(!existsSync(f.core));
      assert.equal(await readFile(rc,'utf8'),mode==='exact'?'authored\n':mode==='edited'?content.replace('export PATH','export  PATH'):content);
      if(mode==='linked')assert.equal(await readFile(outside,'utf8'),content);
      if(mode==='readonly')await chmod(rc,0o644);
    }finally{await clean(f);}
  }
});

test('protocol probe creates nothing and native uninstall rejects install-only arguments',async()=>{
  const directory=await mkdtemp(join(temporaryRoot,'rsi-probe-'));try{
    const result=spawnSync(tool,['--installation-protocol'],{cwd:directory,encoding:'utf8'});
    assert.equal(result.status,0,result.stderr);
    assert.deepEqual(JSON.parse(result.stdout),{interface:1,record:1,transaction:2});
    assert.deepEqual(await readdir(directory),[]);
    const bad=spawnSync(tool,['uninstall','--interface-version','1','--uninstaller',tool],{cwd:directory,encoding:'utf8'});
    assert.notEqual(bad.status,0);assert.match(bad.stderr,/install sources/);
  }finally{await rm(directory,{recursive:true,force:true});}
});


test('Windows removal accepts an existing prefix with alternate case',{skip:!windows},async()=>{
  const f=await fixture();try{
    const args=[...f.args];args[1]=f.prefix.toLowerCase();
    const result=script('uninstall',[...args,flag('Yes')],f.directory);
    assert.equal(result.status,0,result.stderr);assert(!existsSync(f.core));
  }finally{await clean(f);}
});
