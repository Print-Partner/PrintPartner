CREATE TABLE IF NOT EXISTS printer_send_queue (
  tenant_id TEXT NOT NULL,
  id TEXT NOT NULL CHECK (length(id) BETWEEN 1 AND 128),
  position INTEGER NOT NULL CHECK (position >= 0),
  filename TEXT NOT NULL CHECK (length(filename) >= 1),
  artifact_path TEXT NOT NULL CHECK (length(artifact_path) >= 1),
  printer_id TEXT NOT NULL CHECK (length(printer_id) >= 1),
  profile_id INTEGER CHECK (profile_id IS NULL OR profile_id > 0),
  match TEXT CHECK (match IS NULL OR match IN ('compatible', 'pinned')),
  wait_for_idle INTEGER NOT NULL CHECK (wait_for_idle IN (0, 1)),
  start INTEGER NOT NULL CHECK (start IN (0, 1)),
  state TEXT NOT NULL CHECK (state IN ('queued', 'sending', 'done', 'error', 'cancelled')),
  checkoff_units TEXT CHECK (checkoff_units IS NULL OR json_valid(checkoff_units)),
  upload_job_id TEXT CHECK (upload_job_id IS NULL OR length(upload_job_id) >= 1),
  error TEXT CHECK (error IS NULL OR length(error) >= 1),
  host_name TEXT CHECK (host_name IS NULL OR length(host_name) >= 1),
  created_at TEXT NOT NULL CHECK (length(created_at) >= 1),
  updated_at TEXT NOT NULL CHECK (length(updated_at) >= 1),
  PRIMARY KEY (tenant_id, id),
  UNIQUE (tenant_id, position)
);

CREATE UNIQUE INDEX IF NOT EXISTS printer_send_queue_one_sending
ON printer_send_queue(tenant_id, printer_id) WHERE state = 'sending';
