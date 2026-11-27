CREATE TABLE plan_revision_spoolman_colors (
  revision_part_id INTEGER PRIMARY KEY REFERENCES plan_revision_parts(id) ON DELETE CASCADE,
  color_hex TEXT NOT NULL CHECK (
    length(color_hex) = 7 AND substr(color_hex, 1, 1) = '#'
    AND substr(color_hex, 2) NOT GLOB '*[^0-9A-Fa-f]*'
  )
);

CREATE TRIGGER trg_plan_revision_spoolman_colors_immutable_update
BEFORE UPDATE ON plan_revision_spoolman_colors
BEGIN
  SELECT RAISE(ABORT, 'Accepted Plan revision colors are immutable');
END;

CREATE TRIGGER trg_plan_revision_spoolman_colors_immutable_delete
BEFORE DELETE ON plan_revision_spoolman_colors
WHEN EXISTS (SELECT 1 FROM plan_revision_parts WHERE id = OLD.revision_part_id)
BEGIN
  SELECT RAISE(ABORT, 'Accepted Plan revision colors are immutable');
END;
