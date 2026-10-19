import { selectPairedBundle, verifyPairedBundle } from './paired-bundle.mjs';
import { holdGeneration } from './generation-lease.mjs';
const selected = await selectPairedBundle();
const lease = await holdGeneration(selected.bundle);
export const paired = await verifyPairedBundle(selected).catch(async error=>{
  try {await lease.close();} catch(cleanup) {error.cause=cleanup;error.message+=`; cleanup: ${cleanup.message}`;}
  throw error;
});
process.env.RSI_WEB_BINARY = paired.binary;
process.env.RSI_WEB_ASSETS = paired.assets;
if(process.env.RSI_PAIRED_BUNDLE)process.env.RSI_PAIRED_BUNDLE=selected.bundle;
