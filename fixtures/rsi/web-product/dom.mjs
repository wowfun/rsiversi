import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";

// DOM projection only: no Worker, provider, device or network lifecycle claims.
export async function verifyDom(browser, root) {
  const page = await browser.newPage();
  try {
    const document = await readFile(join(root, "plugins/rsi/web/index.html"), "utf8");
    await page.setContent(document.replace(/<script[^>]*>[\s\S]*?<\/script>/g, ""));
    await page.addScriptTag({ path: join(root, "plugins/rsi/web/app.js") });
    const results = await page.evaluate(() => {
      return ["approval", "question"].map(kind => {
        const data = { generation: "1", session: "retained-session", path: "/workspace", draft: "",
          model: { deployment: "test", model: "model" }, transcript: { blocks: [], status: "Running", omitted: false },
          pending: [{ id: "request-1", owner: "retained-session", kind, title: "Act on this request" }],
          notice: "", history_more: false, historical: false };
        panes[0].render(data, []);
        const initial = panes[0].waiting.children.length;
        panes[0].reset();
        const closed = panes[0].waiting.children.length;
        panes[0].render(data, []);
        const reopened = panes[0].waiting.children.length;
        panes[0].render({ ...data, generation: "2" }, []);
        return { kind, initial, closed, reopened, replaced: panes[0].waiting.children.length };
      });
    });
    assert.deepEqual(results, ["approval", "question"].map(kind => ({ kind, initial: 1, closed: 0, reopened: 1, replaced: 1 })));
    const approvals = await page.evaluate(() => {
      const sent = [];
      command = async input => { sent.push(input); };
      for (const owner of ["parent-session", "child-session"]) {
        renderDetail({ detail: { pane: 0, generation: "one", kind: "approval", request: {
          id: "call-1", subject: { session_id: owner }, reason: owner, action: "run tool", review: { owner },
        } } });
      }
      const text = $("detail").textContent;
      [...$("detail").querySelectorAll("button")].find(button => button.textContent === "Allow once").click();
      return { text, sent };
    });
    assert(approvals.text.includes("child-session") && !approvals.text.includes("parent-session"));
    assert.equal(approvals.sent.length, 1);
    assert.equal(approvals.sent[0].owner, "child-session");
    const retries = await page.evaluate(async () => {
      const pane = panes[0];
      const data = { generation: "3", session: "retained-session", path: "/workspace", draft: "original",
        unresolved_text: "original", model: { deployment: "test", model: "model" },
        transcript: { blocks: [], status: "Ready", omitted: false }, pending: [], notice: "" };
      pane.flush = async () => {};
      pane.action = async () => {};
      const results = [];
      for (const text of ["original", "edited next draft"]) {
        pane.render(data, []);
        const label = pane.send.textContent;
        const steerDisabled = pane.steer.disabled;
        pane.input.value = text;
        await pane.submit(false);
        results.push({ text, remaining: pane.input.value, label, steerDisabled });
      }
      pane.render({ ...data, unresolved_text: null }, []);
      return { results, resolvedLabel: pane.send.textContent, resolvedSteerDisabled: pane.steer.disabled };
    });
    assert.deepEqual(retries, {
      results: [
        { text: "original", remaining: "", label: "Retry previous", steerDisabled: true },
        { text: "edited next draft", remaining: "edited next draft", label: "Retry previous", steerDisabled: true },
      ],
      resolvedLabel: "Send ↗", resolvedSteerDisabled: false,
    });
    const disconnects = await page.evaluate(async () => {
      const results = [];
      for (const phase of ["draft", "disconnect"]) {
        let terminated = 0;
        let acknowledgements = 0;
        document.addEventListener("rsi-disconnected", () => { acknowledgements++; }, { once: true });
        connected = true;
        worker = { terminate() { terminated++; } };
        $("login").hidden = true; $("workbench").hidden = false;
        for (const pane of panes) pane.flush = async () => {
          if (phase === "draft") throw new Error("Draft handoff failed");
        };
        call = async () => { throw new Error("Disconnect failed"); };
        $("sign-out").click();
        await new Promise(resolve => setTimeout(resolve, 0));
        results.push({ phase, terminated, acknowledgements, connected,
          login: !$("login").hidden, workbench: !$("workbench").hidden,
          enabled: !$("sign-out").disabled });
      }
      return results;
    });
    assert.deepEqual(disconnects, [
      { phase: "draft", terminated: 0, acknowledgements: 0, connected: true, login: false, workbench: true, enabled: true },
      { phase: "disconnect", terminated: 1, acknowledgements: 0, connected: false, login: true, workbench: false, enabled: true },
    ]);
  } finally { await page.close(); }
}
