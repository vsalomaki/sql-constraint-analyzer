SET DEFINE OFF
@@ref/tables/COUNTRIES.sql
@@ref/data/COUNTRIES.sql
COMMIT;
GRANT REFERENCES ON "REF"."COUNTRIES" TO "APP";
@@app/tables/DEPARTMENTS.sql
@@app/tables/EMPLOYEES.sql
@@app/tables/PROJECTS.sql
@@app/tables/ASSIGNMENTS.sql
@@app/tables/TIMESHEETS.sql
@@app/tables/LEGACY_NOTES.sql
@@app/tables/AuditLog.sql
@@app/ref_constraints/DEPARTMENTS.sql
@@app/ref_constraints/EMPLOYEES.sql
@@app/ref_constraints/PROJECTS.sql
@@app/ref_constraints/ASSIGNMENTS.sql
@@app/ref_constraints/TIMESHEETS.sql
@@app/ref_constraints/AuditLog.sql
@@app/data/DEPARTMENTS.sql
@@app/data/TIMESHEETS.sql
@@app/data/PROJECTS.sql
@@app/data/EMPLOYEES.sql
COMMIT;
@@app/data/AuditLog.sql
COMMIT;
@@app/data/ASSIGNMENTS.sql
COMMIT;
@@app/data/LEGACY_NOTES.sql
COMMIT;
  ALTER TABLE "APP"."LEGACY_NOTES" ADD CONSTRAINT "LEGACY_NOTES_EMP_FK" FOREIGN KEY ("EMP_ID")
	  REFERENCES "APP"."EMPLOYEES" ENABLE NOVALIDATE;
