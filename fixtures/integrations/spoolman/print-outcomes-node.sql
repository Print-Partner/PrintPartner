-- One printer checkoff exactly as Node writes it today. Node's only print_jobs
-- writer, appendPrintOutcomes (web/apps/server/src/services/printer-outcomes-store.ts),
-- calls insertPrintJob without status or filamentConsumedG, so the job is
-- 'sent' with no measured grams, then adds one print_job_parts row per unit.
-- Column values follow insertPrintJob and insertPrintJobParts in
-- web/apps/server/src/db/repository.ts. Plan 1, Part 2, two confirmed units.
INSERT INTO print_jobs(id,tenant_id,profile_id,host_integration_id,printer_id,material,filename,status,filament_consumed_g,at,completed_at,link_id)
  VALUES('5b0f6c2e-6f1d-4c34-9d0a-3f5e8f1c2a01','default',1,'mk4','','','included.gcode','sent',NULL,'2026-10-03T00:00:00.000Z',NULL,'link-1');
INSERT INTO print_job_parts(id,tenant_id,job_id,at,profile_id,part_id,unit_index,result,reason,note,host_integration_id,filename,match_key,role,filament_display,link_id)
  VALUES('5b0f6c2e-6f1d-4c34-9d0a-3f5e8f1c2a02','default','5b0f6c2e-6f1d-4c34-9d0a-3f5e8f1c2a01','2026-10-03T00:00:00.000Z',1,2,0,'confirmed',NULL,NULL,'mk4','included.gcode','included','primary',NULL,'link-1'),
        ('5b0f6c2e-6f1d-4c34-9d0a-3f5e8f1c2a03','default','5b0f6c2e-6f1d-4c34-9d0a-3f5e8f1c2a01','2026-10-03T00:00:00.000Z',1,2,1,'confirmed',NULL,NULL,'mk4','included.gcode','included','primary',NULL,'link-1');
