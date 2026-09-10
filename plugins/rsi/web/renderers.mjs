import { createHash } from "node:crypto";
import { readFile, writeFile, rename, stat } from "node:fs/promises";
import { dirname, resolve, isAbsolute, join } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";

const source = dirname(fileURLToPath(import.meta.url));
export async function buildRenderers(output) {
  if (!isAbsolute(output) || !(await stat(output)).isDirectory()) throw new Error("Renderer output must be an existing absolute bundle directory");
  const bytes = await readFile(join(source, "standard.js"));
  const catalog = { format: 1, renderers: [{ id: "rsi.standard", abi: 1, entry: "standard.js",
    files: [{ name: "standard.js", sha256: createHash("sha256").update(bytes).digest("hex") }],
    schemas: [{ name: "rsi.standard.view", version: 1 }], capabilities: ["invoke", "focus"], surfaces: ["root", "pane", "sidebar", "dialog"],
  }] };
  // Each file is replaced atomically; the admission manifest is published last.
  // A filesystem observer between these renames retains the old valid generation.
  for (const [name, body] of [["standard.js", bytes], ["ui-renderers.json", JSON.stringify(catalog)]]) {
    const temporary = join(output, `.${name}.${process.pid}.tmp`);
    await writeFile(temporary, body, { flag: "wx" });
    await rename(temporary, join(output, name));
  }
  return catalog.renderers[0].files[0].sha256;
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [output, watch, ...extra] = process.argv.slice(2);
  if (!output || (watch && watch !== "--watch") || extra.length) throw new Error("Usage: renderers.mjs /absolute/bundle [--watch]");
  if (watch) console.log("Watching standard.js; app.js, mounts.js, worker.js, styles.css, index.html and Worker Rust changes require a full bundle rebuild and application restart.");
  let previous;
  do {
    const bytes = await readFile(join(source, "standard.js"));
    const digest = createHash("sha256").update(bytes).digest("hex");
    if (digest !== previous) {
      const revision = await buildRenderers(output);
      previous = digest;
      console.log(JSON.stringify({ event: "renderers-built", directory: output, sha256: revision }));
    }
    if (watch) await delay(250);
  } while (watch);
}
