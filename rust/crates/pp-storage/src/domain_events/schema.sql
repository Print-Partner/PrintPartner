CREATE TABLE IF NOT EXISTS domain_events (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  tenant TEXT NOT NULL,
  event_type TEXT NOT NULL CHECK (length(event_type) BETWEEN 1 AND 64),
  schema_version INTEGER NOT NULL CHECK (schema_version >= 1),
  aggregate_type TEXT NOT NULL CHECK (length(aggregate_type) BETWEEN 1 AND 32),
  aggregate_id TEXT NOT NULL CHECK (length(aggregate_id) BETWEEN 1 AND 64),
  aggregate_version INTEGER,
  operation_id TEXT NOT NULL CHECK (length(operation_id) = 32),
  payload_json TEXT NOT NULL CHECK (length(payload_json) <= 2048 AND json_valid(payload_json)),
  occurred_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS domain_events_tenant_seq
ON domain_events(tenant, seq);
