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

export async function assertControls(page, root, labels) {
  assert(labels.length > 0, "task capture needs explicit expected controls");
  const controls = [];
  for (const label of labels) {
    const button = root.getByRole("button", { name: label, exact: true });
    assert.equal(await button.count(), 1, `${label}: expected one reachable control`);
    await button.click({ trial: true });
    assert(await button.isVisible(), `${label}: hidden`);
    assert(await button.isEnabled(), `${label}: disabled`);
    const hit = await button.evaluate(button => {
      const box = button.getBoundingClientRect();
      return box.width > 0 && box.height > 0 && button.contains(document.elementFromPoint(box.x + box.width / 2, box.y + box.height / 2));
    });
    assert(hit, `${label}: obscured or clipped`);
    controls.push({ label, hit });
  }
  return controls;
}

export async function assertNoNotices(page) {
  const notices = await page.locator("#notice, .pane-notice").allTextContents();
  const goal = await page.locator("#detail").allTextContents();
  assert(!goal.some(text => /Goal control rejected|Goal control:|command revision conflict|Control outcome is unresolved/.test(text)), `Unexpected Goal feedback: ${JSON.stringify(goal)}`);
  assert(notices.every(text => !text.trim()), `Unexpected product notice: ${JSON.stringify(notices)}`);
}
