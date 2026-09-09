import assert from "node:assert/strict";

// Synthetic document-only responses exercise retention; Rust/HTTP tests own byte validation.
export async function verifyImageDom(page) {
  const result = await page.evaluate(async () => {
    const originalCall = call;
    const originalView = view;
    const originalCreate = URL.createObjectURL;
    const originalRevoke = URL.revokeObjectURL;
    const active = new Set();
    let reads = 0;
    URL.createObjectURL = blob => { const url = originalCreate(blob); active.add(url); return url; };
    URL.revokeObjectURL = url => { active.delete(url); originalRevoke(url); };
    const media = id => ({ id: String(id), mime: "image/png", bytes: 8, width: 1, height: 1 });
    const body = document.createElement("div"); document.body.append(body);
    try {
      call = async () => { reads++; return new Uint8Array(8); };
      view = { media_limits: { preview_objects: 8, preview_bytes: 32 } };
      for (let id = 0; id < 6; id++) { body.replaceChildren(); await previewImage(body, media(id), String(id)); }
      const byteBound = { objects: imageCache.size, bytes: [...imageCache.values()].reduce((sum, entry) => sum + entry.bytes, 0), active: active.size };
      await previewImage(body, media(2), "new-ticket");
      const reuseReads = reads;
      body.replaceChildren(); await previewImage(body, media(6), "6");
      const lru = [...imageCache.keys()].map(key => JSON.parse(key).id);
      clearImages();
      const released = active.size;
      view = { media_limits: { preview_objects: 2, preview_bytes: 1024 } };
      for (let id = 0; id < 5; id++) { body.replaceChildren(); await previewImage(body, media(id), String(id)); }
      const objectBound = imageCache.size;
      let finish;
      call = () => new Promise(resolve => { finish = resolve; });
      const reading = previewImage(body, media(99), "late");
      clearImages(); body.remove(); finish(new Uint8Array(8)); await reading;
      return { byteBound, reuseReads, lru, released, objectBound, afterClose: imageCache.size, active: active.size };
    } finally {
      clearImages(); body.remove(); call = originalCall; view = originalView;
      URL.createObjectURL = originalCreate; URL.revokeObjectURL = originalRevoke;
    }
  });
  assert.deepEqual(result, { byteBound: { objects: 4, bytes: 32, active: 4 }, reuseReads: 6,
    lru: ["4", "5", "2", "6"], released: 0, objectBound: 2, afterClose: 0, active: 0 });
  const retry = await page.evaluate(async () => {
    const pane = panes[0];
    const data = { generation: "images", session: "session", path: "/workspace", draft: "same text",
      unresolved_text: "same text", images: [{ id: "edited" }], unresolved_images: [{ id: "original" }],
      model: { deployment: "test", model: "model" }, transcript: { blocks: [], status: "Ready" }, pending: [], notice: "" };
    pane.render(data, []); pane.input.value = "same text";
    await pane.submit(false);
    const edited = pane.input.value;
    pane.render({ ...data, unresolved_images: null, unresolved_text: null }, []);
    pane.input.value = "same text";
    const action = pane.action;
    pane.action = async () => { pane.imageEdits++; };
    await pane.submit(false);
    pane.action = action;
    return { edited, during: pane.input.value };
  });
  assert.deepEqual(retry, { edited: "same text", during: "same text" });
}
