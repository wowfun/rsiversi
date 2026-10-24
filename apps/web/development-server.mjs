import { createServer as createViteServer } from 'vite';
import { fileURLToPath } from 'node:url';
import { resolve, dirname, join } from 'node:path';
import { mkdir, mkdtemp, rm } from 'node:fs/promises';
import { documentConfiguration } from './vite.config.mjs';
import { DevelopmentControl, loopbackAddress } from './development-control.mjs';
import { developmentProxy, proxyConfiguration } from './development-proxy.mjs';


export async function runDevelopmentServer({input = process.stdin, output = process.stdout, createServer = createViteServer} = {}) {
let exitCode = 0;
let server, stopping = false, starting = Promise.resolve(), closing;
let cacheDirectory, cacheRoot;
let resolveStopped;
const stopped = new Promise(resolve => { resolveStopped = resolve; });
function stop(error) {
  stopping = true;
  if (error) { console.error(error.message); exitCode = 1; }
  closing ??= (async () => {
    // Wait for creation/binding to settle before closing the eventual listener.
    await starting.catch(() => {});
    // Vite 6.4.3 scans in the background; keep its resolver alive until drained.
    // The parent still owns the finite graceful/forced settlement deadline.
    await Promise.allSettled(Object.values(server?.environments ?? {}).map(environment => environment.depsOptimizer?.scanProcessing));
    await server?.close().catch(() => { exitCode = 1; });
    if (cacheDirectory && dirname(resolve(cacheDirectory)) === cacheRoot) await rm(cacheDirectory, {recursive:true,force:true}).catch(() => { exitCode = 1; });
    input.destroy(); resolveStopped();
  })();
  return closing;
}
const control = new DevelopmentControl(input, output, error => { void stop(error); });
const signals = ['SIGINT', 'SIGTERM'].map(signal => [signal, () => { exitCode = signal === 'SIGINT' ? 130 : 143; control.close(); }]);
for (const [signal, handler] of signals) process.on(signal, handler);
async function start() {
  const initial = await control.expect('start_frontend');
  delete process.env.RSI_BUILD_FAMILY_SHA256;
  process.env.RSI_DEV_BUILD_FAMILY = initial.family;
  delete process.env.RSI_DEV_UPSTREAM;
  const config = documentConfiguration({command: 'serve', mode: 'development'}, {supervised:true});
  cacheRoot = resolve(config.cacheDir, 'rsiversi');
  await mkdir(cacheRoot, {recursive:true});
  cacheDirectory = await mkdtemp(join(cacheRoot, 'run-'));
  const proxy = developmentProxy();
  let bound, rejectBinding;
  const listening = new Promise((resolve, reject) => { bound = resolve; rejectBinding = reject; });
  listening.catch(() => {});
  const ownership = {
    name: 'rsi-development-control',
    configureServer(vite) {
      vite.middlewares.use((_request, response, next) => {
        if (stopping || !proxy.ready) { response.writeHead(503, {'Content-Type': 'text/plain'}).end('Development backend is preparing or stopping'); return; }
        next();
      });
      vite.httpServer.once('listening', () => {
        const address = vite.httpServer.address();
        if (!address || typeof address === 'string' || address.address !== '127.0.0.1' || !address.port || (initial.port && address.port !== initial.port)) { const error = new Error('Unexpected development listener'); rejectBinding(error); control.close(error); return; }
        bound(address.port === 80 ? 'http://127.0.0.1' : `http://127.0.0.1:${address.port}`);
      });
      vite.httpServer.on('error', error => { rejectBinding(error); control.close(error); });
      vite.httpServer.on('close', () => { if (!stopping) control.close(new Error('Development listener closed; restart dev')); });
      vite.httpServer.prependListener('upgrade', (_request, socket) => { if (stopping || !proxy.ready) socket.destroy(); });
    },
  };
  server = await createServer({...config, root:fileURLToPath(new URL('.', import.meta.url)), cacheDir:cacheDirectory, configFile: false,
    customLogger: {hasWarned: false, info(message) { console.error(message); }, warn(message) { console.error(message); }, warnOnce(message) { console.error(message); }, error(message) { console.error(message); }, clearScreen() {}, hasErrorLogged() { return false; }},
    plugins: [ownership, ...config.plugins],
    server: {...config.server, port: initial.port, strictPort: true,
      proxy: proxyConfiguration(websocket => proxy.options(websocket))},
  });
  if (stopping) return;
  // Vite 6's listen convenience treats zero as its default port. Its public
  // HTTP server initializes Vite before binding and preserves kernel port zero.
  server.httpServer.listen(initial.port, '127.0.0.1');
  const origin = await listening;
  if (stopping) return;
  loopbackAddress(origin);
  const configured = control.expect('configure_proxy', null);
  configured.catch(() => {});
  await control.send({event: 'frontend_bound', origin, desktop_bridge: JSON.parse(config.define.__RSI_DESKTOP_BRIDGE__)});
  const message = await configured;
  proxy.configure(message.upstream);
  await control.send({event: 'proxy_ready'});
}
starting = start();
void starting.catch(error => control.close(error));
await stopped;

for (const [signal, handler] of signals) process.off(signal, handler);
return exitCode;
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) process.exitCode = await runDevelopmentServer();
