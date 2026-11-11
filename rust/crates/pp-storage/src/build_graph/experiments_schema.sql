CREATE TABLE build_experiments (
  build_id INTEGER PRIMARY KEY REFERENCES build_profiles(id) ON DELETE CASCADE,
  parent_build_id INTEGER NOT NULL REFERENCES build_profiles(id) ON DELETE RESTRICT,
  CHECK (build_id <> parent_build_id)
);
CREATE INDEX idx_build_experiments_parent ON build_experiments(parent_build_id);
