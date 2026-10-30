import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdtemp, mkdir, writeFile, readFile, rm, access } from "node:fs/promises";
import cp, { spawnSync } from "node:child_process";
import { syncBuiltinESMExports } from "node:module";
import { createHash } from "node:crypto";
import { join } from "node:path";
import { tmpdir } from "node:os";
import {pathToFileURL} from 'node:url';
import { startService } from "./service.mjs";
import { EventEmitter } from 'node:events';
import { PassThrough } from 'node:stream';

function certificateOrPrivateDirectory(program,args,options) {
  if(program==='powershell.exe'){
    assert.equal(process.platform,'win32');
    assert.deepEqual(args.slice(0,3),['-NoProfile','-NonInteractive','-Command']);
    assert.match(args[3], /DirectorySecurity/);
    assert.match(args[3], /SetAccessControl/);
    assert.match(options.env.RSI_FIXTURE_DIRECTORY, /config[\\/]rsi$/);
  }else assert.equal(program,'openssl');
  return {status:0,stdout:'',stderr:''};
}

test("embedded browser owns its Service and invokes device Profiles directly", async t => {
  const report = await mkdtemp(join(tmpdir(), "rsi-embedded-adapter-"));
  let service;
  const calls = [];
  try {
    t.mock.method(cp, 'spawnSync', (program, args, options) => {
      if (program !== 'embedder') return certificateOrPrivateDirectory(program, args, options);
      calls.push(args);
      assert.deepEqual(args.slice(0, 2), ['--profile', 'devices']);
      return {status:0, stdout:JSON.stringify(args[2] === 'register' ? {id:'device'} : {revision:'1'}), stderr:''};
    });
    t.mock.method(cp, 'spawn', (program, args) => {
      assert.equal(program, 'embedder');
      assert.deepEqual(args.slice(0, 2), ['--profile', 'browser-fixture']);
      assert(!args.includes('--assets'));
      const child = Object.assign(new EventEmitter(), {stdout:new PassThrough(), stderr:new PassThrough(),exitCode:null});
      child.kill = () => { child.exitCode=0; child.emit('exit',0,null); };
      setImmediate(()=>child.stdout.write('{"event":"serving"}\n'));
      return child;
    });
    syncBuiltinESMExports();
    service = await startService({binary:'embedder', assets:report, report, embedded:true,
      async configure({config}) {
        const profile = await readFile(join(config, 'application-profiles/browser-fixture/application.profile.toml'), 'utf8');
        assert.match(profile, /plugin = "rsi.web.assets"/);
        assert.match(profile, /plugin = "fixture.browser.serve"/);
      }});
    assert.deepEqual(service.register('embedded'), {id:'device'});
    await service.close(); service = undefined;
    assert.deepEqual(calls.map(args=>args.slice(2)), [
      ['register','embedded'], ['configuration','list'], ['configuration','grant','device','1'],
    ]);
  } finally {
    try { await service?.close(); }
    finally { t.mock.restoreAll(); syncBuiltinESMExports(); await rm(report,{recursive:true,force:true}); }
  }
});

test("renderer watch uses ordinary Web launch and a disjoint relative package recipe", async t => {
  const directory = await mkdtemp(join(tmpdir(), "rsi-renderer-recipe-"));
  let service;
  try {
    const assets = join(directory, 'assets');
    await mkdir(assets);
    t.mock.method(cp, 'spawnSync', () => ({status:0, stdout:'stopped\n', stderr:''}));
    t.mock.method(cp, 'spawn', (_program, args) => {
      assert.deepEqual(args, ['--watch','web','--port','0','--no-open']);
      const child = Object.assign(new EventEmitter(), {stdout:new PassThrough(), stderr:new PassThrough(),exitCode:null});
      child.kill = () => { child.exitCode=0; child.emit('exit',0,null); };
      setImmediate(()=>child.stdout.write('rsi web: http://127.0.0.1:43210#rsi-launch=fixture\n'));
      return child;
    });
    syncBuiltinESMExports();
    let config;
    service = await startService({binary:'fixture', assets, report:directory, watchAssets:true,
      configure(value) { config=value.config; }});
    const document = await readFile(join(config,'rsi.toml'),'utf8');
    assert.match(document, /\[sources\.web\]/);
    assert.match(document, /inputs = \["assets\/standard.js","assets\/file-preview.js","assets\/ui-renderers.json"\]/);
    assert.match(document, /output = "web-package\/package.json"/);
    assert.equal(service.origin,'http://127.0.0.1:43210');
    assert.equal(service.developmentHttp,true);
    assert.equal(new URL(service.launchUrl).hash,'#rsi-launch=fixture');
  } finally {
    try { await service?.close(); }
    finally {
      t.mock.restoreAll(); syncBuiltinESMExports();
      await rm(directory,{recursive:true,force:true});
    }
  }
});

