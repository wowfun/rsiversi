import { spawnSync } from "node:child_process";
import { mkdir, readdir, copyFile } from "node:fs/promises";
import { dirname, resolve, isAbsolute, join } from "node:path";
import { fileURLToPath } from "node:url";

const source = dirname(fileURLToPath(import.meta.url));
const root = resolve(source, "../../..");
const output = process.argv[2];
if (!output || !isAbsolute(output)) throw new Error("Provide an absolute empty output directory");
const options = process.argv.slice(3);
if (options.length > 1 || options.some(option => option !== "--dev")) {
  throw new Error("Usage: build.mjs /absolute/empty/directory [--dev]");
}
const profile = options.length ? "debug" : "release";
await mkdir(output, { recursive: true });
if ((await readdir(output)).length) throw new Error("Output directory must be empty");
function run(command, args) {
  const result = spawnSync(command, args, { cwd: root, stdio: "inherit", timeout: 600_000 });
  if (result.status !== 0) throw result.error ?? new Error(`${command} failed: ${result.status}`);
}
run("cargo", ["build", "--locked", "-p", "rsi-web", "--target", "wasm32-unknown-unknown",
  ...(profile === "release" ? ["--release"] : [])]);
run(process.env.RSI_WASM_BINDGEN ?? "wasm-bindgen", [
  "--target", "web", "--no-typescript", "--out-dir", output,
  join(root, `target/wasm32-unknown-unknown/${profile}/rsi_web.wasm`),
]);
for (const file of ["index.html", "app.js", "worker.js", "styles.css"]) {
  await copyFile(join(source, file), join(output, file));
}
console.log(JSON.stringify({ event: "web-built", directory: output, profile }));
