import assert from "node:assert/strict";
import { join } from "node:path";

export async function verifyRendererDom(page, offer, report, name) {
  await page.evaluate(async (offer) => {
    await fixture.runtime.connection.retire();
    const table = await fixture.MountTable.open();
    await fixture.runtime.attach(
      fixture.makeTransport(),
      table,
      {},
      async () => ({
        drafts: await fixture.DraftStore.open("a".repeat(32), {
          kind: "local",
        }),
      }),
    );
    fixture.assets = offer;
    const reference = {
      application: "ui-nonce",
      target: "3",
      contribution: "4",
      name: "echo",
    };
    fixture.view.panels = [
      {
        view: "1",
        open_revision: "1",
        opening_request: null,
        session: "fixture-session",
        pane: "main",
        generation: "1",
        floating: false,
        reopen: { action: "inspect_block", key: "answer" },
        ui_detail: {
          pane: "main",
          generation: "1",
          ticket: "100",
          binding: reference,
          error: null,
          busy: false,
          model: {
            renderer: "rsi.standard",
            schema: { name: "rsi.standard.view", version: 1 },
            data: null,
            actions: [{ name: "echo", title: "Apply addon" }],
            sources: [],
            standard_view: {
              title: "Addon",
              elements: [
                {
                  kind: "text",
                  text: "<script>window.addonExecuted = true</script>",
                },
                {
                  kind: "input",
                  name: "message",
                  label: "Addon text",
                  value: "initial",
                  multiline: true,
                },
                {
                  kind: "button",
                  action: "echo",
                  label: "Apply addon",
                  value: { expected: "original" },
                },
              ],
            },
          },
        },
      },
    ];
    await fixture.frame();
  }, offer);
  await page.getByLabel("Addon text", { exact: true }).fill("edited 界");
  await page.evaluate(async () => {
    Object.assign(fixture.view.panels[0].ui_detail, {
      ticket: "101",
      busy: true,
    });
    await fixture.frame();
  });
  assert.equal(
    await page.getByLabel("Addon text", { exact: true }).isDisabled(),
    true,
  );
  await page.evaluate(async () => {
    Object.assign(fixture.view.panels[0].ui_detail, {
      error: "Validation rejected",
      busy: false,
    });
    await fixture.frame();
  });
  assert.equal(
    await page.getByLabel("Addon text", { exact: true }).inputValue(),
    "edited 界",
  );
  await page.getByRole("button", { name: "Apply addon", exact: true }).click();
  await page.waitForFunction(() =>
    fixture.calls.some(
      (call) =>
        call.method === "command" &&
        JSON.parse(call.payload).action === "ui_invoke",
    ),
  );
  const result = await page.evaluate(() => ({
    sent: fixture.calls
      .filter(
        (call) =>
          call.method === "command" &&
          JSON.parse(call.payload).action === "ui_invoke",
      )
      .map((call) => JSON.parse(call.payload)),
    scripts: document.querySelectorAll(".ui-contribution script").length,
    executed: !!window.addonExecuted,
    text: document.querySelector(".ui-contribution").textContent,
    ack: fixture.acknowledgements.at(-1),
  }));
  assert.deepEqual(result.sent, [
    {
      action: "ui_invoke",
      ticket: "101",
      name: "echo",
      input: {
        value: { expected: "original" },
        fields: { message: "edited 界" },
      },
    },
  ]);
  assert.equal(result.scripts, 0);
  assert.equal(result.executed, false);
  assert.match(result.text, /<script>/);
  assert.equal(result.ack.resync, false);
  await page.screenshot({
    path: join(report, `${name}-contributed-form-dom.png`),
  });
  await page.evaluate(async () => {
    fixture.view.panels = [];
    await fixture.frame();
  });
  assert.equal(await page.locator(".ui-contribution").count(), 0);
  return result;
}
