import { loopbackAddress } from './development-control.mjs';
import { upstreamAssetPattern } from './bundle-contract.mjs';

export function proxyConfiguration(options) {
  return Object.fromEntries(['^/api/v1/', '^/rsi-renderers/[a-f0-9]{64}/[a-zA-Z0-9._-]+(?:\\?|$)', upstreamAssetPattern].map((path,index) => [path, options(index === 0)]));
}

// Vite's documented configure hook exposes each proxy's original options.
// The proxy retains that object; the target is filled once after backend bind.
export function developmentProxy() {
  let target, configured = false;
  const options = [];
  return {
    get ready() { return configured; },
    configure(upstream) {
      loopbackAddress(upstream);
      if (configured || !options.length) throw new Error('Development proxy was already configured or is unavailable');
      target = upstream;
      for (const option of options) option.target = target;
      configured = true;
    },
    options(websocket = false) {
      return {changeOrigin: false, rewriteWsOrigin: false, ws: websocket, configure(_proxy, option) { options.push(option); if (target) option.target = target; }};
    },
  };
}
