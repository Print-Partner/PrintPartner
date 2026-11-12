INSERT OR IGNORE INTO build_profiles(id,tenant_id,name) VALUES(1,'default','Fixture Build'),(2,'foreign','Foreign Build');
INSERT OR IGNORE INTO projects(id,tenant_id,name,url,source_type,branch) VALUES(1,'default','Base','','local','main');
UPDATE projects SET name='Base' WHERE id=1;
INSERT INTO projects(id,tenant_id,name,url,source_type,branch,local_path,last_synced_at) VALUES(2,'default','Addon','','local','main','<data-dir>/repos/2','2026-11-01T00:00:00.000Z'),(3,'foreign','Secret','','local','main','<data-dir>/repos/3','2026-11-01T00:00:00.000Z');
INSERT OR IGNORE INTO profile_layers(id,tenant_id,profile_id,layer_order,layer_type,project_id) VALUES(1,'default',1,0,'base',1);
INSERT INTO app_settings(tenant_id,key,value) VALUES('default','build_planning.v1.1','{"version":1,"build_id":1,"special_request":"Fixture","requirements":[],"evidence":[],"contributions":[],"compatibility_findings":[],"differences":[],"resolutions":{},"role_filaments":[],"checklist_items":[],"draft_review_blockers":[],"created_at":"2026-11-01T00:00:00.000Z","updated_at":"2026-11-01T00:00:00.000Z"}');
