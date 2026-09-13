import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { startService } from "./service.mjs";

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
