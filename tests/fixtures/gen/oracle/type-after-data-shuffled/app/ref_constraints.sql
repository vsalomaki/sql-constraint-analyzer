--------------------------------------------------------
--  DDL for Ref Constraints for Table DEPARTMENTS
--------------------------------------------------------

  ALTER TABLE "APP"."DEPARTMENTS" ADD CONSTRAINT "DEPARTMENTS_MANAGER_FK" FOREIGN KEY ("MANAGER_ID")
	  REFERENCES "APP"."EMPLOYEES" ("EMP_ID") DEFERRABLE INITIALLY DEFERRED ENABLE;
  ALTER TABLE "APP"."DEPARTMENTS" ADD CONSTRAINT "DEPARTMENTS_COUNTRY_FK" FOREIGN KEY ("COUNTRY_CODE")
	  REFERENCES "REF"."COUNTRIES" ("COUNTRY_CODE") ENABLE;

--------------------------------------------------------
--  DDL for Ref Constraints for Table EMPLOYEES
--------------------------------------------------------

  ALTER TABLE "APP"."EMPLOYEES" ADD CONSTRAINT "EMPLOYEES_DEPT_FK" FOREIGN KEY ("DEPT_ID")
	  REFERENCES "APP"."DEPARTMENTS" ENABLE;
  ALTER TABLE "APP"."EMPLOYEES" ADD CONSTRAINT "EMPLOYEES_MANAGER_FK" FOREIGN KEY ("MANAGER_ID")
	  REFERENCES "APP"."EMPLOYEES" ("EMP_ID") ENABLE;

--------------------------------------------------------
--  DDL for Ref Constraints for Table PROJECTS
--------------------------------------------------------

  ALTER TABLE "APP"."PROJECTS" ADD CONSTRAINT "PROJECTS_DEPT_FK" FOREIGN KEY ("DEPT_NAME")
	  REFERENCES "APP"."DEPARTMENTS" ("NAME") ENABLE NOVALIDATE;

--------------------------------------------------------
--  DDL for Ref Constraints for Table ASSIGNMENTS
--------------------------------------------------------

  ALTER TABLE "APP"."ASSIGNMENTS" ADD CONSTRAINT "ASSIGNMENTS_EMP_FK" FOREIGN KEY ("EMP_ID")
	  REFERENCES "APP"."EMPLOYEES" ENABLE;
  ALTER TABLE "APP"."ASSIGNMENTS" ADD CONSTRAINT "ASSIGNMENTS_PROJECT_FK" FOREIGN KEY ("PROJECT_CODE")
	  REFERENCES "APP"."PROJECTS" ENABLE;

--------------------------------------------------------
--  DDL for Ref Constraints for Table TIMESHEETS
--------------------------------------------------------

  ALTER TABLE "APP"."TIMESHEETS" ADD CONSTRAINT "TIMESHEETS_ASSIGNMENT_FK" FOREIGN KEY ("EMP_ID","PROJECT_CODE")
	  REFERENCES "APP"."ASSIGNMENTS" ("EMP_ID","PROJECT_CODE") ENABLE;

--------------------------------------------------------
--  DDL for Ref Constraints for Table LEGACY_NOTES
--------------------------------------------------------

  ALTER TABLE "APP"."LEGACY_NOTES" ADD CONSTRAINT "LEGACY_NOTES_EMP_FK" FOREIGN KEY ("EMP_ID")
	  REFERENCES "APP"."EMPLOYEES" DISABLE;

--------------------------------------------------------
--  DDL for Ref Constraints for Table AuditLog
--------------------------------------------------------

  ALTER TABLE "APP"."AuditLog" ADD CONSTRAINT "AUDITLOG_EMP_FK" FOREIGN KEY ("EMP_ID")
	  REFERENCES "APP"."EMPLOYEES" ENABLE;

