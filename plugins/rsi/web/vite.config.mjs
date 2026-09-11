import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import { fileURLToPath } from 'node:url';

const local = path => fileURLToPath(new URL(path, import.meta.url));
export default defineConfig(({command}) => {
  const upstream = process.env.RSI_DEV_UPSTREAM;
  if (command === 'serve' && !upstream) throw new Error('Start development through cargo xtask dev web with an isolated service');
  if (upstream) {
    const url = new URL(upstream);
    if (url.protocol !== 'http:' || !['127.0.0.1','[::1]','localhost'].includes(url.hostname) || url.username || url.password || url.pathname !== '/' || url.search || url.hash) throw new Error('Development upstream must be an explicit loopback HTTP origin');
  }
  return {
    plugins:[react(),{name:'rsi-bootstrap-restart',handleHotUpdate(context){if(/\/(?:app\.js|worker\.js|drafts\.js|mounts\.js|src\/(?:main\.tsx|slots\.tsx|bridge\.ts))$/.test(context.file)){context.server.ws.send({type:'full-reload'});return []}}}],
    resolve:{alias:{'@rsi/dsh-slots':local('./vendor/dsh/slots/index.ts'),'@rsi/dsh-store-contract':local('./vendor/dsh/store-contract.ts')}},
    server:{host:'127.0.0.1',strictPort:true,fs:{strict:true,allow:[local('./')]},
      headers:{'Content-Security-Policy':"default-src 'self'; script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; connect-src 'self' ws://127.0.0.1:*; img-src 'self' blob: data:; worker-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"},
      proxy:upstream ? Object.fromEntries(['/api','^/(?!app\\.js(?:\\?|$)|styles\\.css(?:\\?|$))[^/]+\\.(?:js|wasm|json|css|png)(?:\\?|$)'].map(path=>[path,{target:upstream,changeOrigin:false}])) : undefined},
    build:{target:'es2022',emptyOutDir:false,assetsDir:'',rollupOptions:{external:['/mounts.js','/drafts.js'],output:{inlineDynamicImports:true,entryFileNames:'app.js',assetFileNames:'styles[extname]'}}},
  };
});
