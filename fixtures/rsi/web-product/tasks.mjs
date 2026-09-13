import assert from "node:assert/strict";
import { mkdir, writeFile, readFile, copyFile, chmod } from "node:fs/promises";
import { join, resolve } from "node:path";
import { createHash } from "node:crypto";
import { chromium, firefox } from "playwright";
import { startService } from "./service.mjs";
import { browserNames, assertControls, assertNoNotices, recordTaskFailure } from "./task-checks.mjs";

const names = browserNames(process.env.RSI_WEB_BROWSER);
const report = process.env.RSI_WEB_REPORT;
const assets = process.env.RSI_WEB_ASSETS;
if (!report || !assets) throw new Error("Set RSI_WEB_REPORT and RSI_WEB_ASSETS");
await mkdir(report, { recursive: false });
const binary = join(report, "rsi");
await copyFile(process.env.RSI_WEB_BINARY ?? resolve("target/debug/rsi"), binary);
await chmod(binary, 0o700);
const binaryHash = createHash("sha256").update(await readFile(binary)).digest("hex");

async function until(predicate, label, maximum = 30000) {
  const deadline = Date.now() + maximum;
  while (!await predicate()) {
    if (Date.now() >= deadline) throw new Error(`${label} timeout`);
    await new Promise(resolve => setTimeout(resolve, 50));
  }
}
async function geometry(page, pane, detail) {
  return page.evaluate(({ detail, pane }) => {
    const root = document.querySelector(detail ? "#detail" : pane);
    const rect = root.getBoundingClientRect();
    const buttons = [...root.querySelectorAll("button")].filter(button => {
      const box = button.getBoundingClientRect();
      if (button.disabled || box.width <= 0 || box.height <= 0 || box.top < 0 || box.bottom > innerHeight) return false;
      for (let parent = button.parentElement; parent; parent = parent.parentElement) {
        const style = getComputedStyle(parent), clip = parent.getBoundingClientRect();
        if (/(auto|scroll|hidden|clip)/.test(style.overflowY) && (box.top < clip.top || box.bottom > clip.bottom)) return false;
        if (/(auto|scroll|hidden|clip)/.test(style.overflowX) && (box.left < clip.left || box.right > clip.right)) return false;
      }
      return true;
    });
    const hit = button => {
      const box = button.getBoundingClientRect();
      return button.contains(document.elementFromPoint(box.x + box.width / 2, box.y + box.height / 2));
    };
    const send = [...root.querySelectorAll("button")].find(button => button.textContent === "Send ↗");
    return { viewport: [innerWidth, innerHeight], page_width: document.documentElement.scrollWidth,
      root: { left: rect.left, right: rect.right, width: rect.width },
      controls: buttons.map(button => ({ label: button.textContent, hit: hit(button) })),
      send_hit: detail ? null : !!send && hit(send) };
  }, { detail, pane });
}

