import assert from "node:assert/strict";
import { png } from "./png.mjs";
import { resources } from "./controls.mjs";

// Real React details and connection-owned image interests. Cache budgets/LRU are
// additionally exercised by apps/web/tests/image-previews.test.mjs.
export async function verifyImageDom(page) {
  await page.evaluate(
    (bytes) => {
      fixture.imageBytes = new Uint8Array(bytes);
      fixture.imageReads = [];
      fixture.imageUrls = new Set();
      const create = URL.createObjectURL.bind(URL),
        revoke = URL.revokeObjectURL.bind(URL);
      URL.createObjectURL = (blob) => {
        const url = create(blob);
        fixture.imageUrls.add(url);
        return url;
      };
      URL.revokeObjectURL = (url) => {
        fixture.imageUrls.delete(url);
        return revoke(url);
      };
      fixture.setReply(
        "read_image",
        (payload) =>
          new Promise((resolve, reject) =>
            fixture.imageReads.push({
              ticket: JSON.parse(payload).ticket,
              resolve,
              reject,
            }),
          ),
      );
      fixture.view.media_limits = {
        preview_objects: 8,
        preview_bytes: 4096,
        preview_concurrency: 2,
      };
      fixture.imagePanel = (id, ticket = id) => ({
        view: String(id.charCodeAt(0) - 96),
        open_revision: "1",
        opening_request: null,
        session: fixture.view.surfaces.main.session,
        pane: "main",
        generation: fixture.view.surfaces.main.generation,
        floating: false,
        reopen: {
          action: "inspect_image",
          media: {
            id: id.repeat(64),
            mime: "image/png",
            bytes: bytes.length,
            width: 1,
            height: 1,
          },
        },
        image_detail: {
          ticket,
          media: {
            id: id.repeat(64),
            mime: "image/png",
            bytes: bytes.length,
            width: 1,
            height: 1,
          },
        },
      });
    },
    [...png(1, 1, [35, 95, 140, 255])],
  );
  await page.evaluate(async () => {
    fixture.view.panels = ["a", "b", "c"].map((id) => fixture.imagePanel(id));
    await fixture.frame();
  });
  await page.waitForFunction(() => fixture.imageReads.length === 2);
  assert.deepEqual(
    await page.evaluate(() => fixture.imageReads.map((read) => read.ticket)),
    ["a", "b"],
  );
  await page.evaluate(() =>
    fixture.imageReads[0].resolve(fixture.imageBytes.slice()),
  );
  await page.waitForFunction(() => fixture.imageReads.length === 3);
  await page.evaluate(async () => {
    fixture.view.panels = fixture.view.panels.filter(
      (panel) => panel.view !== "2",
    );
    await fixture.frame();
    fixture.imageReads[1].resolve(fixture.imageBytes.slice());
    fixture.imageReads[2].resolve(fixture.imageBytes.slice());
  });
  await page.waitForFunction(
    () =>
      [...document.querySelectorAll(".image-preview")].filter(
        (image) => image.naturalWidth === 1,
      ).length === 2,
  );
  assert.equal(
    await page.evaluate(() => fixture.imageUrls.size),
    2,
    "removed interest never creates a URL",
  );
  const url = await page.locator(".image-preview").first().getAttribute("src");
  await page.evaluate(async () => {
    fixture.view.panels[0] = fixture.imagePanel("a", "new-ticket");
    await fixture.frame();
  });
  await page.waitForFunction(
    () => document.querySelector(".image-preview")?.naturalWidth === 1,
  );
  assert.equal(
    await page.locator(".image-preview").first().getAttribute("src"),
    url,
  );
  assert.equal(
    await page.evaluate(() => fixture.imageReads.length),
    3,
    "canonical cached image is reused across tickets",
  );
  await page.evaluate(async () => {
    fixture.view.panels = [fixture.imagePanel("d")];
    await fixture.frame();
  });
  await resources(page);
  await page.waitForFunction(() => fixture.imageReads.length === 4);
  await page.evaluate(() =>
    fixture.imageReads[3].reject(new Error("Fixture read failed")),
  );
  await page.getByRole("button", { name: "Retry image", exact: true }).click();
  await page.waitForFunction(() => fixture.imageReads.length === 5);
  await page.evaluate(() =>
    fixture.imageReads[4].resolve(fixture.imageBytes.slice()),
  );
  await page.waitForFunction(
    () => document.querySelector(".image-preview")?.naturalWidth === 1,
  );
  await page.evaluate(async () => {
    fixture.view.panels = [];
    await fixture.frame();
  });
  const retry = await page.evaluate(async () => {
    const pane = fixture.runtime.panes.get("main");
    const image = (id) => ({
      id: id.repeat(64),
      mime: "image/png",
      bytes: 8,
      width: 1,
      height: 1,
    });
    const results = [];
    try {
      pane.edit("same text", [image("a")]);
      await pane.flush();
      const editor = pane.editor;
      await editor.update((record) =>
        editor.store.freeze(record, {
          kind: "message",
          id: "image-message",
          opaque: "{}",
          text_bytes: 9,
          images: 1,
          references: 0,
        }),
      );
      await editor.update((record) => editor.store.begin(record));
      pane.edit("same text", [image("b")]);
      await pane.flush();
      fixture.setReply("dispatch_submission", async () =>
        JSON.stringify({ status: "complete", receipt: "{}" }),
      );
      await pane.submit("primary");
      results.push(pane.input.value);
      fixture.setReply("prepare_submission", async (payload) =>
        JSON.stringify({
          kind: "message",
          id: "image-next",
          opaque: payload,
          text_bytes: 9,
          images: 1,
          references: 0,
        }),
      );
      fixture.setReply("dispatch_submission", async () => {
        pane.edit("same text", [image("c")]);
        await pane.flush();
        return JSON.stringify({ status: "complete", receipt: "{}" });
      });
      await pane.submit("primary");
      results.push(pane.input.value);
      return results;
    } finally {
      fixture.setReply("prepare_submission");
      fixture.setReply("dispatch_submission");
    }
  });
  assert.deepEqual(retry, ["same text", "same text"]);
  await page.evaluate(async () => {
    await fixture.runtime.connection.fail(new Error("Image owner retired"));
    await fixture.tick();
  });
  assert.equal(
    await page.evaluate(() => fixture.imageUrls.size),
    0,
    "connection retirement releases every canonical URL",
  );
  assert.equal(await page.locator(".image-preview").count(), 0);
  return {
    parallel: true,
    cancelled_interest: true,
    stable_url: true,
    failed_read_retry: true,
    image_only_edit_retained: true,
    retired_urls: 0,
  };
}
