import { proxyConfiguration } from './development-proxy.mjs';
import { loopbackAddress } from './development-control.mjs';
import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import { fileURLToPath } from 'node:url';
import { xtermDocumentOverride } from './xterm-document.mjs';
import { desktopBridgeDescriptor } from './wire-descriptors.mjs';

const local = path => fileURLToPath(new URL(path, import.meta.url));
export function documentConfiguration({command}, {supervised = false} = {}) {
  const upstream = process.env.RSI_DEV_UPSTREAM;
  if (command === 'serve' && !upstream && !supervised) throw new Error('Start product development through rsi --watch web with a configured frontend recipe; isolated browser fixtures use fixture-dev web');
  if (upstream) loopbackAddress(upstream);
  return {
    cacheDir: local("./node_modules/.vite"),
    define: {
      __RSI_BUILD_FAMILY__: JSON.stringify(process.env.RSI_BUILD_FAMILY_SHA256 ?? process.env.RSI_DEV_BUILD_FAMILY ?? ""),
      __RSI_DESKTOP_BRIDGE__: JSON.stringify(desktopBridgeDescriptor()),
    },
    plugins:[xtermDocumentOverride(),react(),{name:'rsi-bootstrap-restart',handleHotUpdate(context){if(/\/(?:app\.js|worker\.js|admission\.js|drafts\.js|mounts\.js|src\/(?:main\.tsx|slots\.tsx|bridge\.ts))$/.test(context.file)){context.server.ws.send({type:'full-reload'});return []}}}],
    optimizeDeps:{exclude:['@xterm/xterm']},
    resolve:{alias:{'@rsi/dsh-slots':local('./vendor/dsh/slots/index.ts'),'@rsi/dsh-store-contract':local('./vendor/dsh/store-contract.ts')}},
    server:{host:'127.0.0.1',strictPort:true,fs:{strict:true,allow:[local('./')],deny:['**/.git/**','**/.env*','**/target/**','**/Cargo.toml','**/Cargo.lock','**/*.rs']},
      headers:{'Content-Security-Policy':"default-src 'self'; script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; connect-src 'self' ws://127.0.0.1:*; img-src 'self' blob: data:; worker-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"},
      proxy:upstream ? proxyConfiguration(websocket => ({target:upstream,changeOrigin:false,rewriteWsOrigin:false,ws:websocket})) : undefined},
    build:{target:'es2022',emptyOutDir:false,assetsDir:'',rollupOptions:{external:['/mounts.js','/drafts.js'],output:{inlineDynamicImports:true,entryFileNames:'app.js',assetFileNames:'styles[extname]'}}},
  };
}
export default defineConfig(environment => documentConfiguration(environment));
