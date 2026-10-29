import assert from "node:assert/strict";
import { writeFile } from "node:fs/promises";
import { join } from "node:path";

// Connection factory ports are deterministic; the UI, React commits, draft store,
// transport admission and owner retirement are the production implementations.
export async function verifyConnectionDom(browser, url, report, name) {
  const results = [];
  const cases = [
    "falsy-receipts",
    "close-failures",
    "drain-order",
    "late-render",
    "late-save",
    "navigation-replacement",
    "opening-close",
    "authentication-failure",
    "cleanup-failure",
    "native-cancel-failure",
  ];
  for (const scenario of cases) {
    const page = await browser.newPage({
        viewport: { width: 1440, height: 900 },
      }),
      errors = [];
    page.on("pageerror", (error) => errors.push(error.message));
    try {
      await page.goto(`${url}?native`);
      await page.waitForFunction(() => window.ready);
      await page.evaluate(async () => {
        await fixture.frame();
        await fixture.runtime.panes.get("main").binding;
        fixture.state = () => ({
          phase: fixture.runtime.connection.phase,
          label: document.querySelector("#connection-state").textContent,
          login: !document.querySelector("#login").hidden,
          workbench: !document.querySelector("#workbench").hidden,
          enabled: !document.querySelector("[data-testid=disconnect]").disabled,
        });
        fixture.reconnect = async () => {
          await fixture.runtime.connection.retire();
          await fixture.runtime.connect("{}");
          await fixture.frame();
          await fixture.runtime.panes.get("main").binding;
        };
      });
      let result;
      if (scenario === "falsy-receipts") {
        result = await page.evaluate(async () => {
          const values = [];
          for (const receipt of [null, 0, ""]) {
            await fixture.reconnect();
            const transport = fixture.transport;
            let received = 0;
            const listener = () => received++;
            document.addEventListener("rsi-disconnected", listener);
            fixture.setReply("disconnect", () => receipt);
            await fixture.runtime.disconnect();
            await fixture.tick();
            document.removeEventListener("rsi-disconnected", listener);
            values.push({
              receipt,
              terminated: transport.terminated,
              received,
              ...fixture.state(),
            });
          }
          return values;
        });
        assert.deepEqual(
          result,
          [null, 0, ""].map((receipt) => ({
            receipt,
            terminated: 1,
            received: 1,
            phase: "Closed",
            label: "Disconnected",
            login: true,
            workbench: false,
            enabled: true,
          })),
        );
      } else if (scenario === "close-failures") {
        result = await page.evaluate(async () => {
          const results = [];
          for (const phase of ["storage", "disconnect"]) {
            await fixture.reconnect();
            const transport = fixture.transport,
              pane = fixture.runtime.panes.get("main"),
              flush = pane.flush;
            let received = 0;
            const listener = () => received++;
            document.addEventListener("rsi-disconnected", listener);
            fixture.setReply("disconnect", () => {
              throw Error("Fixture disconnect failed");
            });
            if (phase === "storage")
              pane.flush = async () => {
                throw Error("Fixture save failed");
              };
            try {
              await fixture.runtime.disconnect();
              await fixture.tick();
              results.push({
                phase,
                terminated: transport.terminated,
                received,
                ...fixture.state(),
              });
            } finally {
              pane.flush = flush;
              document.removeEventListener("rsi-disconnected", listener);
            }
          }
          return results;
        });
        assert.deepEqual(result, [
          {
            phase: "Ready",
            terminated: 0,
            received: 0,
            label: "Connected",
            login: false,
            workbench: true,
            enabled: true,
          },
          {
            phase: "Failed",
            terminated: 1,
            received: 0,
            label: "Connection failed",
            login: true,
            workbench: false,
            enabled: true,
          },
        ]);
      } else if (scenario === "drain-order") {
        result = await page.evaluate(async () => {
          const order = [];
          let releaseDrafts, releaseMounts, enteredMounts;
          const drafts = new Promise((resolve) => (releaseDrafts = resolve)),
            mounts = new Promise((resolve) => (releaseMounts = resolve)),
            entered = new Promise((resolve) => (enteredMounts = resolve));
          await fixture.runtime.connection.retire();
          fixture.table({
            render: async () => ({}),
            close() {
              order.push("dispose-start");
              enteredMounts();
              return mounts.then(() => order.push("dispose-done"));
            },
          });
          await fixture.runtime.connect("{}");
          await fixture.frame();
          const transport = fixture.transport;
          transport.teardown = () => order.push("terminate");
          fixture.setReply("disconnect", () => {
            order.push("disconnect");
            return { active_requests: 0 };
          });
          for (const pane of fixture.runtime.panes.values())
            pane.flush = () => drafts;
          const closing = fixture.runtime.disconnect();
          await Promise.resolve();
          const beforeDrafts = [...order];
          releaseDrafts();
          await entered;
          const beforeDisposal = [...order];
          releaseMounts();
          await closing;
          await fixture.tick();
          return {
            beforeDrafts,
            beforeDisposal,
            order,
            state: fixture.state(),
          };
        });
        assert.deepEqual(result, {
          beforeDrafts: [],
          beforeDisposal: ["dispose-start"],
          order: ["dispose-start", "dispose-done", "disconnect", "terminate"],
          state: {
            phase: "Closed",
            label: "Disconnected",
            login: true,
            workbench: false,
            enabled: true,
          },
        });
      } else if (scenario === "late-render") {
        result = await page.evaluate(async () => {
          const old = fixture.runtime.connection,
            transport = fixture.transport,
            acks = fixture.acknowledgements.length;
          fixture.hold();
          const frame = fixture.frame();
          await fixture.tick();
          await old.fail(Error("Fixture lost transport"));
          await fixture.runtime.connect("{}");
          const fresh = fixture.runtime.connection,
            freshTransport = fixture.transport;
          fixture.release();
          await frame;
          await fixture.tick();
          return {
            retained: fixture.runtime.connection === fresh,
            oldPhase: old.phase,
            oldTerminations: transport.terminated,
            freshTerminations: freshTransport.terminated,
            lateAcks: fixture.acknowledgements.length - acks,
            state: fixture.state(),
          };
        });
        assert.deepEqual(result, {
          retained: true,
          oldPhase: "Failed",
          oldTerminations: 1,
          freshTerminations: 0,
          lateAcks: 0,
          state: {
            phase: "Ready",
            label: "Connected",
            login: false,
            workbench: true,
            enabled: true,
          },
        });
      } else if (scenario === "late-save") {
        result = await page.evaluate(async () => {
          const results = [];
          for (const fails of [false, true]) {
            await fixture.reconnect();
            const old = fixture.runtime.connection;
            let finish, entered;
            const started = new Promise((resolve) => (entered = resolve)),
              saving = new Promise(
                (resolve, reject) =>
                  (finish = () =>
                    fails ? reject(Error("Old save failed")) : resolve()),
              );
            const pane = fixture.runtime.panes.get("main"),
              flush = pane.flush;
            pane.flush = () => {
              entered();
              return saving;
            };
            const closing = fixture.runtime.disconnect();
            await started;
            await old.fail(Error("Fixture transport failed"));
            await fixture.runtime.connect("{}");
            const fresh = fixture.runtime.connection,
              transport = fixture.transport;
            finish();
            await closing;
            pane.flush = flush;
            await fixture.tick();
            results.push({
              fails,
              retained: fixture.runtime.connection === fresh,
              oldPhase: old.phase,
              terminations: transport.terminated,
              disconnects: transport.calls.filter(
                (call) => call.method === "disconnect",
              ).length,
              state: fixture.state(),
            });
          }
          return results;
        });
        assert.deepEqual(
          result,
          [false, true].map((fails) => ({
            fails,
            retained: true,
            oldPhase: "Failed",
            terminations: 0,
            disconnects: 0,
            state: {
              phase: "Ready",
              label: "Connected",
              login: false,
              workbench: true,
              enabled: true,
            },
          })),
        );
      } else if (scenario === "navigation-replacement") {
        result = await page.evaluate(async () => {
          const results = [];
          for (const action of ["open", "close"]) {
            await fixture.reconnect();
            const pane = fixture.runtime.panes.get("main"),
              flush = pane.flush;
            let release, entered;
            const started = new Promise((resolve) => (entered = resolve)),
              saving = new Promise((resolve) => (release = resolve));
            pane.flush = () => {
              entered();
              return saving;
            };
            const opening = (
              action === "open"
                ? fixture.input.open({
                    action: "open",
                    session: "undispatched",
                  })
                : fixture.input.close("main")
            ).catch((error) => error.message);
            await started;
            const fenced = pane.switching;
            await fixture.runtime.connection.fail(
              Error("Old connection failed"),
            );
            await fixture.runtime.connect("{}");
            const cleared = !pane.switching;
            pane.switching = true;
            release();
            await opening;
            const retained = pane.switching;
            pane.switching = false;
            pane.flush = flush;
            await fixture.frame();
            await pane.binding;
            results.push({
              action,
              fenced,
              cleared,
              retained,
              inputEnabled: !pane.input.disabled,
            });
          }
          return results;
        });
        assert.deepEqual(
          result,
          ["open", "close"].map((action) => ({
            action,
            fenced: true,
            cleared: true,
            retained: true,
            inputEnabled: true,
          })),
        );
      } else if (scenario === "opening-close") {
        result = await page.evaluate(async () => {
          await fixture.runtime.connection.retire();
          let release, entered, received;
          const storage = new Promise((resolve) => (release = resolve)),
            opening = new Promise((resolve) => (entered = resolve)),
            receipt = new Promise((resolve) => (received = resolve));
          let flushes = 0,
            disposals = 0;
          const transport = fixture.makeTransport();
          const table = {
            close() {
              disposals++;
            },
            render: async () => ({}),
          };
          const attaching = fixture.runtime.attach(transport, table, {}, () => {
            entered();
            return storage;
          });
          await opening;
          for (const pane of fixture.runtime.panes.values())
            pane.flush = async () => flushes++;
          fixture.nativeCalls.length = 0;
          document.addEventListener("rsi-disconnected", received, {
            once: true,
          });
          window.dispatchEvent(
            new CustomEvent("rsi-native-close", { detail: 7 }),
          );
          const before = {
            phase: fixture.runtime.connection.phase,
            flushes,
            terminations: transport.terminated,
          };
          release({});
          await attaching;
          await receipt;
          await fixture.tick();
          return {
            before,
            flushes,
            expectedFlushes: fixture.runtime.panes.size,
            disposals,
            terminations: transport.terminated,
            disconnects: transport.calls.filter(
              (call) => call.method === "disconnect",
            ).length,
            state: fixture.state(),
            nativeCalls: fixture.nativeCalls,
          };
        });
        assert.deepEqual(result, {
          before: { phase: "Opening", flushes: 0, terminations: 0 },
          flushes: result.expectedFlushes,
          expectedFlushes: result.expectedFlushes,
          disposals: 1,
          terminations: 1,
          disconnects: 1,
          state: {
            phase: "Closed",
            label: "Disconnected",
            login: true,
            workbench: false,
            enabled: true,
          },
          nativeCalls: [{ kind: "close", attempt: 7, ready: true }],
        });
      } else if (scenario === "authentication-failure") {
        result = await page.evaluate(async () => {
          let reject,
            entered,
            frames = 0;
          const storage = new Promise((_, fail) => (reject = fail)),
            opening = new Promise((resolve) => (entered = resolve)),
            transport = fixture.makeTransport();
          const table = {
            render() {
              frames++;
              return {};
            },
            close() {},
          };
          const attaching = fixture.runtime
            .attach(transport, table, {}, () => {
              entered();
              return storage;
            })
            .catch((error) => error.message);
          await opening;
          const pending = fixture.frame();
          reject("Fixture draft setup failed");
          const message = await attaching;
          await pending;
          await fixture.runtime.connection.settled();
          await fixture.tick();
          return {
            message,
            frames,
            terminations: transport.terminated,
            state: fixture.state(),
          };
        });
        assert.deepEqual(result, {
          message: "Fixture draft setup failed",
          frames: 0,
          terminations: 1,
          state: {
            phase: "Failed",
            label: "Connection failed",
            login: true,
            workbench: false,
            enabled: true,
          },
        });
      } else if (scenario === "cleanup-failure") {
        result = await page.evaluate(async () => {
          const owner = fixture.runtime.connection,
            count = fixture.transports.length;
          fixture.transport.teardown = () =>
            Promise.reject(Error("Fixture teardown failed"));
          await owner.fail(Error("Fixture disconnect")).catch(() => {});
          await fixture.runtime.connect("{}");
          await fixture.tick();
          return {
            creations: fixture.transports.length - count,
            retained: owner === fixture.runtime.connection,
            state: fixture.state(),
            notice: document.querySelector("#notice").textContent,
          };
        });
        assert.equal(result.creations, 0);
        assert.equal(result.retained, true);
        assert.equal(result.state.phase, "Failed");
        assert.match(result.notice, /Fixture teardown failed/);
      } else if (scenario === "native-cancel-failure") {
        result = await page.evaluate(async () => {
          fixture.runtime.panes.get("main").flush = async () => {
            throw Error("Fixture unsaved input");
          };
          fixture.nativeReply(() => {
            throw Error("Fixture native cancellation failed");
          });
          window.dispatchEvent(
            new CustomEvent("rsi-native-close", { detail: 9 }),
          );
          for (
            let i = 0;
            i < 10 && fixture.runtime.connection.phase !== "Failed";
            i++
          )
            await fixture.tick();
          await fixture.runtime.connection.settled();
          await fixture.tick();
          return {
            state: fixture.state(),
            nativeCalls: fixture.nativeCalls,
            notice: document.querySelector("#notice").textContent,
          };
        });
        assert.equal(result.state.phase, "Failed");
        assert.deepEqual(result.nativeCalls, [
          { kind: "close", attempt: 9, ready: false },
        ]);
        assert.match(
          result.notice,
          /Draft is not saved: Fixture unsaved input/,
        );
        assert.match(result.notice, /Could not cancel native close/);
      }
      assert.deepEqual(errors, [], scenario);
      results.push({ scenario, ...{ result }, errors });
      await page.screenshot({
        path: join(report, `${name}-connection-${scenario}.png`),
      });
    } finally {
      await page.close();
    }
  }
  await writeFile(
    join(report, `${name}-connection-races.json`),
    JSON.stringify(results, null, 2),
  );
  return results;
}
