CREATE TABLE IF NOT EXISTS printer_tls_pins (
  tenant_id TEXT NOT NULL,
  integration_id TEXT NOT NULL CHECK (length(integration_id) >= 1),
  host TEXT NOT NULL CHECK (length(host) >= 1),
  port INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535),
  sha256 BLOB NOT NULL CHECK (length(sha256) = 32),
  pinned_at TEXT NOT NULL CHECK (length(pinned_at) >= 1),
  PRIMARY KEY (tenant_id, integration_id)
);
