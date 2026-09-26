import assert from 'node:assert/strict';
import {readFile, writeFile, readdir} from 'node:fs/promises';
import {join} from 'node:path';

// Evidence text only; never follow links into files owned outside the fixture.
export async function redactEvidence(directory, key) {
  assert(key.length > 0);
  let redacted = 0, failed = 0;
  async function scan(path) {
    let entries;
    try { entries = await readdir(path, {withFileTypes: true}); } catch { failed++; return; }
    for (const entry of entries) {
      const child = join(path, entry.name);
      if (entry.isDirectory()) await scan(child);
      else if (entry.isFile() && /\.(json|jsonl|log|txt)$/.test(entry.name)) {
        try {
          const text = await readFile(child, 'utf8');
          if (text.includes(key)) { await writeFile(child, text.replaceAll(key, '[REDACTED]')); redacted++; }
        } catch { failed++; }
      }
    }
  }
  await scan(directory);
  if (redacted || failed) throw new Error(`Evidence redaction: ${redacted} files sanitized, ${failed} files inaccessible`);
}

export function requireProviderUsage(facts) {
  const usage = facts.filter(fact => fact.type === 'model_event' && fact.event?.type === 'usage').map(fact => fact.event.usage);
  assert(usage.length > 0, 'Actual provider usage is required');
  for (const entry of usage) {
    assert(entry && Number.isSafeInteger(entry.input_tokens) && entry.input_tokens > 0, 'Invalid provider input usage');
    assert(Number.isSafeInteger(entry.output_tokens) && entry.output_tokens > 0, 'Invalid provider output usage');
  }
  return usage;
}
