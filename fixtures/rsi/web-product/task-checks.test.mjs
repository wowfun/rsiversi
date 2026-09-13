import assert from "node:assert/strict";
import { test } from "node:test";
import { chromium } from "playwright";
import { browserNames, assertControls, assertNoNotices, recordTaskFailure } from "./task-checks.mjs";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";

test("renderer capture failures retain the original task failure metadata", async () => {
  const directory = await mkdtemp(join(tmpdir(), "rsi-task-failure-"));
  try {
    const page = { async screenshot() { throw new Error("renderer exited during screenshot"); },
      async content() { throw new Error("renderer exited during HTML capture"); } };
    await recordTaskFailure(page, directory, { error: "original task assertion", measurements: [{ label: "last completed phase" }] });
    const saved = JSON.parse(await readFile(join(directory, "failure.json"), "utf8"));
    assert.equal(saved.error, "original task assertion");
    assert.equal(saved.measurements[0].label, "last completed phase");
    assert.equal(saved.capture_errors.length, 2);
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test("engine selection cannot produce an empty task run", () => {
  assert.throws(() => browserNames("webkit"), /Unsupported RSI_WEB_BROWSER/);
  assert.deepEqual(browserNames(undefined), ["chromium", "firefox"]);
  assert.deepEqual(browserNames("firefox"), ["firefox"]);
});

test("task assertions reject missing, hidden, disabled and obscured controls and product notices", async () => {
  const browser = await chromium.launch({ headless: true });
  try {
    const page = await browser.newPage();
    page.setDefaultTimeout(1000);
    for (const markup of ["", '<button style="display:none">Complete result</button>',
      '<button disabled>Complete result</button>', '<button>Complete result</button><div style="position:fixed;inset:0"></div>']) {
      await page.setContent(`<main>${markup}</main>`);
      await assert.rejects(() => assertControls(page, page.locator("main"), ["Complete result"]));
    }
    await page.setContent('<main><div style="height:1500px"></div><button>Complete result</button></main><div id="notice"></div><div class="pane-notice"></div>');
    assert.deepEqual(await assertControls(page, page.locator("main"), ["Complete result"]), [{ label: "Complete result", hit: true }]);
    await assertNoNotices(page);
    for (const selector of ["#notice", ".pane-notice"]) {
      await page.locator(selector).evaluate(node => { node.textContent = "Failed to open result"; });
      await assert.rejects(() => assertNoNotices(page), /Unexpected product notice/);
      await page.locator(selector).evaluate(node => { node.textContent = ""; });
    }
  } finally { await browser.close(); }
});
