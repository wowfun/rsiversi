// One display policy for registered workspaces and path-only conversation frames.
export function workspaceName(path = '', custom) {
  if (custom) return custom;
  const name = path.replace(/[\\/]+$/, '').split(/[\\/]/).pop() || path;
  if (/^[a-z]:$/i.test(name) && path.length > 2) return path;
  return name === 'default-workspace' ? 'Default workspace' : name;
}
