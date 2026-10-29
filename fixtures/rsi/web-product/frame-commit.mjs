import { verifyFrameDom } from "./frames.mjs";
import { verifyVisibilityDom } from "./visibility-dom.mjs";
// The real React tree and document factory, with isolated transport/renderer ports.
import assert from "node:assert/strict";
import { mkdir, writeFile, readFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { chromium, firefox } from "playwright";
import { sourceFixture } from "./source-fixture.mjs";
import { fileURLToPath } from "node:url";
export async function verifyFrameCommit(browser, url, report, name) {
  const results = [];
  for (const scenario of [
    "commit",
    "settings",
    "resources",
    "external",
    "patches",
    "visibility",
    "inline-owner",
    "files",
    "references",
    "projections",
    "transcript",
    "renderer-error",
    "renderer-rejection",
    "detached-host",
    "slot-error",
    "unmounted-pane",
    "replacement",
  ]) {
    const page = await browser.newPage({
        viewport: { width: 1440, height: 900 },
      }),
      errors = [];
    page.on("pageerror", (error) => {
      errors.push(error.message);
      console.error(name, scenario, error.message);
    });
    page.on("console", (message) => {
      if (message.type() === "error")
        console.error(name, scenario, message.text());
    });
    await page.goto(url);
    await page
      .waitForFunction(() => window.ready)
      .catch(async (error) => {
        await writeFile(
          join(report, `${name}-${scenario}-failure.html`),
          await page.content(),
        );
        throw error;
      });
    await page.evaluate(() => fixture.frame());
    assert.equal(await page.evaluate(() => fixture.acknowledgements.length), 1);
    await page
      .locator('textarea[aria-label="Main message"]')
      .fill("Draft remains stable");
    if (scenario === "commit") {
      const tools=page.getByLabel('Session tools',{exact:true});
      await tools.click();
      await page.getByRole('button',{name:'Earlier history',exact:true}).waitFor();
      assert.equal(await page.locator('.pane-session').isVisible(),true);
      assert.match(await page.locator('.pane-session').innerText(),/\/fixture · fixture-session/);
      await page.keyboard.press('Escape');
      assert(await tools.evaluate(node=>node===document.activeElement));
      assert.equal(await page.locator('.session-menu').evaluate(node=>node.open),false);
      await tools.click();
      await page.getByRole('button',{name:'Session commands',exact:true}).click();
      assert.equal(await page.locator('.session-menu').evaluate(node=>node.open),false);
      const navigation=page.locator('.navigation-options > summary');
      await navigation.click();await page.getByRole('menuitem',{name:'Refresh',exact:true}).click();
      assert.equal(await navigation.evaluate(node=>node.parentElement.open),false);
      await navigation.click();await page.getByLabel('Main message',{exact:true}).click();
      assert.equal(await navigation.evaluate(node=>node.parentElement.open),false);
    }
    await page.evaluate(() => {
      window.originalInput = document.querySelector(
        'textarea[aria-label="Main message"]',
      );
      window.originalMessage = document.querySelector(".message");
      fixture.view.surfaces.main.transcript.blocks[0].text = "Frame two";
    });
    if (scenario === "settings") {
      await page.evaluate(async () => {
        await fixture.runtime.command({
          action: "settings_read",
          namespace: "fixture.numbers",
        });
        fixture.view.settings_revision = "1";
        fixture.view.settings = {
          ticket: "number-1",
          version: "1",
          namespace: "fixture.numbers",
          text: '{"limit":2}',
          description: {
            writable: true,
            defaults: { limit: 2 },
            metadata: {
              description: "Exact numeric input",
              applies: "live",
              sensitive_fields: [],
              schema: {
                type: "object",
                properties: { limit: { type: "number" } },
                required: ["limit"],
              },
            },
          },
        };
        await fixture.frame();
      });
      const dialog = page.getByRole("dialog", {
        name: "Settings",
        exact: true,
      });
      await dialog
        .getByLabel("Settings / limit", { exact: true })
        .fill("1.0000000000000001");
      await dialog
        .getByRole("button", { name: "Edit as JSON", exact: true })
        .click();
      assert.match(
        await dialog.getByLabel("Settings JSON", { exact: true }).inputValue(),
        /1\.0000000000000001/,
      );
      await dialog
        .getByRole("button", { name: "Use form", exact: true })
        .click();
      await dialog
        .getByRole("status")
        .filter({ hasText: "exact numbers" })
        .waitFor();
      await page.evaluate(async () => {
        fixture.view.settings = {
          ...fixture.view.settings,
          ticket: "number-2",
          version: "2",
          text: '{"limit":5}',
        };
        await fixture.frame();
      });
      await dialog
        .getByRole("alert")
        .filter({ hasText: "Settings changed while this draft was open" })
        .waitFor();
      assert.equal(
        await dialog
          .getByRole("button", { name: "Save settings", exact: true })
          .isDisabled(),
        true,
      );
      await dialog
        .getByRole("button", { name: "Discard edits and reload", exact: true })
        .click();
      await page.evaluate(async () => {
        fixture.view.settings_revision = "2";
        fixture.view.settings = {
          ...fixture.view.settings,
          ticket: "number-3",
        };
        await fixture.frame();
      });
      assert.equal(
        await dialog
          .getByLabel("Settings / limit", { exact: true })
          .inputValue(),
        "5",
      );
      await dialog
        .getByRole("button", { name: "Save settings", exact: true })
        .focus();
      await page.keyboard.press("Enter");
      await page.evaluate(async () => {
        fixture.view.settings_revision = "3";
        fixture.view.settings = {
          ...fixture.view.settings,
          ticket: "number-4",
        };
        await fixture.frame();
      });
      assert.equal(
        await dialog
          .getByRole("button", { name: "Save settings", exact: true })
          .isDisabled(),
        false,
      );
      assert.equal(
        await dialog
          .locator("form[data-settings-ticket]")
          .evaluate(
            (form) =>
              form.contains(document.activeElement) &&
              document.activeElement.matches(
                "button:not(:disabled),input:not(:disabled),textarea:not(:disabled),select:not(:disabled)",
              ),
          ),
        true,
        "fresh Settings retains an enabled keyboard target",
      );
      await page.screenshot({ path: join(report, `${name}-settings.png`) });
      await dialog
        .getByRole("button", { name: "Close settings", exact: true })
        .click();
      await dialog.waitFor({ state: "detached" });
    } else if (scenario === "resources") {
      await page.evaluate(async () => {
        fixture.view.panels = [
          {
            view: "approval",
            open_revision: "1",
            opening_request: null,
            session: "fixture-session",
            pane: "main",
            generation: "1",
            floating: false,
            reopen: {
              action: "inspect_interaction",
              owner: "parent",
              id: "call",
            },
            detail: {
              pane: "main",
              generation: "1",
              kind: "approval",
              ticket: "1",
              request: {
                id: "call",
                subject: { session_id: "parent" },
                reason: "Parent request",
                action: "Read workspace",
                review: {
                  cwd: "/fixture",
                  sandbox: "read-only",
                  arguments: { path: "test.txt" },
                },
              },
            },
          },
        ];
        await fixture.frame();
      });
      await page
        .getByRole("button", { name: "Allow once", exact: true })
        .waitFor();
      await page.evaluate(async () => {
        fixture.view.panels[0].detail.request.subject.session_id = "child";
        fixture.view.panels[0].detail.request.reason = "Child request";
        await fixture.frame();
      });
      await page.getByText("Child request", { exact: true }).waitFor();
      assert.equal(
        await page.getByText("Parent request", { exact: true }).count(),
        0,
      );
      await page.evaluate(()=>fixture.setReply('command',payload=>
        JSON.parse(payload).action==='approve'
          ? new Promise((_,reject)=>{window.rejectDecision=reject}) : '{}'));
      await page
        .getByRole("button", { name: "Allow once", exact: true })
        .click();
      await page.waitForFunction(() =>
        fixture.calls.some(
          (call) =>
            call.method === "command" &&
            JSON.parse(call.payload).action === "approve",
        ),
      );
      await page.evaluate(async()=>{fixture.view.panels[0].detail.ticket='2';await fixture.frame()});
      assert.equal(await page.getByRole('button',{name:'Allow once',exact:true}).isDisabled(),true);
      assert.equal(await page.getByRole('button',{name:'Refresh request',exact:true}).isDisabled(),true);
      await page.evaluate(()=>window.rejectDecision(Error('lost decision reply')));
      await page.getByRole('alert').filter({hasText:'outcome is unknown'}).waitFor();
      await page.evaluate(async()=>{fixture.view.panels[0].detail.ticket='3';await fixture.frame();fixture.setReply('command')});
      assert.equal(await page.getByRole('button',{name:'Deny',exact:true}).isDisabled(),true);
      assert.equal(
        await page.evaluate(
          () =>
            JSON.parse(
              fixture.calls.findLast(
                (call) =>
                  call.method === "command" &&
                  JSON.parse(call.payload).action === "approve",
              ).payload,
            ).owner,
        ),
        "child",
      );
      assert.equal(
        await page
          .getByRole("button", { name: "Deny", exact: true })
          .isDisabled(),
        true,
      );
      await page
        .getByRole("button", { name: "Refresh request", exact: true })
        .click();
      assert.equal(
        await page
          .getByRole("button", { name: "Deny", exact: true })
          .isDisabled(),
        true,
      );
      await page.screenshot({
        path: join(report, `${name}-resource-approval.png`),
      });
      await page.evaluate(async () => {
        fixture.view.panels[0].detail.ticket = "4";
        await fixture.frame();
      });
      assert.equal(
        await page
          .getByRole("button", { name: "Deny", exact: true })
          .isDisabled(),
        false,
      );
      await page.evaluate(async () => {
        fixture.view.panels = [];
        await fixture.frame();
      });
      assert.equal(
        await page.getByRole("button", { name: "Deny", exact: true }).count(),
        0,
      );
    } else if (scenario === "external") {
      await page.evaluate(async () => {
        fixture.view.surfaces.main = {
          kind: "external",
          generation: "external-1",
          external: {
            observed: {
              connected: true,
              snapshot: {
                id: "peer",
                generation: "1",
                endpoint: "Fixture agent",
                cwd: "/fixture",
                status: "running",
              },
              permissions: [
                {
                  id: "permit",
                  generation: "1",
                  title: "Read fixture",
                  options: [
                    { id: "allow", name: "Allow once", kind: "allow_once" },
                  ],
                },
              ],
            },
            capabilities: { submit: true },
            blocks: [
              {
                key: "observed",
                role: "assistant",
                text: "External frame",
                source: {},
              },
            ],
            following: true,
            busy: false,
          },
          attention_focus: {
            kind: "external",
            request: "permit",
            generation: "1",
          },
        };
        await fixture.frame();
      });
      assert.equal(
        await page.evaluate(() => fixture.acknowledgements.length),
        2,
      );
      assert.equal(
        await page
          .locator(".external-permission")
          .evaluate((node) => node === document.activeElement),
        true,
      );
      await page
        .getByLabel("External message", { exact: true })
        .fill("External draft");
      await page.evaluate(async () => {
        window.externalInput = document.querySelector(
          '[aria-label="External message"]',
        );
        window.externalRecord = document.querySelector(".external-record");
        await fixture.frame();
      });
      assert.equal(
        await page.evaluate(
          () =>
            externalInput ===
              document.querySelector('[aria-label="External message"]') &&
            externalRecord === document.querySelector(".external-record"),
        ),
        true,
      );
      await page.getByTestId("composer-send").click();
      await page.waitForFunction(
        () => fixture.runtime.panes.get("main").input.value === "",
      );
      assert.equal(
        await page.evaluate(
          () =>
            fixture.calls.filter(
              (call) =>
                call.method === "command" &&
                JSON.parse(call.payload).command?.kind === "submit",
            ).length,
        ),
        1,
      );
      await page.screenshot({ path: join(report, `${name}-external.png`) });
    } else if (scenario === "patches") {
      await verifyFrameDom(page, report, name);
    } else if (scenario === "visibility") {
      await verifyVisibilityDom(page, report, name);
    } else if (scenario === "inline-owner") {
      await page.evaluate(async () => {
        fixture.probeInline();
        fixture.view.surfaces.main.ui_cards = true;
        fixture.view.surfaces.main.inline = {
          answer: { model: {}, ticket: "one", binding: { epoch: "one" } },
        };
        await fixture.frame();
        fixture.view.surfaces.main.inline = {
          answer: { error: "Card temporarily unavailable" },
        };
        await fixture.frame();
      });
      assert.equal(
        await page.evaluate(() => fixture.retainedInline),
        true,
        "React and the controller leave renderer descendants intact until renderer retirement",
      );
      await page
        .getByText("Card temporarily unavailable", { exact: true })
        .waitFor();
    } else if (scenario === "files") {
      await page.locator('textarea[aria-label="Main message"]').fill("Read @");
      await page.getByRole("button", { name: "Add files or run commands", exact: true }).click();
      await page
        .getByRole("menuitem", { name: "File", exact: true })
        .click();
      const dialog = page.getByRole("dialog", {
        name: "Insert workspace file path",
      });
      await dialog
        .getByRole("button", { name: "test.txt · preview", exact: true })
        .click();
      await dialog
        .getByRole("button", { name: "Insert file path", exact: true })
        .click();
      assert.equal(
        await page.locator('textarea[aria-label="Main message"]').inputValue(),
        "Read @test.txt",
      );
      assert.equal(
        await page
          .locator('textarea[aria-label="Main message"]')
          .evaluate((node) => node === document.activeElement),
        true,
      );
      await page.getByRole("button", { name: "Add files or run commands", exact: true }).click();
      await page
        .getByRole("menuitem", { name: "File", exact: true })
        .click();
      await dialog
        .getByRole("button", { name: "Reference conversation", exact: true })
        .click();
      await page
        .getByRole("dialog", { name: "Reference a conversation" })
        .waitFor();
      assert.equal(await dialog.count(), 0);
      await page
        .getByRole("dialog", { name: "Reference a conversation" })
        .getByRole("button", { name: "Close", exact: true })
        .click();
      await page.screenshot({ path: join(report, `${name}-files.png`) });
    } else if (scenario === "references") {
      await page.getByRole("button", { name: "Add files or run commands", exact: true }).click();
      await page
        .getByRole("menuitem", { name: "Conversation", exact: true })
        .click();
      const dialog = page.getByRole("dialog", {
        name: "Reference a conversation",
      });
      await dialog
        .getByLabel("Source Session ID", { exact: true })
        .fill("source");
      await dialog
        .getByRole("button", { name: "Capture preview", exact: true })
        .click();
      await dialog
        .getByRole("button", { name: "Add to draft", exact: true })
        .click();
      await page.locator(".reference-row").waitFor();
      await page.evaluate(async () => {
        const pane = fixture.runtime.panes.get("main");
        pane.edit("");
        await pane.flush();
        window.referenceRow = document.querySelector(".reference-row");
        for (let i = 0; i < 5; i++) await fixture.frame();
        await pane.restoreDrafts();
      });
      assert.equal(
        await page.evaluate(
          () => referenceRow === document.querySelector(".reference-row"),
        ),
        true,
      );
      assert.equal(
        await page
          .getByRole("button", { name: "Remove empty draft", exact: true })
          .count(),
        0,
        "a reference-only draft is not empty",
      );
      await page
        .getByRole("button", { name: "Close saved drafts", exact: true })
        .click();
      await page
        .getByRole("button", { name: "Preview reference", exact: true })
        .click();
      await page
        .getByRole("dialog", { name: "Frozen conversation reference" })
        .getByRole("button", { name: "Close", exact: true })
        .click();
      await page
        .getByRole("button", { name: "Remove reference", exact: true })
        .click();
      await page.waitForFunction(
        () => !document.querySelector(".reference-row"),
      );
      assert.equal(
        await page.evaluate(
          () =>
            fixture.calls.filter(
              (call) =>
                call.method === "reference_input" &&
                JSON.parse(call.payload).operation.kind === "release",
            ).length,
        ),
        1,
      );
      await page.getByRole("button", { name: "Add files or run commands", exact: true }).click();
      await page
        .getByRole("menuitem", { name: "Conversation", exact: true })
        .click();
      await dialog
        .getByLabel("Source Session ID", { exact: true })
        .fill("source");
      await dialog
        .getByRole("button", { name: "Capture preview", exact: true })
        .click();
      await dialog
        .getByText("Captured. Review it before adding to the draft.", {
          exact: true,
        })
        .waitFor();
      await dialog.getByRole("button", { name: "Close", exact: true }).click();
      await page.waitForFunction(
        () =>
          fixture.calls.filter(
            (call) =>
              call.method === "reference_input" &&
              JSON.parse(call.payload).operation.kind === "release",
          ).length === 2,
      );
      assert.equal(
        await page
          .locator('textarea[aria-label="Main message"]')
          .evaluate((node) => node === document.activeElement),
        true,
      );
      await page.screenshot({ path: join(report, `${name}-references.png`) });
    } else if (scenario === "projections") {
      await page.evaluate(async () => {
        Object.assign(fixture.view.surfaces.main, {
          active: "turn",
          queue: [
            {
              queue_slot: { id: "slot" },
              source_kind: "human",
              delivery_label: "Queued",
              message_id: "message",
              read_label: "Read pending",
              editable: true,
            },
          ],
          pending: [
            {
              kind: "question",
              title: "Choose",
              owner: "session",
              id: "question",
            },
          ],
          commands: {
            revision: { kind: "draft" },
            commands: [{ name: "help", description: "Help", draft_safe: true }],
          },
        });
        fixture.hold();
        window.frame = fixture.frame();
      });
      await page.locator(".queue-action").waitFor();
      assert.equal(await page.locator(".queue-action").isDisabled(), true);
      assert.equal(
        await page
          .getByRole("button", { name: "Stop", exact: true })
          .isVisible(),
        false,
      );
      await page.evaluate(async () => {
        fixture.release();
        await window.frame;
      });
      await page.waitForFunction(
        () => !document.querySelector(".queue-action").disabled,
      );
      await page
        .getByRole("button", { name: "Read pending", exact: true })
        .click();
      await page.getByRole("dialog", { name: "Edit queued input" }).waitFor();
      await page
        .getByLabel("Queued text 1", { exact: true })
        .fill("Updated queued text");
      await page.getByRole("button", { name: "Add text", exact: true }).click();
      assert.equal(
        await page.getByLabel("Queued text 1", { exact: true }).inputValue(),
        "Updated queued text",
      );
      assert.equal(
        await page
          .getByLabel("Queued text 3", { exact: true })
          .evaluate((node) => node === document.activeElement),
        true,
      );
      await page
        .getByRole("dialog", { name: "Edit queued input" })
        .getByRole("button", { name: "Close", exact: true })
        .click();
      assert.equal(
        await page
          .getByRole("button", { name: "Read pending", exact: true })
          .evaluate((node) => node === document.activeElement),
        true,
      );
      await page
        .getByRole("button", { name: "Read pending", exact: true })
        .click();
      await page.getByRole("dialog", { name: "Edit queued input" }).waitFor();
      await page.evaluate(async () => {
        fixture.view.surfaces.main.protected = true;
        await fixture.frame();
      });
      assert.equal(
        await page.getByRole("dialog", { name: "Edit queued input" }).count(),
        0,
      );
      await page.evaluate(async () => {
        fixture.view.surfaces.main.protected = false;
        await fixture.frame();
      });
      await page.getByRole("button", { name: "Answer: Choose" }).click();
      assert.equal(
        await page.evaluate(() =>
          fixture.calls.some(
            (call) =>
              call.method === "command" &&
              JSON.parse(call.payload).action === "inspect_interaction" &&
              JSON.parse(call.payload).id === "question",
          ),
        ),
        true,
      );
      await page.evaluate(async () => {
        fixture.view.surfaces.main.protected = true;
        await fixture.frame();
      });
      assert.equal(await page.locator(".queue-action").count(), 0);
      assert.equal(
        await page.locator('[data-interaction-id="question"]').count(),
        0,
      );
      assert.equal(
        await page
          .getByRole("button", { name: "Stop", exact: true })
          .isVisible(),
        false,
      );
      await page.evaluate(async () => {
        fixture.view.surfaces.main.protected = false;
        await fixture.frame();
      });
      await page.getByRole("button", { name: "Answer: Choose" }).waitFor();
      await page.locator('textarea[aria-label="Main message"]').fill("/he");
      await page.evaluate(async () => {
        const pane = fixture.runtime.panes.get("main"),
          state = pane.completionState;
        fixture.view.surfaces.main.completions = {
          sequence: state.sequence,
          query: state.query,
          entries: [
            { replacement: "/help", description: "Help", group: "command" },
            {
              replacement: "/history",
              description: "History",
              group: "command",
            },
          ],
          notice: "",
        };
        await fixture.frame();
        window.firstOption = document.querySelector(".completion-option");
      });
      await page.locator(".completion-option").first().waitFor();
      await page
        .locator('textarea[aria-label="Main message"]')
        .press("ArrowDown");
      assert.equal(
        await page
          .locator(".completion-option")
          .nth(1)
          .getAttribute("aria-selected"),
        "true",
      );
      await page.evaluate(async () => {
        for (let i = 0; i < 10; i++) await fixture.frame();
      });
      assert.equal(
        await page.evaluate(
          () => firstOption === document.querySelector(".completion-option"),
        ),
        true,
      );
      await page.evaluate(() => {
        const pane = fixture.runtime.panes.get("main");
        pane.completionFailure = "Request failed";
        pane.renderCompletions(fixture.view.surfaces.main);
      });
      await page
        .locator('.input-completions [role="status"]')
        .getByText("Request failed")
        .waitFor();
      assert.equal(
        await page.evaluate(
          () => firstOption === document.querySelector(".completion-option"),
        ),
        true,
        "error text must not remount options",
      );
      assert.equal(
        await page.locator('[role="listbox"] [role="status"]').count(),
        0,
      );
      await page.locator('textarea[aria-label="Main message"]').press("Enter");
      assert.equal(
        await page.locator('textarea[aria-label="Main message"]').inputValue(),
        "/history",
      );
      assert.equal(await page.locator(".input-completions").isVisible(), false);
      await page.screenshot({ path: join(report, `${name}-projections.png`) });
    } else if (scenario === "transcript") {
      await page.evaluate(async () => {
        const transcript = {
          status: "Running",
          blocks: [
            {
              key: "process",
              role: "reasoning",
              title: "Thinking",
              text: "Process\n".repeat(80),
              sources: 1,
            },
            {
              key: "answer",
              role: "assistant",
              title: "Assistant",
              text: "Answer\n".repeat(100),
              sources: 1,
            },
          ],
          omitted: false,
          turns: {
            entries: {
              turn: {
                id: "turn",
                status: "Running",
                running: true,
                foldable: true,
                partial: false,
                blocks: ["process", "answer"],
                process: ["process"],
                candidate: ["answer"],
                answer: [],
              },
            },
          },
        };
        fixture.view.surfaces.main.transcript = transcript;
        await fixture.frame();
        window.messages = [...document.querySelectorAll(".message")];
      });
      const retained = await page.evaluate(async () => {
        const container = document.querySelector(".transcript"),
          observer = new MutationObserver(() => {});
        observer.observe(container, {
          childList: true,
          attributes: true,
          subtree: true,
        });
        await fixture.frame();
        const records = observer.takeRecords(),
          moves = records.filter(
            (record) => record.type === "childList",
          ).length,
          blockAttributeWrites = records.filter(
            (record) =>
              record.type === "attributes" &&
              record.target.matches(".message,.turn-summary"),
          ).length;
        observer.disconnect();
        const owner = fixture.runtime.panes.get("main"),
          answer = messages[1];
        container.scrollTop +=
          answer.getBoundingClientRect().top -
          container.getBoundingClientRect().top +
          45;
        owner.readingSummary = true;
        const before = answer.getBoundingClientRect().top;
        Object.assign(
          fixture.view.surfaces.main.transcript.turns.entries.turn,
          {
            status: "Completed",
            running: false,
            candidate: [],
            answer: ["answer"],
          },
        );
        await fixture.frame();
        return {
          moves,
          blockAttributeWrites,
          identity: messages.every(
            (node, index) =>
              node === document.querySelectorAll(".message")[index],
          ),
          collapsed: messages[0].hidden && !messages[1].hidden,
          anchorDelta: Math.abs(answer.getBoundingClientRect().top - before),
        };
      });
      assert.equal(retained.moves, 0);
      assert.equal(retained.blockAttributeWrites, 0);
      assert.equal(retained.identity, true);
      assert.equal(retained.collapsed, true);
      assert(retained.anchorDelta <= 1, JSON.stringify(retained));
      assert.equal(
        await page.locator(".turn-summary").getAttribute("aria-expanded"),
        "false",
      );
      await page.locator(".turn-summary").click();
      assert.equal(
        await page
          .locator(".message.reasoning")
          .evaluate((node) => node.hidden),
        false,
      );
      assert.equal(
        await page.locator(".turn-summary").getAttribute("aria-expanded"),
        "true",
      );
      await page.locator(".turn-summary").click();
      await page.evaluate(async () => {
        Object.assign(
          fixture.view.surfaces.main.transcript.turns.entries.turn,
          { status: "Cancelled", foldable: false },
        );
        await fixture.frame();
      });
      assert.equal(
        await page
          .locator(".message.reasoning")
          .evaluate((node) => node.hidden),
        false,
      );
      assert.equal(
        await page.locator(".turn-summary").getAttribute("aria-expanded"),
        "true",
      );
      await page.evaluate(() => {
        const select = document.querySelector('[aria-label="Detail level"]');
        select.value = "detailed";
        select.dispatchEvent(new Event("change", { bubbles: true }));
      });
      await page.locator(".message.turn-expanded").first().waitFor();
      await page.evaluate(async () => {
        fixture.view.surfaces.main.transcript.turns.entries = {};
        await fixture.frame();
      });
      assert.equal(
        await page.locator(".message.turn-expanded,.turn-summary").count(),
        0,
      );
      await page.screenshot({ path: join(report, `${name}-transcript.png`) });
      results.push({
        browser: name,
        scenario: "transcript-identity-and-anchor",
        ...retained,
      });
    } else if (scenario === "commit") {
      await page.evaluate(() => {
        fixture.hold();
        window.frame = fixture.frame();
      });
      await page.getByText("Frame two", { exact: true }).waitFor();
      assert.equal(
        await page.evaluate(() => fixture.acknowledgements.length),
        1,
        "renderer must settle before ACK",
      );
      await page.evaluate(async () => {
        fixture.release();
        await window.frame;
      });
      assert.equal(
        await page.evaluate(() => fixture.acknowledgements.length),
        2,
      );
      assert.deepEqual(
        await page.evaluate(() => ({
          input:
            originalInput ===
            document.querySelector('textarea[aria-label="Main message"]'),
          message: originalMessage === document.querySelector(".message"),
          focus: document.activeElement === originalInput,
          text: originalInput.value,
        })),
        {
          input: true,
          message: true,
          focus: true,
          text: "Draft remains stable",
        },
      );
      await page.setViewportSize({width:800,height:900});
      await page.getByRole('button',{name:'Expand navigation',exact:true}).click();
      const drawer=page.getByRole('dialog',{name:'Workspace navigation',exact:true});
      await drawer.getByRole('button',{name:'Collapse navigation',exact:true}).click();
      await drawer.waitFor({state:'hidden'});
      await page.setViewportSize({width:1440,height:900});
      await page.getByRole('button',{name:'Collapse navigation',exact:true}).click();
      await page.getByRole('button',{name:'Expand navigation',exact:true}).click();
      assert.equal(await drawer.count(),0,'Wide rail restores the docked sidebar');
      await page.getByRole('button',{name:'Collapse navigation',exact:true}).waitFor();
      await page.screenshot({ path: join(report, `${name}-commit.png`) });
    } else if (scenario === "replacement") {
      await page.evaluate(() => {
        fixture.hold();
        window.frame = fixture.frame();
      });
      await page.getByText("Frame two", { exact: true }).waitFor();
      await page.evaluate(async () => {
        await fixture.owner.dispose();
        fixture.release();
        await window.frame;
      });
      assert.equal(
        await page.evaluate(() => fixture.acknowledgements.length),
        1,
      );
    } else if (scenario === "renderer-rejection") {
      await page.evaluate(async () => {fixture.rendererError(false);await fixture.frame()});
      await page.waitForFunction(()=>fixture.acknowledgements.length===2);
      assert.equal(await page.evaluate(()=>fixture.runtime.connection.phase),"Ready");
      assert.deepEqual(await page.evaluate(()=>fixture.acknowledgements.at(-1).renderer),{revision:"rejected",accept:false,error:"fixture renderer failed"});
      await page.locator('#notice').filter({hasText:'fixture renderer failed'}).waitFor();
    } else {
      await page.evaluate((scenario) => {
        if (scenario === "renderer-error") fixture.rendererError();
        else if (scenario === "detached-host") fixture.detach();
        else if (scenario === "unmounted-pane") {
          // Inject the interval after projection allocated a Pane and before
          // React mounted it, then exercise the real connection failure cleanup.
          const Pane = fixture.runtime.panes.get('main').constructor;
          const pane = new Pane('compare');
          pane.staged = {data: structuredClone(fixture.view.surfaces.main)};
          fixture.runtime.panes.set('compare',pane);
          fixture.breakSlot();
        } else fixture.breakSlot();
      }, scenario);
      if (!["slot-error","unmounted-pane"].includes(scenario)) await page.evaluate(() => fixture.frame());
      await page.waitForFunction(
        () => fixture.runtime.connection.phase === "Failed",
      );
      assert.equal(
        await page.evaluate(() => fixture.acknowledgements.length),
        1,
        "failed presentation has no successful ACK",
      );
      if (scenario === 'unmounted-pane') {
        assert.equal(await page.evaluate(()=>fixture.runtime.panes.get('compare').input),undefined);
        await page.getByText('Connection failed',{exact:true}).waitFor();
        await page.locator('#notice').filter({hasText:'Reconnect explicitly'}).waitFor();
      }
    }
    if (!["slot-error","unmounted-pane"].includes(scenario)) assert.deepEqual(errors, []);
    else
      assert(
        errors.every((error) => error.includes("fixture React slot failed")),
      );
    results.push({
      browser: name,
      version: browser.version(),
      scenario,
      errors,
    });
    await page.close();
  }
  return results;
}
if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  const report = resolve(process.env.RSI_REPORT_DIR);
  await mkdir(report, { recursive: true });
  const source = await readFile(
    new URL("./document-scene.tsx", import.meta.url),
    "utf8",
  );
  const server = await sourceFixture({ name: "frame-fixture", source }),
    results = [];
  try {
    for (const [name, engine] of Object.entries({ chromium, firefox })) {
      const browser = await engine.launch();
      try {
        results.push(
          ...(await verifyFrameCommit(browser, server.url, report, name)),
        );
      } finally {
        await browser.close();
      }
    }
    await writeFile(
      join(report, "result.json"),
      JSON.stringify(results, null, 2),
    );
  } finally {
    await server.close();
  }
}
