import assert from "node:assert/strict";
import { writeFile } from "node:fs/promises";
import { join } from "node:path";

export async function recordTaskFailure(page, directory, evidence) {
  const capture_errors = [];
  if (page) {
    await page.screenshot({ path: join(directory, "failure.png"), fullPage: true }).catch(error => capture_errors.push(`screenshot: ${error}`));
    try { await writeFile(join(directory, "failure.html"), await page.content()); }
    catch (error) { capture_errors.push(`HTML: ${error}`); }
  }
  await writeFile(join(directory, "failure.json"), JSON.stringify({ ...evidence, capture_errors }, null, 2));
}

export function browserNames(value) {
  assert(!value || ["chromium", "firefox"].includes(value), `Unsupported RSI_WEB_BROWSER: ${value}`);
  return value ? [value] : ["chromium", "firefox"];
}

export async function assertControls(page, rootSelector, labels) {
  assert(labels.length > 0, "task capture needs explicit expected controls");
  const controls = [];
  for (const label of labels) {
    const button = page.locator(rootSelector).getByRole("button", { name: label, exact: true });
    assert.equal(await button.count(), 1, `${label}: expected one reachable control`);
    await button.click({ trial: true });
    assert(await button.isVisible(), `${label}: hidden`);
    assert(await button.isEnabled(), `${label}: disabled`);
    const sample = await page.evaluate(({ rootSelector, label }) => {
      const buttons = [...document.querySelectorAll(rootSelector)].flatMap(root => [...root.querySelectorAll("button")])
        .filter(button => (button.getAttribute("aria-label") ?? button.textContent).trim() === label);
      if (buttons.length !== 1) return { count: buttons.length };
      const [button] = buttons;
      const box = button.getBoundingClientRect();
      return { count: 1, enabled: !button.matches(":disabled"), box: box.toJSON(),
        hit: box.width > 0 && box.height > 0 && button.contains(document.elementFromPoint(box.x + box.width / 2, box.y + box.height / 2)) };
    }, { rootSelector, label });
    assert.equal(sample.count, 1, `${label}: expected one current control: ${JSON.stringify(sample)}`);
    assert(sample.enabled, `${label}: disabled during capture`);
    assert(sample.hit, `${label}: obscured or clipped: ${JSON.stringify(sample)}`);
    controls.push({ label, hit: sample.hit });
  }
  return controls;
}

export async function assertNoNotices(page) {
  const notices = await page.locator("#notice, .pane-notice").allTextContents();
  const goal = await page.locator("#detail").allTextContents();
  assert(!goal.some(text => /Goal control rejected|Goal control:|command revision conflict|Control outcome is unresolved/.test(text)), `Unexpected Goal feedback: ${JSON.stringify(goal)}`);
  assert(notices.every(text => !text.trim()), `Unexpected product notice: ${JSON.stringify(notices)}`);
}
