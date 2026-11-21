// The Host still opens the returned name relative to its bound workspace handle.
export function workspaceDocument(root: string, path: string): string | undefined {
  const driveRoot = (value: string) => /^[a-z]:\//i.test(value);
  const separators = (value: string) => value.replace(/^\\\\\?\\([a-z]:[\\/])/i, '$1').replaceAll('\\', '/');
  const windows = driveRoot(separators(root));
  const base = windows ? separators(root) : root;
  const target = windows ? separators(path) : path;
  if (!target || target.startsWith('//') || (!windows && !base.startsWith('/'))) return;
  if (!windows && (/^[a-z]:[\\/]/i.test(path) || path.startsWith('\\'))) return;
  if (windows && (driveRoot(target) && target[0].toLowerCase() !== base[0].toLowerCase() ||
      target.slice(driveRoot(target) ? 2 : 0).includes(':'))) return;
  const baseParts = (windows ? base.slice(2) : base).split('/').filter(Boolean);
  const name = windows && driveRoot(target) ? target.slice(2) : target;
  const parts = name.startsWith('/') ? [] : [...baseParts];
  for (const part of name.split('/')) {
    if (part === '..') parts.pop();
    else if (part && part !== '.') parts.push(part);
  }
  if (parts.length <= baseParts.length || !baseParts.every((part, i) =>
    windows ? part.toLowerCase() === parts[i].toLowerCase() : part === parts[i])) return;
  return parts.slice(baseParts.length).join('/');
}
