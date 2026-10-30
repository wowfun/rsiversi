const owners = new Set([
   'worker.js', 'admission.js', 'document-connection.js',
  'mounts.js', 'drafts.js', 'device-store.js', 'presentation-store.js',
  'presentation-layout.js', 'settings-dialog.js',
  'src/main.tsx', 'src/workbench-root.tsx', 'src/frame-commit.tsx', 'src/slots.tsx', 'src/bridge.ts', 'src/native.ts',
  'src/session/state.ts', 'src/presentation.tsx', 'src/resource-owner.mjs', 'src/resource-hosts.ts',
  'layout-edits.js', 'src/navigation-device.ts', 'src/session/composer-state.ts',
  'disclosure.js', 'notice-state.js', 'code-highlight.js', 'image-previews.js',
]);

// Receives a path relative to the Web root; outside files never select an owner.
export function documentReloadRequired(relativePath) {
  const path = relativePath.replaceAll('\\', '/');
  return path.startsWith('src/runtime/') || owners.has(path);
}
