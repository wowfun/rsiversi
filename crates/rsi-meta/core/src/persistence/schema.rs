pub(super) const STORE_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS store_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
INSERT OR IGNORE INTO store_meta(key, value) VALUES ('token_generation', '0');
INSERT OR IGNORE INTO store_meta(key, value) VALUES ('minimum_event_cursor', '0');
INSERT OR IGNORE INTO store_meta(key, value) VALUES ('latest_event_cursor', '0');
INSERT OR IGNORE INTO store_meta(key, value) VALUES ('latest_graph_revision', '0');
INSERT OR IGNORE INTO store_meta(key, value) VALUES ('latest_composition_event', 'null');
INSERT OR IGNORE INTO store_meta(key, value) VALUES (
    'desired_state',
    '{"manifest_sha256":null,"lock_sha256":null,"applied":false,"last_rejection_code":null,"plugin_restart_requested":false}'
);
CREATE TABLE plugin_state (
    composition_id TEXT NOT NULL,
    instance_id TEXT NOT NULL,
    state_key TEXT NOT NULL,
    version INTEGER NOT NULL CHECK (version > 0),
    value_json TEXT,
    tombstone INTEGER NOT NULL CHECK (tombstone IN (0, 1)),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (composition_id, instance_id, state_key)
);
CREATE TABLE control_event (
    cursor INTEGER PRIMARY KEY AUTOINCREMENT,
    composition_id TEXT NOT NULL,
    command_id TEXT NOT NULL,
    graph_revision INTEGER NOT NULL CHECK (graph_revision >= 0),
    event_json TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE TABLE command_outcome (
    command_id TEXT PRIMARY KEY,
    composition_id TEXT NOT NULL,
    request_hash BLOB NOT NULL,
    operation_kind TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'terminal', 'expired')),
    outcome_json TEXT,
    terminal_classification TEXT,
    pending_json TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    completed_at INTEGER,
    expires_at INTEGER
);
CREATE TABLE apply_journal (
    command_id TEXT PRIMARY KEY REFERENCES command_outcome(command_id),
    composition_id TEXT NOT NULL,
    operation_kind TEXT NOT NULL CHECK (operation_kind IN ('apply', 'install')),
    installed_manifest_path TEXT NOT NULL,
    installed_lock_path TEXT NOT NULL,
    candidate_manifest_hash TEXT NOT NULL,
    candidate_lock_hash TEXT NOT NULL,
    previous_manifest_bytes BLOB,
    previous_lock_bytes BLOB,
    previous_manifest_hash TEXT,
    previous_lock_hash TEXT,
    terminal_graph_revision INTEGER NOT NULL,
    terminal_event_json TEXT NOT NULL,
    terminal_outcome_json TEXT NOT NULL,
    terminal_desired_json TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX control_event_created ON control_event(created_at, cursor);
CREATE INDEX command_outcome_retention
    ON command_outcome(status, completed_at, command_id);
INSERT INTO store_meta(key, value) VALUES ('schema_version', '3');
"#;
