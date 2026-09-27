import { pairedBundle } from './paired-bundle.mjs';
export const paired = await pairedBundle();
process.env.RSI_WEB_BINARY = paired.binary;
process.env.RSI_BINARY = paired.binary;
process.env.RSI_WEB_ASSETS = paired.assets;
