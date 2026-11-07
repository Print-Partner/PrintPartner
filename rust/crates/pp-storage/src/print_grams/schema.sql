CREATE TABLE IF NOT EXISTS print_link_grams (
  tenant_id TEXT NOT NULL,
  link_id TEXT NOT NULL CHECK (length(link_id) >= 1),
  grams REAL NOT NULL CHECK (grams > 0),
  units INTEGER NOT NULL CHECK (units > 0),
  basis TEXT NOT NULL CHECK (basis IN ('estimated', 'measured')),
  updated_at TEXT NOT NULL CHECK (length(updated_at) >= 1),
  PRIMARY KEY (tenant_id, link_id)
);
