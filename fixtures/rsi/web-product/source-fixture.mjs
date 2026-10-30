import {join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {once} from 'node:events';
import {createServer, normalizePath} from '../../../apps/web/node_modules/vite/dist/node/index.js';
import {documentConfiguration} from '../../../apps/web/vite.config.mjs';

// Compile an authored fixture entry against the product's real module graph.
// Production source is never rewritten or evaluated as extracted function text.
export async function sourceFixture({name, source, configure, root = fileURLToPath(new URL('../../../apps/web/', import.meta.url))}) {
  const entry = normalizePath(join(root, `${name}.tsx`));
  const config = documentConfiguration({command:'build'});
  const server = await createServer({...config, root, configFile:false,
    cacheDir:join(root, `node_modules/.vite-${name}`),
    optimizeDeps:{...config.optimizeDeps,entries:[],noDiscovery:true,include:['react','react-dom','react-dom/client','react/jsx-runtime','react/jsx-dev-runtime','use-sync-external-store/shim','use-sync-external-store/shim/with-selector','clsx','@xterm/addon-fit']},
    esbuild:{...config.esbuild,jsx:'automatic'}, server:{...config.server,host:'127.0.0.1',port:0,watch:null},
    plugins:[...(config.plugins??[]),{name,
      resolveId(id) { if(id===`/${name}.tsx`)return entry; },
      load(id) { if(id===entry)return source; },
      configureServer(server) {
        configure?.(server);
        server.middlewares.use(`/${name}.html`,async(_req,res,next)=>{
          try {
          for (const [key,value] of Object.entries(config.server?.headers??{})) res.setHeader(key,value);
          res.setHeader('Content-Type','text/html');
          res.end(await server.transformIndexHtml(`/${name}.html`, `<!doctype html><html><head><meta name="viewport" content="width=device-width,initial-scale=1"></head><body><div id="root"></div><script type="module" src="/${name}.tsx"></script></body></html>`));
          } catch(error) { next(error); }
        });
      },
    }],
  });
  try {
    // Vite 6's listen helper treats port zero as 5173. The public HTTP server
    // retains kernel-assigned ports and still runs Vite's initialization hook.
    const listening=once(server.httpServer,'listening');
    server.httpServer.listen(0,'127.0.0.1');
    await listening;
  }
  catch(error) { await server.close(); throw error; }
  return {url:`http://127.0.0.1:${server.httpServer.address().port}/${name}.html`,close:()=>server.close()};
}
