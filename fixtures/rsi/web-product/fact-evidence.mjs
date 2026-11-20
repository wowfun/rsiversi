import assert from 'node:assert/strict';

// Metadata admission and payload selection share one isolated SQLite snapshot.
export function readFactEvidence(database, session) {
  assert(typeof session === 'string' && session.length > 0 && session.length <= 128);
  database.exec('BEGIN');
  try {
    let count = 0, bytes = 0;
    const from = `FROM facts f LEFT JOIN agent_controls c
      ON c.session_id=f.session_id AND c.seq=f.acceptance_control_seq
      WHERE f.session_id=? ORDER BY f.seq`;
    for (const row of database.prepare(`SELECT length(CAST(f.fact_json AS BLOB)) + coalesce(length(CAST(c.control_json AS BLOB)),0) AS bytes ${from} LIMIT 513`).iterate(session)) {
      count += 1; bytes += row.bytes;
      assert(count <= 512 && bytes <= 16 * 1024 * 1024, 'live evidence exceeds its bounded Fact selection');
    }
    assert(count > 0, 'live evidence has no Facts');
    const facts = [];
    for (const row of database.prepare(`SELECT f.fact_json, f.acceptance_control_seq, c.control_json ${from} LIMIT 512`).iterate(session)) {
      const fact = JSON.parse(row.fact_json);
      if (row.acceptance_control_seq != null) {
        const acceptance = JSON.parse(row.control_json);
        assert(fact.type === 'input_message_claimed' && fact.accepted_control_seq === row.acceptance_control_seq &&
          acceptance?.type === 'message_accepted' && acceptance.seq === fact.accepted_control_seq, 'claim acceptance differs');
        const {source, message_id, content} = acceptance.message;
        delete fact.accepted_control_seq;
        fact.source = {...source, message_id};
        fact.content = content;
      }
      facts.push(fact);
    }
    return {facts, selection: {source: 'isolated-sqlite-read-transaction', session, count, bytes}};
  } finally { database.exec('ROLLBACK'); }
}
