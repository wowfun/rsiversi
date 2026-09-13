import assert from "node:assert/strict";
import { test } from "node:test";
import { chromium, firefox } from "playwright";
import { browserNames, assertControls, assertNoNotices, recordTaskFailure } from "./task-checks.mjs";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";

for (const [name, engine] of [["chromium", chromium], ["firefox", firefox]]) {
  test(`${name}: control capture measures the current standard renderer after replacement`, async () => {
    const browser = await engine.launch({ headless: true });
    try {
      const page = await browser.newPage();
      await page.setContent("<main></main>");
      await page.evaluate(async source => {
        const url = URL.createObjectURL(new Blob([source], { type: "text/javascript" }));
        const { mount } = await import(url);
        URL.revokeObjectURL(url);
        let revision = 0;
        window.invocations = 0;
        const snapshot = () => ({ model: { standard_view: { elements: [{ kind: "button", label: "Pause after current round", action: "goal", value: ++revision }] } }, busy: false });
        const renderer = await mount(document.querySelector("main"), snapshot(), { invoke() { ++window.invocations; } }, new AbortController().signal);
        window.replaceControl = () => renderer.update(snapshot());
      }, await readFile(new URL("../../../plugins/rsi/web/standard.js", import.meta.url), "utf8"));
      let replacements = 0;
      const replace = async () => {
        ++replacements;
        await page.evaluate(() => window.replaceControl());
      };
      // Preserve the real lookup/evaluation boundary: a locator can hand its
      // callback an element that the renderer retired after it was resolved.
      const wrapLocator = locator => new Proxy(locator, { get(target, property) {
        if (property === "getByRole") return (...args) => wrapLocator(target.getByRole(...args));
        if (property === "evaluate") return async (callback, arg) => {
          const element = await target.elementHandle();
          try { await replace(); return await element.evaluate(callback, arg); }
          finally { await element.dispose(); }
        };
        const value = Reflect.get(target, property);
        return typeof value === "function" ? value.bind(target) : value;
      } });
      const observedPage = new Proxy(page, { get(target, property) {
        if (property === "locator") return (...args) => wrapLocator(target.locator(...args));
        if (property === "evaluate") return async (...args) => { await replace(); return target.evaluate(...args); };
        const value = Reflect.get(target, property);
        return typeof value === "function" ? value.bind(target) : value;
      } });
      assert.deepEqual(await assertControls(observedPage, "main", ["Pause after current round"]), [{ label: "Pause after current round", hit: true }]);
      assert.equal(replacements, 1);
      assert.equal(await page.evaluate(() => window.invocations), 0, "capture must not invoke or replay a mutation");
    } finally { await browser.close(); }
  });
}

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
      await assert.rejects(() => assertControls(page, "main", ["Complete result"]));
    }
    await page.setContent('<main><div style="height:1500px"></div><button>Complete result</button></main><div id="notice"></div><div class="pane-notice"></div>');
    assert.deepEqual(await assertControls(page, "main", ["Complete result"]), [{ label: "Complete result", hit: true }]);
    await assertNoNotices(page);
    await page.locator("body").evaluate(node => node.insertAdjacentHTML("beforeend", '<div id="detail"></div>'));
    for (const feedback of ["Goal control rejected. Request old: command revision conflict", "Control outcome is unresolved. Request original."]) {
      await page.locator("#detail").evaluate((node, text) => { node.textContent = text; }, feedback);
      await assert.rejects(() => assertNoNotices(page), /Unexpected Goal feedback/);
    }
    await page.locator("#detail").evaluate(node => { node.textContent = ""; });
    for (const selector of ["#notice", ".pane-notice"]) {
      await page.locator(selector).evaluate(node => { node.textContent = "Failed to open result"; });
      await assert.rejects(() => assertNoNotices(page), /Unexpected product notice/);
      await page.locator(selector).evaluate(node => { node.textContent = ""; });
    }
  } finally { await browser.close(); }
});
