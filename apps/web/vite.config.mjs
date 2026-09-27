import { upstreamAssetPattern } from './bundle-contract.mjs';
import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import { fileURLToPath } from 'node:url';
import { xtermDocumentOverride } from './xterm-document.mjs';

const local = path => fileURLToPath(new URL(path, import.meta.url));
export default defineConfig(({command}) => {
  const upstream = process.env.RSI_DEV_UPSTREAM;
  if (command === 'serve' && !upstream) throw new Error('Start development through pnpm -C apps/web dev with an isolated service');
  if (upstream) {
    const url = new URL(upstream);
    if (url.protocol !== 'http:' || !['127.0.0.1','[::1]','localhost'].includes(url.hostname) || url.username || url.password || url.pathname !== '/' || url.search || url.hash) throw new Error('Development upstream must be an explicit loopback HTTP origin');
  }
  return {
    cacheDir: local("./node_modules/.vite"),
    define: { __RSI_BUILD_FAMILY__: JSON.stringify(process.env.RSI_BUILD_FAMILY_SHA256 ?? process.env.RSI_DEV_BUILD_FAMILY ?? "") },
    plugins:[xtermDocumentOverride(),react(),{name:'rsi-bootstrap-restart',handleHotUpdate(context){if(/\/(?:app\.js|worker\.js|admission\.js|drafts\.js|mounts\.js|src\/(?:main\.tsx|slots\.tsx|bridge\.ts))$/.test(context.file)){context.server.ws.send({type:'full-reload'});return []}}}],
    optimizeDeps:{exclude:['@xterm/xterm']},
    resolve:{alias:{'@rsi/dsh-slots':local('./vendor/dsh/slots/index.ts'),'@rsi/dsh-store-contract':local('./vendor/dsh/store-contract.ts')}},
    server:{host:'127.0.0.1',strictPort:true,fs:{strict:true,allow:[local('./')],deny:['**/.git/**','**/.env*','**/target/**','**/Cargo.toml','**/Cargo.lock','**/*.rs']},
      headers:{'Content-Security-Policy':"default-src 'self'; script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; connect-src 'self' ws://127.0.0.1:*; img-src 'self' blob: data:; worker-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"},
      proxy:upstream ? Object.fromEntries(['^/api/v1/', '^/rsi-renderers/[a-f0-9]{64}/[a-zA-Z0-9._-]+(?:\\?|$)', upstreamAssetPattern].map(path=>[path,{target:upstream,changeOrigin:false}])) : undefined},
    build:{target:'es2022',emptyOutDir:false,assetsDir:'',rollupOptions:{external:['/mounts.js','/drafts.js'],output:{inlineDynamicImports:true,entryFileNames:'app.js',assetFileNames:'styles[extname]'}}},
  };
});
