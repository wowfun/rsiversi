import {createHash} from 'node:crypto';
import {readFileSync} from 'node:fs';

export const validProtocolIdentity = (name, version) => typeof name === 'string'
  && /^[a-z0-9_.-]{1,128}$/.test(name) && Number.isInteger(version) && version >= 1 && version <= 0xffff_ffff;

// Format 2 commits exactly the bytes hashed by the native build script.
export function wireDescriptor(path, expectedName) {
  const bytes = readFileSync(path);
  if (bytes.length > 16 * 1024 * 1024) throw new Error('Wire snapshot exceeds capacity');
  const snapshot = JSON.parse(bytes);
  const {name, version} = snapshot.contract ?? {};
  if (snapshot.format !== 2 || !validProtocolIdentity(name, version) || (expectedName && name !== expectedName)) throw new Error('Invalid wire snapshot');
  return Object.freeze({name, version, schema:createHash('sha256').update(bytes).digest('hex')});
}
