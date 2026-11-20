// IndexedDB namespace shared by device-local records. The caller owns validation
// of the logical endpoint and principal before encoding.
export const idBytes = id => Uint8Array.from(id.match(/../g) ?? [], pair => Number.parseInt(pair,16)).buffer;
export const idHex = buffer => Array.from(new Uint8Array(buffer), byte => byte.toString(16).padStart(2,'0')).join('');

export function storedNamespace(endpoint, principal) {
  return [idBytes(endpoint), idBytes(principal === 'local' ? '' : principal.slice(7))];
}

export function readNamespace(endpoint, principal) {
  if (!(endpoint instanceof ArrayBuffer) || endpoint.byteLength !== 16 ||
      !(principal instanceof ArrayBuffer) || (principal.byteLength !== 0 && principal.byteLength !== 16)) return undefined;
  return [idHex(endpoint), principal.byteLength ? 'device:'+idHex(principal) : 'local'];
}
