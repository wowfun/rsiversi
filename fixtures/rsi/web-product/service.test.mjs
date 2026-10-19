import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdtemp, writeFile, readFile, rm, access } from "node:fs/promises";
import cp, { spawnSync } from "node:child_process";
import { syncBuiltinESMExports } from "node:module";
import { createHash } from "node:crypto";
import { join } from "node:path";
import { tmpdir } from "node:os";
import {pathToFileURL} from 'node:url';
import { startService } from "./service.mjs";

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
    t.mock.method(cp, 'spawnSync', program => {
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