test("recovery and tasks retain leased binary identity without copying it when browser startup fails", async () => {
  const directory = await mkdtemp(join(tmpdir(), "rsi-binary-evidence-"));
  try {
    const binary = join(directory, "fixture");
    const bytes = "unused executable\n";
    await writeFile(binary, bytes, { mode: 0o700 });
    const family = 'a'.repeat(64);
    const name=process.platform==='win32'?'rsi.exe':'rsi';
    await writeFile(join(directory, 'receipt.json'), JSON.stringify({format:2,kind:'web',family_sha256:family,artifacts:{[name]:createHash('sha256').update(bytes).digest('hex')}}));
    const preload=join(directory,'lease-stub.mjs');
    await writeFile(preload,`import cp from 'node:child_process';import {syncBuiltinESMExports} from 'node:module';
      const launch=cp.spawn;cp.spawn=(binary,args,options)=>launch(binary,args[0]==='fixture-lease'?['-e',"process.stdout.write('ready\\\\n');process.stdin.resume();"]:args,options);syncBuiltinESMExports();`);
    await writeFile(join(directory, 'rsi-build.json'), JSON.stringify({format:1,family_sha256:family}));
    for (const probe of ["recovery", "tasks"]) {
      const report = join(directory, probe);
      const result = spawnSync(process.execPath, ['--import',pathToFileURL(preload).href,join(import.meta.dirname, `${probe}.mjs`)], {
        env: { ...process.env, NODE_OPTIONS:'', RSI_PAIRED_BUNDLE:'', RSI_APP_TOOLS_BINARY:process.execPath, RSI_WEB_BINARY: binary, RSI_WEB_ASSETS: directory,
          RSI_WEB_REPORT: report, RSI_WEB_BROWSER: "chromium", PLAYWRIGHT_BROWSERS_PATH: join(directory, "absent-browsers") },
        encoding: "utf8", timeout: 15_000,
      });
      assert.equal(result.status, 1, result.error?.message ?? result.stderr);
      assert.match(result.stderr, /Executable doesn't exist/, "fixture reaches the forced browser-startup failure");
      assert.deepEqual(JSON.parse(await readFile(join(report, "binary.json"), "utf8")), {
        sha256: createHash("sha256").update(bytes).digest("hex"),
      });
      assert.equal(await readFile(binary, "utf8"), bytes);
      await assert.rejects(access(join(report, "rsi")), {code:"ENOENT"});
    }
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test("startup preserves its bad readiness reply when child cleanup also fails", async t => {
  const directory = await mkdtemp(join(tmpdir(), "rsi-service-failure-"));
  const launch = cp.spawn;
  try {
    const binary = join(directory, "fixture");
    t.mock.method(cp, 'spawnSync', (program, args, options) => {
      if (program === binary) {
        assert.deepEqual(args, ['host', 'status']);
        return {status:0, stdout:'stopped\n', stderr:''};
      }
      return certificateOrPrivateDirectory(program,args,options);
    });
    t.mock.method(cp, 'spawn', (program, args, options) => {
      assert.equal(program, binary);
      const child=launch(process.execPath, ['-e', "process.stdout.write('invalid-readiness\\n');setInterval(()=>{},1000);"], options);
      const kill=child.kill.bind(child);child.kill=()=>kill('SIGKILL');return child;
    });
    syncBuiltinESMExports();
    await assert.rejects(startService({ binary, assets: directory, report: directory }), error => {
      assert(error instanceof AggregateError);
      assert(error.cause instanceof SyntaxError, "original invalid readiness reply must remain inspectable");
      assert.equal(error.errors[0], error.cause);
      assert(error.errors[1] instanceof AggregateError);
      assert.match(String(error.errors[1].errors[0]), /Service cleanup failed.*SIGKILL/);
      return true;
    });
  } finally {
    t.mock.restoreAll();
    syncBuiltinESMExports();
    await rm(directory, { recursive: true, force: true });
  }
});

test("failed admission before Host creation preserves its primary error without stopping an absent owner", async t => {
  const directory = await mkdtemp(join(tmpdir(), "rsi-service-admission-"));
  const launch = cp.spawn;
  const commands = [];
  try {
    const binary = join(directory, "fixture");
    t.mock.method(cp, 'spawnSync', (program, args, options) => {
      if (program === binary) {
        commands.push(args);
        assert.deepEqual(args, ['host', 'status']);
        return {status:0, stdout:'stopped\n', stderr:''};
      }
      return certificateOrPrivateDirectory(program,args,options);
    });
    t.mock.method(cp, 'spawn', (program, args, options) => {
      assert.equal(program, binary);
      return launch(process.execPath, ['-e', "process.stderr.write('fixture admission rejected');process.exitCode=1;"], options);
    });
    syncBuiltinESMExports();
    await assert.rejects(startService({binary, assets:directory, report:directory}), error => {
      assert(!(error instanceof AggregateError), "absent Host must not create a cleanup failure");
      assert.match(error.message, /Service exited before readiness: fixture admission rejected/);
      return true;
    });
    assert.deepEqual(commands, [['host', 'status']]);
  } finally {
    t.mock.restoreAll();
    syncBuiltinESMExports();
    await rm(directory, {recursive:true, force:true});
  }
});

test('Windows forced termination records its outcome without masking a bad readiness reply', {skip:process.platform!=='win32'}, async t=>{
  const directory=await mkdtemp(join(tmpdir(),'rsi-windows-cleanup-')),launch=cp.spawn;
  try{
    t.mock.method(cp,'spawnSync',(program,args,options)=>program==='fixture'?{status:0,stdout:'stopped\n',stderr:''}:certificateOrPrivateDirectory(program,args,options));
    t.mock.method(cp,'spawn',(_program,_args,options)=>launch(process.execPath,['-e',"process.stdout.write('invalid-readiness\\n');setInterval(()=>{},1000)"],options));
    syncBuiltinESMExports();
    await assert.rejects(startService({binary:'fixture',assets:directory,report:directory}),SyntaxError);
    const outcome=JSON.parse(await readFile(join(directory,'service-process-shutdown.json'),'utf8'));
    assert.equal(outcome.mode,'windows-terminate-process');assert.equal(outcome.signal,'SIGTERM');
  }finally{t.mock.restoreAll();syncBuiltinESMExports();await rm(directory,{recursive:true,force:true})}
});


test('manual receipt connection enables only explicitly selected development HTTP', async () => {
  const {connectWorkbench} = await import('./browser-fixture.mjs');
  for (const developmentHttp of [false, true]) {
    const actions = [];
    const page = {
      goto: async origin => actions.push(['goto',origin]),
      locator: selector => ({fill: async value => actions.push(['fill',selector,value]), waitFor: async () => actions.push(['visible',selector])}),
      getByLabel: label => ({check: async () => actions.push(['check',label])}),
      getByRole: (_role, {name}) => ({click: async () => actions.push(['click',name])}),
    };
    await connectWorkbench(page, {developmentHttp, origin:'fixture', register: () => ({id:'device'})}, 'fixture');
    assert.deepEqual(actions.map(row=>row[0]), developmentHttp ? ['goto','fill','check','click','visible'] : ['goto','fill','click','visible']);
  }
});

test('live Fact admission stops metadata scanning before materializing oversized history', async () => {
  const {DatabaseSync} = await import('node:sqlite');
  const {readFactEvidence} = await import('./fact-evidence.mjs');
  const database = new DatabaseSync(':memory:');
  let visited = 0;
  database.function('observe', value => { visited++; return value; });
  database.exec('CREATE TABLE durable(session_id TEXT, seq INTEGER, body TEXT, PRIMARY KEY(session_id,seq)); CREATE VIEW facts AS SELECT session_id,seq,observe(body) AS fact_json FROM durable;');
  const insert = database.prepare('INSERT INTO durable VALUES(?,?,?)');
  try {
    insert.run('accepted',2,JSON.stringify({seq:2,text:'中文'})); insert.run('accepted',1,JSON.stringify({seq:1}));
    const {facts,selection} = readFactEvidence(database,'accepted');
    assert.deepEqual(facts.map(fact=>fact.seq),[1,2]);
    assert.equal(selection.bytes,Buffer.byteLength(JSON.stringify(facts[0]))+Buffer.byteLength(JSON.stringify(facts[1])));
    for(let seq=1;seq<=1000;seq++)insert.run('too-many',seq,'not JSON');
    visited=0;
    assert.throws(()=>readFactEvidence(database,'too-many'),/bounded Fact selection/);
    assert.equal(visited,513,'row bound rejects before JSON parsing or visiting the remaining history');
    insert.run('too-large',1,'x'.repeat(16*1024*1024+1)); insert.run('too-large',2,'not JSON');
    visited=0;
    assert.throws(()=>readFactEvidence(database,'too-large'),/bounded Fact selection/);
    assert.equal(visited,1,'byte overflow stops before the next metadata row');
    assert.throws(()=>readFactEvidence(database,'missing'),/no Facts/);
  } finally {database.close();}
});
