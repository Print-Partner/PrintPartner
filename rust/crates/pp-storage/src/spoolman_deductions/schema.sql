CREATE TABLE IF NOT EXISTS spoolman_deductions (
  id INTEGER PRIMARY KEY,
  tenant_id TEXT NOT NULL,
  burst TEXT NOT NULL CHECK (length(burst) >= 1),
  link_id TEXT NOT NULL CHECK (length(link_id) >= 1),
  profile_id INTEGER NOT NULL CHECK (profile_id > 0),
  state TEXT NOT NULL CHECK (state IN ('pending', 'sending', 'applied', 'unknown', 'failed', 'skipped', 'dismissed')),
  reason TEXT CHECK (reason IN ('no_grams', 'no_pinned_spool', 'integration_missing', 'integration_disabled', 'integrations_unreadable', 'module_setting_unreadable')),
  units INTEGER NOT NULL CHECK (units > 0),
  integration_id TEXT,
  spool_id INTEGER CHECK (spool_id IS NULL OR spool_id > 0),
  grams REAL CHECK (grams IS NULL OR grams > 0),
  grams_basis TEXT CHECK (grams_basis IS NULL OR grams_basis IN ('estimated', 'measured')),
  created_at TEXT NOT NULL CHECK (length(created_at) >= 1),
  updated_at TEXT NOT NULL CHECK (length(updated_at) >= 1),
  CHECK ((state = 'skipped') = (reason IS NOT NULL)),
  CHECK (state = 'skipped' OR (integration_id IS NOT NULL AND spool_id IS NOT NULL AND grams IS NOT NULL AND grams_basis IS NOT NULL))
);
CREATE INDEX IF NOT EXISTS spoolman_deductions_burst ON spoolman_deductions (tenant_id, burst);
CREATE INDEX IF NOT EXISTS spoolman_deductions_link ON spoolman_deductions (tenant_id, link_id);
CREATE TABLE IF NOT EXISTS spoolman_deduction_units (
  tenant_id TEXT NOT NULL,
  link_id TEXT NOT NULL,
  part_id INTEGER NOT NULL,
  unit_index INTEGER NOT NULL CHECK (unit_index >= 0),
  deduction_id INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, link_id, part_id, unit_index)
);
