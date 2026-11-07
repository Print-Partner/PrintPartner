CREATE TABLE IF NOT EXISTS plan_snapshot_headers (
  snapshot_id INTEGER PRIMARY KEY REFERENCES plan_snapshots(id) ON DELETE CASCADE,
  tenant_id TEXT NOT NULL,
  name TEXT NOT NULL,
  order_number TEXT,
  customer TEXT,
  due_date TEXT,
  captured_at TEXT NOT NULL
);
