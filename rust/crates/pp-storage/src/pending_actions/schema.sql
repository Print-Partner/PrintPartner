CREATE TABLE IF NOT EXISTS pending_actions (
  seq INTEGER PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL UNIQUE CHECK (length(id) BETWEEN 1 AND 64),
  action_type TEXT NOT NULL CHECK (length(action_type) >= 1),
  plan_id INTEGER NOT NULL,
  label TEXT NOT NULL,
  summary TEXT NOT NULL,
  params_json TEXT NOT NULL CHECK (json_valid(params_json) AND json_type(params_json) = 'object'),
  depends_on_json TEXT NOT NULL CHECK (json_valid(depends_on_json) AND json_type(depends_on_json) = 'array'),
  staged_by TEXT NOT NULL CHECK (length(staged_by) BETWEEN 1 AND 120),
  state TEXT NOT NULL CHECK (state IN ('pending', 'applying', 'applied', 'dismissed', 'unknown')),
  created_at TEXT NOT NULL CHECK (length(created_at) >= 1),
  expires_at TEXT NOT NULL CHECK (expires_at >= created_at),
  claimed_at TEXT,
  settled_at TEXT,
  CHECK ((state IN ('pending', 'applying')) = (settled_at IS NULL)),
  CHECK (state <> 'applying' OR claimed_at IS NOT NULL)
);
CREATE INDEX IF NOT EXISTS pending_actions_open ON pending_actions (tenant_id, state, seq);
