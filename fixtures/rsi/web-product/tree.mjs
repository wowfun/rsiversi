import assert from "node:assert/strict";
import { join } from "node:path";

export async function verifyTree(page, pane, service, report, browser) {
  const identities = await page.locator(".pane-session").allTextContents();
  await pane.getByRole("textbox", { name: "Right message" }).fill("Please inspect a child task");
  await pane.getByRole("button", { name: "Send ↗" }).click();
  await pane.locator(".message.assistant").filter({ hasText: "Reviewed: Subagent activation completed." }).waitFor();
  await pane.locator(".pane-status").filter({ hasText: "Completed" }).waitFor();
  await pane.getByRole("button", { name: "Agent tree", exact: true }).click();
  const detail = page.getByRole("dialog");
  await detail.getByRole("button", { name: "Inspect agent tree", exact: true }).click();
  await detail.getByRole("button", { name: "Inspect inspect-child", exact: true }).waitFor();
  await page.screenshot({ path: join(report, `${browser}-agent-tree.png`) });
  await detail.getByRole("button", { name: "Inspect inspect-child", exact: true }).click();
  await detail.getByRole("button", { name: "Agent: inspect-child", exact: true }).waitFor();
  await detail.getByRole("button", { name: "Read conversation", exact: true }).click();
  await detail.getByText("Read-only history.", { exact: false }).waitFor();
  await detail.getByText(/Fact \d+ · Input: Child inspector evidence/).waitFor();
  assert.match(await detail.innerText(), /Through Fact \d+/);
  assert.equal(await page.locator(".transcript").count(), 2);
  assert.deepEqual(await page.locator(".pane-session").allTextContents(), identities);
  assert.ok(service.provider.requests.some(request => request.prompt === "Child inspector evidence"));
  await page.screenshot({ path: join(report, `${browser}-child-history.png`) });
  await detail.getByRole("button", { name: "Root agent", exact: true }).click();
  await detail.getByRole("button", { name: "Inspect inspect-child", exact: true }).waitFor();
  await page.getByRole("button", { name: "Close details", exact: true }).click();
  await detail.waitFor({ state: "hidden" });
}
