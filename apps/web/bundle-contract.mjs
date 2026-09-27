import { readFileSync } from 'node:fs';

// This build input belongs to the resource admission boundary, not the receipt.
const manifest = JSON.parse(readFileSync(new URL('../../crates/rsi/web-assets/bundle.json', import.meta.url), 'utf8'));
export const bootstrapFiles = Object.freeze(manifest.bootstrap.map(file => Object.freeze(file)));
export const rendererFiles = Object.freeze(manifest.renderers);
const names = bootstrapFiles.filter(file => file.stage !== 'document').map(file => file.name);
// Renderer code is available only through its leased, generation-qualified route.
export const upstreamAssetPattern = `^/(?:${[...names, 'ui-renderers.json'].map(name => name.replaceAll('.', '\\.')).join('|')})(?:\\?|$)`;
