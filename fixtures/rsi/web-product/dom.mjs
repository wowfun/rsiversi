import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { verifyImageDom } from "./images-dom.mjs";

// DOM projection only: no Worker, provider, device or network lifecycle claims.
export async function verifyDom(browser, root, report, name) {
  const page = await browser.newPage();
  try {
    const document = await readFile(join(root, "plugins/rsi/web/index.html"), "utf8");
    await page.setContent(document.replace(/<script[^>]*>[\s\S]*?<\/script>/g, ""));
    await page.addStyleTag({ path: join(root, "plugins/rsi/web/styles.css") });
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
    await verifyImageDom(page);
    const ime = await page.evaluate(() => {
      let submitted = 0;
      panes[0].submit = async () => { submitted++; };
      const input = panes[0].input;
      for (const options of [{ isComposing: true }, { keyCode: 229 }]) {
        input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", ctrlKey: true, bubbles: true, cancelable: true, ...options }));
      }
      const composing = submitted;
      input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", ctrlKey: true, bubbles: true, cancelable: true }));
      return { composing, after: submitted };
    });
    assert.deepEqual(ime, { composing: 0, after: 1 });
    const contributed = await page.evaluate(() => {
      const sent = [];
      command = async input => { sent.push(input); };
      const reference = { application: "ui-nonce", target: "3", contribution: "4", name: "echo" };
      const bound = { reference, actions: { echo: reference }, view: { title: "Addon", elements: [
        { kind: "text", text: "<script>window.addonExecuted = true</script>" },
        { kind: "input", name: "message", label: "Addon text", value: "initial", multiline: true },
        { kind: "button", action: "echo", label: "Apply addon", value: { expected: "original" } },
      ] } };
      const detail = { pane: 0, generation: "one", ticket: "100", view: bound, error: null, busy: false };
      renderDetail({ ui_detail: detail });
      document.querySelector("[data-ui-field]").value = "edited 界";
      renderDetail({ ui_detail: { ...detail, ticket: "101", busy: true } });
      const busy = document.querySelector("[data-ui-field]").disabled;
      renderDetail({ ui_detail: { ...detail, ticket: "101", error: "Validation rejected", busy: false } });
      document.querySelector(".ui-contribution > button").click();
      return { sent, busy, remaining: document.querySelector("[data-ui-field]").value,
        scripts: document.querySelectorAll(".ui-contribution script").length,
        executed: !!window.addonExecuted, text: document.querySelector(".ui-contribution").textContent };
    });
    assert.equal(contributed.busy, true);
    assert.equal(contributed.remaining, "edited 界");
    assert.equal(contributed.scripts, 0); assert.equal(contributed.executed, false);
    assert.match(contributed.text, /<script>/);
    assert.deepEqual(contributed.sent, [{ action: "ui_invoke", ticket: "101",
      reference: { application: "ui-nonce", target: "3", contribution: "4", name: "echo" },
      input: { value: { expected: "original" }, fields: { message: "edited 界" } } }]);
    await page.screenshot({ path: join(report, `${name}-contributed-form-dom.png`) });
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
