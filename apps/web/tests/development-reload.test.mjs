import assert from 'node:assert/strict';
import {test} from 'node:test';
import {documentReloadRequired} from '../development-reload.mjs';

test('new runtime controllers always replace the document owner', () => {
  for (const path of ['src/runtime/connection.ts', 'src/runtime/session/delivery.ts', 'src\\runtime\\new-controller.js']) {
    assert.equal(documentReloadRequired(path), true, path);
  }
});

test('existing connection, persistence and bootstrap owners cannot hot swap', () => {
  for (const path of ['worker.js','mounts.js','drafts.js','document-connection.js','device-store.js','presentation-store.js','src/main.tsx','src/bridge.ts','src/presentation.tsx','src/resource-owner.mjs','src/session/state.ts','layout-edits.js','src/navigation-device.ts','src/session/composer-state.ts','image-previews.js','code-highlight.js']) {
    assert.equal(documentReloadRequired(path), true, path);
  }
});

test('presentation refresh does not match similarly named or outside modules', () => {
  for (const path of ['src/navigation.tsx','src/settings/General.tsx','src/runtime.module.css','src/runtime-utils.ts','vendor/dsh/renderer/bind.ts','../other/src/runtime/connection.ts']) {
    assert.equal(documentReloadRequired(path), false, path);
  }
});
