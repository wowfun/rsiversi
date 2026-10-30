import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFile, writeFile, mkdir } from "node:fs/promises";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium, firefox } from "playwright";
import { sourceFixture } from "./source-fixture.mjs";
import { verifyFrameCommit } from "./frame-commit.mjs";
import { verifyImageDom } from "./images-dom.mjs";
import { verifyComposer } from "./composer.mjs";
import { verifyComposerBindings } from "./composer-bindings.mjs";
import { verifyConnectionDom } from "./connection-dom.mjs";
import { verifyRendererDom } from "./renderer-dom.mjs";
import { verifyExternalDraftDom } from "./external-drafts-dom.mjs";

// Authored entry against the actual product graph. Synthetic transport ports do
// not establish Worker, HTTP, native-window or live-provider coverage.
export async function verifyDom(browser, root, report, name) {
  await verifyExternalDraftDom(browser, root, report, name);
  const standard = await readFile(join(root, "apps/web/standard.js"), "utf8");
  const offer = {
    revision: "a".repeat(64),
    catalog: {
      format: 1,
      renderers: [
        {
          id: "rsi.standard",
          abi: 1,
          entry: "standard.js",
          files: [
            {
              name: "standard.js",
              sha256: createHash("sha256").update(standard).digest("hex"),
            },
          ],
          schemas: [{ name: "rsi.standard.view", version: 1 }],
          capabilities: ["invoke", "focus"],
          surfaces: ["pane"],
        },
      ],
    },
  };
  const server = await sourceFixture({
    root: join(root, "apps/web"),
    name: "document-fixture",
    source: await readFile(
      new URL("./document-scene.tsx", import.meta.url),
      "utf8",
    ),
    configure(server) {
      server.middlewares.use(
        `/rsi-renderers/${offer.revision}/standard.js`,
        (_req, res) => {
          res.setHeader("Content-Type", "text/javascript");
          res.end(standard);
        },
      );
    },
  });
  try {
    const frames = await verifyFrameCommit(browser, server.url, report, name);
    await writeFile(
      join(report, `${name}-frames-react.json`),
      JSON.stringify(frames, null, 2),
    );
    for (const [scenario, verify] of Object.entries({
      composer: verifyComposer,
      bindings: (page) => verifyComposerBindings(page, report, name),
      images: verifyImageDom,
      renderer: (page) => verifyRendererDom(page, offer, report, name),
    })) {
      const page = await browser.newPage({
          viewport: { width: 1440, height: 900 },
        }),
        errors = [];
      page.on("pageerror", (error) => errors.push(error.message));
      try {
        await page.goto(server.url);
        await page.waitForFunction(() => window.ready);
        await page.evaluate(() => fixture.frame());
        const result = await verify(page);
        assert.deepEqual(errors, [], scenario);
        await writeFile(
          join(report, `${name}-${scenario}-react.json`),
          JSON.stringify(result, null, 2),
        );
      } finally {
        await page.close();
      }
    }
    await verifyConnectionDom(browser, server.url, report, name);
  } finally {
    await server.close();
  }
}
if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  const root = resolve(dirname(fileURLToPath(import.meta.url)), "../../.."),
    report = resolve(process.env.RSI_REPORT_DIR);
  await mkdir(report, { recursive: true });
  for (const [name, engine] of Object.entries({ chromium, firefox })) {
    const browser = await engine.launch();
    try {
      await verifyDom(browser, root, report, name);
      console.log(`${name}: document fixtures passed`);
    } finally {
      await browser.close();
    }
  }
}
