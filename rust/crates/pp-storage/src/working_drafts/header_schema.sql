CREATE TABLE IF NOT EXISTS plan_draft_headers (
  draft_id INTEGER PRIMARY KEY REFERENCES plan_drafts(id) ON DELETE CASCADE,
  tenant_id TEXT NOT NULL,
  name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 4096),
  order_number TEXT CHECK (order_number IS NULL OR length(order_number) BETWEEN 1 AND 4096),
  customer TEXT CHECK (customer IS NULL OR length(customer) BETWEEN 1 AND 4096),
  due_date TEXT CHECK (due_date IS NULL OR (length(due_date) = 10 AND date(due_date) IS due_date)),
  header_version INTEGER NOT NULL CHECK (header_version BETWEEN 1 AND 2147483647),
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS build_profile_details (
  profile_id INTEGER PRIMARY KEY REFERENCES build_profiles(id) ON DELETE CASCADE,
  tenant_id TEXT NOT NULL,
  customer TEXT CHECK (customer IS NULL OR length(customer) BETWEEN 1 AND 4096),
  due_date TEXT CHECK (due_date IS NULL OR (length(due_date) = 10 AND date(due_date) IS due_date)),
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS plan_draft_header_receipts (
  tenant_id TEXT NOT NULL,
  profile_id INTEGER NOT NULL,
  actor_id TEXT NOT NULL,
  idempotency_key TEXT NOT NULL,
  payload_digest TEXT NOT NULL,
  draft_id INTEGER NOT NULL REFERENCES plan_drafts(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  order_number TEXT,
  customer TEXT,
  due_date TEXT,
  header_version INTEGER NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (tenant_id, profile_id, actor_id, idempotency_key)
);
