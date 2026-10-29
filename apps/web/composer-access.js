const modes = new Set(['read-only', 'workspace-write', 'danger-full-access']);
const storageKey = 'rsi.composer-access';
export function accessMode(value) {
  if (!modes.has(value)) throw new Error('Invalid composer access mode');
  return value;
}
function read(storage) {
  const raw = storage.getItem(storageKey);
  if (!raw) return [];
  if (raw.length > 256 * 1024) throw new Error('Composer access preferences exceed their limit');
  const entries = JSON.parse(raw);
  if (!Array.isArray(entries) || entries.length > 128 || entries.some(entry =>
    !Array.isArray(entry) || entry.length !== 2 || typeof entry[0] !== 'string' || entry[0].length > 2048 || !modes.has(entry[1])))
    throw new Error('Invalid composer access preferences');
  return entries;
}
export function accessKey(namespace, session) {
  if (!Array.isArray(namespace) || namespace.length !== 2 || namespace.some(value => typeof value !== 'string' || value.length > 512) || typeof session !== 'string' || session.length > 256)
    throw new Error('Invalid composer access identity');
  const key = JSON.stringify([...namespace, session]);
  if (key.length > 2048) throw new Error('Composer access identity exceeds its limit');
  return key;
}
export function readAccess(key, storage = localStorage) {
  return read(storage).find(entry => entry[0] === key)?.[1];
}
export function saveAccess(key, value, storage = localStorage) {
  if (typeof key !== 'string' || key.length > 2048) throw new Error('Invalid composer access identity');
  accessMode(value);
  const entries = read(storage).filter(entry => entry[0] !== key);
  entries.push([key, value]);
  storage.setItem(storageKey, JSON.stringify(entries.slice(-128)));
}
