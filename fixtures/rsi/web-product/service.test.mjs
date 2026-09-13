import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdtemp, writeFile, readFile, rm } from "node:fs/promises";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { startService } from "./service.mjs";

test("recovery and tasks retain frozen binary identity when browser startup fails", async () => {
  const directory = await mkdtemp(join(tmpdir(), "rsi-binary-evidence-"));
  try {
    const binary = join(directory, "fixture");
    const bytes = "unused executable\n";
    await writeFile(binary, bytes, { mode: 0o700 });
    for (const probe of ["recovery", "tasks"]) {
      const report = join(directory, probe);
      const result = spawnSync(process.execPath, [join(import.meta.dirname, `${probe}.mjs`)], {
        env: { ...process.env, RSI_WEB_BINARY: binary, RSI_WEB_ASSETS: directory,
          RSI_WEB_REPORT: report, RSI_WEB_BROWSER: "chromium", PLAYWRIGHT_BROWSERS_PATH: join(directory, "absent-browsers") },
        encoding: "utf8", timeout: 15_000,
      });
      assert.equal(result.status, 1, result.error?.message ?? result.stderr);
      assert.match(result.stderr, /Executable doesn't exist/, "fixture reaches the forced browser-startup failure");
      assert.deepEqual(JSON.parse(await readFile(join(report, "binary.json"), "utf8")), {
        sha256: createHash("sha256").update(bytes).digest("hex"),
      });
      assert.equal(await readFile(join(report, "rsi"), "utf8"), bytes);
    }
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test("startup preserves its bad readiness reply when child cleanup also fails", async () => {
  const directory = await mkdtemp(join(tmpdir(), "rsi-service-failure-"));
  try {
    const binary = join(directory, "fixture");
    await writeFile(binary, '#!/bin/sh\nprintf "%s\\n" invalid-readiness\nexec sleep 60\n', { mode: 0o700 });
    await assert.rejects(startService({ binary, assets: directory, report: directory }), error => {
      assert(error instanceof AggregateError);
      assert(error.cause instanceof SyntaxError, "original invalid readiness reply must remain inspectable");
      assert.equal(error.errors[0], error.cause);
      assert.match(String(error.errors[1]), /Service cleanup failed.*SIGTERM/);
      return true;
    });
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
