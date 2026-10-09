// CDP strings are UTF-16. The Rust envelope carries the JSON text without decoding it.
export const cdpPacket = value => ({kind:'cdp',value:JSON.stringify(value)});
export function decodeCdp(value) {
  if(typeof value!=='string')throw new Error('invalid private CDP payload');
  const decoded=JSON.parse(value);
  if(!decoded||typeof decoded!=='object'||Array.isArray(decoded))throw new Error('invalid private CDP object');
  return decoded;
}
