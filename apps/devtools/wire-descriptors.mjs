import {openSync, readSync, closeSync} from 'node:fs';

export const validProtocolIdentity = (name, version) => typeof name === 'string'
  && /^[a-z][a-z0-9_.-]{0,127}$/.test(name) && Number.isInteger(version) && version >= 1 && version <= 0xffff_ffff;

export function wireDescriptor(path, expectedName) {
  const file = openSync(path, 'r');
  const buffer = Buffer.alloc(1025);
  let length = 0;
  try {
    while (length < buffer.length) {
      const read = readSync(file, buffer, length, buffer.length - length, null);
      if (!read) break;
      length += read;
    }
  } finally { closeSync(file); }
  if (length > 1024) throw new Error('Wire declaration exceeds capacity');
  const bytes = buffer.subarray(0, length);
  const declaration = JSON.parse(bytes);
  const {name, version} = declaration;
  if (Object.keys(declaration).sort().join(',') !== 'name,version' || !validProtocolIdentity(name, version)
    || (expectedName && name !== expectedName)) throw new Error('Invalid wire declaration');
  return Object.freeze({name, version});
}
