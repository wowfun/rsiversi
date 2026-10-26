import assert from 'node:assert/strict';

// Metadata admission and payload selection share one isolated SQLite snapshot.
export function readFactEvidence(database, session) {
  assert(typeof session === 'string' && session.length > 0 && session.length <= 128);
  database.exec('BEGIN');
  try {
    let count = 0, bytes = 0;
    for (const row of database.prepare('SELECT length(CAST(fact_json AS BLOB)) AS bytes FROM facts WHERE session_id=? ORDER BY seq LIMIT 513').iterate(session)) {
      count += 1; bytes += row.bytes;
      assert(count <= 512 && bytes <= 16 * 1024 * 1024, 'live evidence exceeds its bounded Fact selection');
    }
    assert(count > 0, 'live evidence has no Facts');
    const facts = database.prepare('SELECT fact_json FROM facts WHERE session_id=? ORDER BY seq LIMIT 512').all(session).map(row => JSON.parse(row.fact_json));
    return {facts, selection: {source: 'isolated-sqlite-read-transaction', session, count, bytes}};
  } finally { database.exec('ROLLBACK'); }
}