const results = [];
for (const [name, engine] of [["chromium", chromium], ["firefox", firefox]]) {
  if (!names.includes(name)) continue;
  const directory = join(report, name);
  await mkdir(directory);
  const browser = await engine.launch({ headless: true });
  let service, page;
  const errors = [], measurements = [];
  try {
    service = await startService({ binary, assets, report: directory });
    await writeFile(join(service.workspace, "card.txt"), "before\n");
    const context = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1440, height: 980 } });
    await context.addInitScript(() => {
      const identities = new WeakMap(); let sequence = 0;
      window.taskInputEvents = [];
      window.taskVisibleRequests = [];
      window.taskInvocations = [];
      window.taskGoalErrors = [];
      const observed = new WeakSet();
      const boundedPush = (list, item) => { list.push(item); if (list.length > 64) list.shift(); };
      const observeGoalError = (text, source) => {
        if (!/Goal control rejected|Goal control:|command revision conflict|UI action or surface has retired|Control outcome is unresolved/.test(text)) return;
        const diagnostic = text.slice(0, 4096);
        if (window.taskGoalErrors.at(-1)?.diagnostic !== diagnostic) boundedPush(window.taskGoalErrors, { time: performance.now(), source, diagnostic });
      };
      document.addEventListener("DOMContentLoaded", () => {
        new MutationObserver(() => observeGoalError(document.querySelector("#detail")?.textContent ?? "", "DOM"))
          .observe(document.body, { subtree: true, childList: true, characterData: true });
      });
      const post = Worker.prototype.postMessage;
      Worker.prototype.postMessage = function(message, ...rest) {
        if (!observed.has(this)) {
          observed.add(this);
          this.addEventListener("message", ({ data }) => {
            if (data?.kind === "reply") {
              const invocation = window.taskInvocations.find(item => item.id === data.id);
              if (invocation) invocation.reply = { time: performance.now(), error: data.error?.slice(0, 4096), notAdmitted: data.notAdmitted, result: typeof data.result === "string" ? data.result.slice(0, 4096) : data.result };
            }
            if (data?.kind === "view") {
              const frame = JSON.parse(data.view);
              observeGoalError(JSON.stringify(frame.view?.ui_detail ?? frame.sections?.ui_detail ?? {}), "Worker frame");
            }
          });
        }
        if (message?.method === "command") {
          const request = JSON.parse(message.payload);
          if (request.action === "ui_invoke") {
            const value = request.input?.value;
            boundedPush(window.taskInvocations, { id: message.id, time: performance.now(), ticket: request.ticket, name: request.name,
              kind: value?.kind, request_id: value?.request?.request_id ?? value?.request,
              expected_revision: value?.request?.expected_revision ?? value?.revision,
              action: value?.request?.action, reply: null });
          }
          if (request.action === "ui_visible" && request.keys.length) {
            window.taskVisibleRequests.push(request);
            if (window.taskVisibleRequests.length === 1) {
              queueMicrotask(() => this.dispatchEvent(new MessageEvent("message", { data: {
                kind: "reply", id: message.id, error: "Injected visible-hint admission failure", notAdmitted: true,
              } })));
              return;
            }
          }
        }
        return post.call(this, message, ...rest);
      };
      for (const type of ["mousedown", "mouseup", "click"]) document.addEventListener(type, event => {
        const target = event.target.closest?.("button");
        if (!target) return;
        if (!identities.has(target)) identities.set(target, ++sequence);
        window.taskInputEvents.push({ type, label: target.textContent, id: identities.get(target), time: performance.now() });
        if (window.taskInputEvents.length > 64) window.taskInputEvents.shift();
      }, true);
    });
    page = await context.newPage();
    page.setDefaultTimeout(30000);
    page.on("pageerror", error => errors.push(error.message));
    await page.goto(service.origin);
    await page.getByLabel("Device registration receipt").fill(JSON.stringify(service.register(`${name} task evidence`)));
    await page.getByRole("button", { name: "Connect", exact: true }).click();
    await page.locator("#workbench").waitFor({ state: "visible" });
    await page.locator(".workspace-add summary").click();
    await page.getByLabel("Server directory").fill(service.workspace);
    await page.locator("#workspace-form").getByRole("button", { name: "Add workspace", exact: true }).click();
    await page.locator("#workspaces .nav-item").click();
    await page.getByRole("button", { name: "Trajectory", exact: true }).click();
    const paneSelector = '[aria-label="Main conversation"]';
    const pane = page.locator(paneSelector);
    const detail = page.locator("#detail .ui-contribution");
    const goalUntil = async (predicate, label) => until(async () => {
      const evidence = await page.evaluate(() => ({ errors: window.taskGoalErrors, replies: window.taskInvocations.filter(item => item.name === "goal" && item.reply?.error) }));
      assert.deepEqual(evidence, { errors: [], replies: [] }, `${label}: explicit Goal failure ${JSON.stringify(evidence)}`);
      return predicate();
    }, label);
    const goalVisible = async (locator, label) => goalUntil(() => locator.isVisible(), label);
    const close = async () => {
      if (await page.locator("#detail").isVisible()) await page.getByRole("button", { name: "Close details", exact: true }).click();
      await page.locator("#detail").waitFor({ state: "hidden" });
    };
    const surface = async name => { await close(); await page.getByRole("button", { name, exact: true }).click(); };
    const capture = async (label, isDetail = false, expected = []) => {
      for (const [size, viewport] of [["desktop", { width: 1440, height: 980 }], ["narrow", { width: 390, height: 844 }]]) {
        await page.setViewportSize(viewport);
        await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
        const expected_controls = await assertControls(page, isDetail ? detail : pane, expected);
        await assertNoNotices(page);
        const metric = await geometry(page, paneSelector, isDetail);
        assert(metric.page_width <= viewport.width, JSON.stringify(metric));
        assert(metric.root.left >= 0 && metric.root.right <= viewport.width + 1, JSON.stringify(metric));
        assert(metric.controls.length > 0 && metric.controls.every(button => button.hit), JSON.stringify(metric));
        if (!isDetail) assert(metric.send_hit, JSON.stringify(metric));
        measurements.push({ label, size, expected_controls, ...metric });
        await page.screenshot({ path: join(directory, `${label}-${size}.png`), fullPage: true });
      }
      await page.setViewportSize({ width: 1440, height: 980 });
    };
    const send = async text => {
      await close();
      const before = service.provider.requests.length;
      await pane.getByLabel("Main message", { exact: true }).fill(text);
      assert.equal(await pane.getByLabel("Main message", { exact: true }).inputValue(), text);
      await pane.getByRole("button", { name: "Send ↗", exact: true }).click();
      await until(async () => {
        const review = pane.locator(".pending button").filter({ hasText: "Review:" });
        if (await review.count()) {
          await review.first().click();
          await page.getByRole("button", { name: "Allow once", exact: true }).click();
        }
        return service.provider.requests.length > before && (
          /Completed|Failed/.test(await pane.locator(".pane-status").innerText()) ||
          (await pane.locator(".transcript .message.assistant").last().innerText().catch(() => "")).includes("Waiting for fixture release."));
      }, "submitted Turn");
    };

    await send("Please record an inline patch");
    const inline = pane.locator(".inline-card:visible");
    await inline.locator("pre").filter({ hasText: "+after · 界" }).waitFor();
    assert.equal(await readFile(join(service.workspace, "card.txt"), "utf8"), "after · 界\n");
    await writeFile(join(service.workspace, "card.txt"), "unrelated later edit\n");
    assert.match(await inline.innerText(), /-before/);
    assert.doesNotMatch(await inline.innerText(), /unrelated later edit/);
    await capture("recorded-inline-patch", false, ["Complete result"]);
    const visibilityRetry = await page.evaluate(() => window.taskVisibleRequests.slice(0, 2));
    assert.equal(visibilityRetry.length, 2);
    assert.deepEqual(visibilityRetry[0], visibilityRetry[1], "visible-hint retry must retain its sequence and selection");
    await inline.getByRole("button", { name: "Complete result", exact: true }).click();
    await inline.locator("pre").filter({ hasText: '"status": "applied"' }).waitFor();

    await surface("Goal");
    await detail.getByLabel("Goal objective", { exact: true }).fill("UI Goal hold for explicit control evidence");
    await detail.getByLabel("Maximum automatic rounds", { exact: true }).fill("3");
    await capture("goal-create", true, ["Create and start Goal"]);
    const beforeGoal = service.provider.requests.length;
    await detail.getByRole("button", { name: "Create and start Goal", exact: true }).click();
    await detail.locator(".ui-field").filter({ hasText: "Allocated rounds: 1 / 3" }).waitFor();
    await detail.locator(".ui-field").filter({ hasText: "Current driving: Armed" }).waitFor();
    await until(() => service.provider.requests.length === beforeGoal + 1, "Goal stream entered");
    await detail.locator(".ui-field").filter({ hasText: "Driver: Waiting" }).waitFor();
    await capture("goal-armed", true, ["Pause after current round", "Cancel automatic round"]);
    await detail.getByRole("button", { name: "Pause after current round", exact: true }).click();
    await detail.locator(".ui-field").filter({ hasText: "Durable phase: Paused" }).waitFor();
    await detail.locator(".ui-field").filter({ hasText: "Current driving: Disarmed" }).waitFor();
    assert.equal(service.provider.requests.length, beforeGoal + 1);
    await capture("goal-paused-claimed", true, ["Resume Goal", "Cancel automatic round"]);
    service.provider.release("UI Goal hold");
    await pane.locator(".pane-status").filter({ hasText: "Completed" }).waitFor();
    await detail.locator(".ui-field").filter({ hasText: "Driver: Disarmed" }).waitFor();
    await goalVisible(detail.getByRole("button", { name: "Create and start Goal", exact: true }), "Paused first-round settlement projection");
    await detail.getByRole("button", { name: "Resume Goal", exact: true }).click();
    await goalVisible(detail.locator(".ui-field").filter({ hasText: "Allocated rounds: 2 / 3" }), "second Goal allocation");
    await goalUntil(() => service.provider.requests.length === beforeGoal + 2, "resumed Goal stream entered");
    await detail.getByRole("button", { name: "Cancel automatic round", exact: true }).click();
    await pane.locator(".pane-status").filter({ hasText: "Cancelled" }).waitFor();
    await detail.locator(".ui-field").filter({ hasText: "Driver: Disarmed" }).waitFor();
    assert.match(await detail.innerText(), /Allocated rounds: 2 \/ 3/);
    await capture("goal-cancelled", true, ["Resume Goal", "Create and start Goal"]);

    await send("Please observe background job");
    await surface("Current-Turn Jobs");
    await detail.locator("pre").filter({ hasText: "Running" }).waitFor();
    assert.match(await detail.innerText(), /Reported: false/);
    const requestsBeforeRead = service.provider.requests.length;
    await capture("jobs-running", true, ["Refresh current Turn"]);
    await writeFile(join(service.workspace, "job-release"), "release\n");
    await until(async () => {
      await detail.getByRole("button", { name: "Refresh current Turn", exact: true }).click();
      return /Completed/.test(await detail.innerText());
    }, "job completion status");
    assert.match(await detail.innerText(), /Reported: false · Output retained: true/);
    assert.equal(service.provider.requests.length, requestsBeforeRead);
    await capture("jobs-completed-unreported", true, ["Refresh current Turn"]);
    service.provider.release("observe background job");
    await pane.locator(".pane-status").filter({ hasText: "Failed" }).waitFor();
    await pane.locator(".transcript").getByText("background jobs completed without an explicit report: job-1", { exact: true }).waitFor();
    await detail.getByRole("button", { name: "Refresh current Turn", exact: true }).click();
    await detail.getByText("No active Turn. Jobs are unavailable after their Turn finishes.", { exact: true }).waitFor();
    await capture("jobs-revoked", true, ["Refresh current Turn"]);
    await assertNoNotices(page);
    await close();
    await page.locator("#sign-out").click();
    await page.locator("#login").waitFor({ state: "visible" });
    assert.deepEqual(errors, []);
    results.push({ browser: name, version: browser.version(), binary_sha256: binaryHash, ok: true,
      ui_invocations: await page.evaluate(() => window.taskInvocations), goal_errors: await page.evaluate(() => window.taskGoalErrors),
      measurements, visibility_retry: visibilityRetry, requests: service.provider.requests, jobs_terminal: "failed_unreported_job", clean_sign_out: true });
  } catch (error) {
    await recordTaskFailure(page, directory, { error: String(error), errors, measurements, requests: service?.provider.requests,
      ui_invocations: await page?.evaluate(() => window.taskInvocations).catch(() => []),
      goal_errors: await page?.evaluate(() => window.taskGoalErrors).catch(() => []),
      input_events: await page?.evaluate(() => window.taskInputEvents).catch(() => []) }).catch(reportError => { error.cause = reportError; });
    throw error;
  } finally {
    await browser.close();
    await service?.close();
    await writeFile(join(report, "results.json"), JSON.stringify(results, null, 2));
  }
}
assert.equal(results.length, names.length, "every selected browser must complete the task probe");
