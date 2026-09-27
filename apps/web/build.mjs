import { bootstrapFiles, rendererFiles } from './bundle-contract.mjs';
import { previewDocument } from './preview-document.mjs';
import { buildRenderers } from "./renderers.mjs";
import { spawnSync } from "node:child_process";
import { mkdir, readdir, copyFile, writeFile } from "node:fs/promises";
import { dirname, resolve, isAbsolute, join } from "node:path";
import { fileURLToPath } from "node:url";
import { build } from "vite";

const source = dirname(fileURLToPath(import.meta.url));
const root = resolve(source, "../..");
const output = process.argv[2];
const family = process.env.RSI_BUILD_FAMILY_SHA256;
if (!/^[a-f0-9]{64}$/.test(family ?? '')) throw new Error('Internal asset stage requires a paired producer; run pnpm -C apps/web build');
const target = resolve(process.env.CARGO_TARGET_DIR ?? join(root, 'target'));
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
  "--target-dir", target,
  ...(profile === "release" ? ["--release"] : [])]);
run(process.env.RSI_WASM_BINDGEN ?? "wasm-bindgen", [
  "--target", "web", "--no-typescript", "--remove-name-section", "--out-dir", output,
  ...(profile === "debug" ? ["--no-demangle"] : []),
  join(target, `wasm32-unknown-unknown/${profile}/rsi_web.wasm`),
]);
await build({ root: source, configFile: join(source, "vite.config.mjs"), build: { outDir: output } });
for (const {name: file, stage} of bootstrapFiles.filter(file => ["copy", "worker"].includes(file.stage))) {
  if (stage === 'worker') await writeFile(join(output, file), (await (await import('node:fs/promises')).readFile(join(source, file), 'utf8')).replaceAll('__RSI_BUILD_FAMILY__', family));
  else await copyFile(join(source, file), join(output, file));
}
const builtBootstrap=await build({configFile:false,root:source,logLevel:"error",build:{write:false,target:"es2022",minify:true,lib:{entry:join(source,"preview-bootstrap.js"),formats:["iife"],name:"PreviewBootstrap"},rollupOptions:{output:{inlineDynamicImports:true}}}});
const bootstrap=builtBootstrap[0].output.find(item=>item.type==="chunk").code;
for(const file of bootstrapFiles.filter(file => file.stage === "preview").map(file => file.name))await writeFile(join(output,file),previewDocument(bootstrap));
await buildRenderers(output);
const produced = (await readdir(output)).sort();
const expected = [...bootstrapFiles.map(file => file.name), ...rendererFiles].sort();
if (JSON.stringify(produced) !== JSON.stringify(expected)) throw new Error('Bundle outputs differ from the compiled asset contract');
const compiler = spawnSync("rustc", ["--print", "host-tuple"], { cwd: root, encoding: "utf8" });
if (compiler.status !== 0) throw compiler.error ?? new Error("rustc host lookup failed");
const host = compiler.stdout.trim();
if (!host) throw new Error("rustc did not report its native host");
run("cargo", ["run", "--locked", "-p", "rsi-web-assets", "--example", "check_bundle",
  "--target", host, ...(profile === "release" ? ["--release"] : []), "--", output, "--write-pairing", family]);
console.log(JSON.stringify({ event: "web-built", directory: output, profile }));
