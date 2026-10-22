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
    assert.match(document, /inputs = \["assets\/standard.js","assets\/lazy.js","assets\/ui-renderers.json"\]/);
    assert.match(document, /output = "web-package\/package.json"/);
    assert.equal(service.origin,'http://127.0.0.1:43210');
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
        env: { ...process.env, NODE_OPTIONS:'', RSI_APP_TOOLS_BINARY:process.execPath, RSI_WEB_BINARY: binary, RSI_WEB_ASSETS: directory,
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
    t.mock.method(cp, 'spawnSync', (program, args) => {
      if (program === binary) {
        assert.deepEqual(args, ['host', 'status']);
        return {status:0, stdout:'stopped\n', stderr:''};
      }
      assert.equal(program, 'openssl');
      return {status:0, stdout:'', stderr:''};
    });
    t.mock.method(cp, 'spawn', (program, args, options) => {
      assert.equal(program, binary);
      return launch(process.execPath, ['-e', "process.stdout.write('invalid-readiness\\n');setInterval(()=>{},1000);"], options);
    });
    syncBuiltinESMExports();
    await assert.rejects(startService({ binary, assets: directory, report: directory }), error => {
      assert(error instanceof AggregateError);
      assert(error.cause instanceof SyntaxError, "original invalid readiness reply must remain inspectable");
      assert.equal(error.errors[0], error.cause);
      assert(error.errors[1] instanceof AggregateError);
      assert.match(String(error.errors[1].errors[0]), /Service cleanup failed.*SIGTERM/);
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
    t.mock.method(cp, 'spawnSync', (program, args) => {
      if (program === binary) {
        commands.push(args);
        assert.deepEqual(args, ['host', 'status']);
        return {status:0, stdout:'stopped\n', stderr:''};
      }
      assert.equal(program, 'openssl');
      return {status:0, stdout:'', stderr:''};
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
