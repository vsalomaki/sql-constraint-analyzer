--------------------------------------------------------
--  DDL for Sequence AUDIT_SEQ
--------------------------------------------------------

   CREATE SEQUENCE  "APP"."AUDIT_SEQ"  MINVALUE 1 MAXVALUE 9999999999999999999999999999 INCREMENT BY 1 START WITH 1 CACHE 20 NOORDER  NOCYCLE ;

--------------------------------------------------------
--  DDL for Table DEPARTMENTS
--------------------------------------------------------

  CREATE TABLE "APP"."DEPARTMENTS" 
   (	"DEPT_ID" NUMBER(9,0), 
	"NAME" VARCHAR2(40 BYTE), 
	"MANAGER_ID" NUMBER(9,0), 
	"COUNTRY_CODE" CHAR(3 BYTE), 
	 CONSTRAINT "DEPARTMENTS_PK" PRIMARY KEY ("DEPT_ID")
  USING INDEX PCTFREE 10 INITRANS 2 MAXTRANS 255 COMPUTE STATISTICS 
  TABLESPACE "USERS"  ENABLE
   ) SEGMENT CREATION IMMEDIATE 
  PCTFREE 10 PCTUSED 40 INITRANS 1 MAXTRANS 255 
 NOCOMPRESS LOGGING
  STORAGE(INITIAL 65536 NEXT 1048576 MINEXTENTS 1 MAXEXTENTS 2147483645
  PCTINCREASE 0 FREELISTS 1 FREELIST GROUPS 1
  BUFFER_POOL DEFAULT FLASH_CACHE DEFAULT CELL_FLASH_CACHE DEFAULT)
  TABLESPACE "USERS" ;

  ALTER TABLE "APP"."DEPARTMENTS" MODIFY ("NAME" NOT NULL ENABLE);

  ALTER TABLE "APP"."DEPARTMENTS" ADD CONSTRAINT "DEPARTMENTS_NAME_UK" UNIQUE ("NAME")
  USING INDEX PCTFREE 10 INITRANS 2 MAXTRANS 255 COMPUTE STATISTICS 
  TABLESPACE "USERS"  ENABLE;

--------------------------------------------------------
--  DDL for Table EMPLOYEES
--------------------------------------------------------

  CREATE TABLE "APP"."EMPLOYEES" 
   (	"EMP_ID" NUMBER(9,0), 
	"FIRST_NAME" VARCHAR2(40 BYTE), 
	"LAST_NAME" VARCHAR2(40 BYTE), 
	"EMAIL" VARCHAR2(80 BYTE) NOT NULL ENABLE, 
	"DEPT_ID" NUMBER(9,0), 
	"MANAGER_ID" NUMBER(9,0), 
	"HIRE_DATE" DATE, 
	"STATUS" VARCHAR2(10 BYTE) DEFAULT 'ACTIVE' NOT NULL ENABLE, 
	"CREATED_AT" TIMESTAMP (6) DEFAULT ON NULL SYSTIMESTAMP, 
	 CONSTRAINT "EMPLOYEES_PK" PRIMARY KEY ("EMP_ID")
  USING INDEX PCTFREE 10 INITRANS 2 MAXTRANS 255 COMPUTE STATISTICS 
  TABLESPACE "USERS"  ENABLE
   ) SEGMENT CREATION IMMEDIATE 
  PCTFREE 10 PCTUSED 40 INITRANS 1 MAXTRANS 255 
 NOCOMPRESS LOGGING
  STORAGE(INITIAL 65536 NEXT 1048576 MINEXTENTS 1 MAXEXTENTS 2147483645
  PCTINCREASE 0 FREELISTS 1 FREELIST GROUPS 1
  BUFFER_POOL DEFAULT FLASH_CACHE DEFAULT CELL_FLASH_CACHE DEFAULT)
  TABLESPACE "USERS" ;

  ALTER TABLE "APP"."EMPLOYEES" MODIFY ("LAST_NAME" NOT NULL ENABLE);

  ALTER TABLE "APP"."EMPLOYEES" MODIFY ("HIRE_DATE" NOT NULL ENABLE);

  ALTER TABLE "APP"."EMPLOYEES" ADD ("PHONE" VARCHAR2(20 BYTE));
--------------------------------------------------------
--  DDL for Index EMPLOYEES_EMAIL_UIX
--------------------------------------------------------

  CREATE UNIQUE INDEX "APP"."EMPLOYEES_EMAIL_UIX" ON "APP"."EMPLOYEES" ("EMAIL") 
  PCTFREE 10 INITRANS 2 MAXTRANS 255 COMPUTE STATISTICS 
  TABLESPACE "USERS" ;

--------------------------------------------------------
--  DDL for Table PROJECTS
--------------------------------------------------------

  CREATE TABLE "APP"."PROJECTS" 
   (	"PROJECT_CODE" VARCHAR2(10 BYTE), 
	"NAME" VARCHAR2(80 BYTE), 
	"DEPT_NAME" VARCHAR2(40 BYTE), 
	"EXTERNAL_REF" VARCHAR2(20 BYTE), 
	"START_DATE" DATE, 
	 CONSTRAINT "PROJECTS_PK" PRIMARY KEY ("PROJECT_CODE")
  USING INDEX PCTFREE 10 INITRANS 2 MAXTRANS 255 COMPUTE STATISTICS 
  TABLESPACE "USERS"  ENABLE, 
	 CONSTRAINT "PROJECTS_EXTREF_UK" UNIQUE ("EXTERNAL_REF")
  USING INDEX PCTFREE 10 INITRANS 2 MAXTRANS 255 COMPUTE STATISTICS 
  TABLESPACE "USERS"  ENABLE
   ) SEGMENT CREATION IMMEDIATE 
  PCTFREE 10 PCTUSED 40 INITRANS 1 MAXTRANS 255 
 NOCOMPRESS LOGGING
  STORAGE(INITIAL 65536 NEXT 1048576 MINEXTENTS 1 MAXEXTENTS 2147483645
  PCTINCREASE 0 FREELISTS 1 FREELIST GROUPS 1
  BUFFER_POOL DEFAULT FLASH_CACHE DEFAULT CELL_FLASH_CACHE DEFAULT)
  TABLESPACE "USERS" ;

  ALTER TABLE "APP"."PROJECTS" MODIFY ("NAME" NOT NULL ENABLE);

--------------------------------------------------------
--  DDL for Table ASSIGNMENTS
--------------------------------------------------------

  CREATE TABLE "APP"."ASSIGNMENTS" 
   (	"EMP_ID" NUMBER(9,0), 
	"PROJECT_CODE" VARCHAR2(10 BYTE), 
	"ROLE" VARCHAR2(20 BYTE), 
	"ASSIGNED_ON" DATE, 
	 CONSTRAINT "ASSIGNMENTS_PK" PRIMARY KEY ("EMP_ID","PROJECT_CODE")
  USING INDEX PCTFREE 10 INITRANS 2 MAXTRANS 255 COMPUTE STATISTICS 
  TABLESPACE "USERS"  ENABLE
   ) SEGMENT CREATION IMMEDIATE 
  PCTFREE 10 PCTUSED 40 INITRANS 1 MAXTRANS 255 
 NOCOMPRESS LOGGING
  STORAGE(INITIAL 65536 NEXT 1048576 MINEXTENTS 1 MAXEXTENTS 2147483645
  PCTINCREASE 0 FREELISTS 1 FREELIST GROUPS 1
  BUFFER_POOL DEFAULT FLASH_CACHE DEFAULT CELL_FLASH_CACHE DEFAULT)
  TABLESPACE "USERS" ;

  ALTER TABLE "APP"."ASSIGNMENTS" ADD CONSTRAINT "ASSIGNMENTS_ROLE_UK" UNIQUE ("EMP_ID","ROLE")
  USING INDEX PCTFREE 10 INITRANS 2 MAXTRANS 255 COMPUTE STATISTICS 
  TABLESPACE "USERS"  ENABLE;

--------------------------------------------------------
--  DDL for Table TIMESHEETS
--------------------------------------------------------

  CREATE TABLE "APP"."TIMESHEETS" 
   (	"TS_ID" NUMBER(12,0), 
	"EMP_ID" NUMBER(9,0), 
	"PROJECT_CODE" VARCHAR2(10 BYTE), 
	"WORK_DATE" DATE, 
	"HOURS" NUMBER(4,2), 
	 CONSTRAINT "TIMESHEETS_PK" PRIMARY KEY ("TS_ID")
  USING INDEX PCTFREE 10 INITRANS 2 MAXTRANS 255 COMPUTE STATISTICS 
  TABLESPACE "USERS"  ENABLE
   ) SEGMENT CREATION IMMEDIATE 
  PCTFREE 10 PCTUSED 40 INITRANS 1 MAXTRANS 255 
 NOCOMPRESS LOGGING
  STORAGE(INITIAL 65536 NEXT 1048576 MINEXTENTS 1 MAXEXTENTS 2147483645
  PCTINCREASE 0 FREELISTS 1 FREELIST GROUPS 1
  BUFFER_POOL DEFAULT FLASH_CACHE DEFAULT CELL_FLASH_CACHE DEFAULT)
  TABLESPACE "USERS" ;

  ALTER TABLE "APP"."TIMESHEETS" MODIFY ("WORK_DATE" NOT NULL ENABLE);

  ALTER TABLE "APP"."TIMESHEETS" MODIFY ("HOURS" NOT NULL ENABLE);

  ALTER TABLE "APP"."TIMESHEETS" ADD CONSTRAINT "TIMESHEETS_DAY_UK" UNIQUE ("EMP_ID","PROJECT_CODE","WORK_DATE")
  USING INDEX PCTFREE 10 INITRANS 2 MAXTRANS 255 COMPUTE STATISTICS 
  TABLESPACE "USERS"  ENABLE;

--------------------------------------------------------
--  DDL for Table LEGACY_NOTES
--------------------------------------------------------

  CREATE TABLE "APP"."LEGACY_NOTES" 
   (	"NOTE_ID" NUMBER(9,0), 
	"EMP_ID" NUMBER(9,0), 
	"NOTE_TEXT" VARCHAR2(400 BYTE), 
	 CONSTRAINT "LEGACY_NOTES_PK" PRIMARY KEY ("NOTE_ID")
  USING INDEX PCTFREE 10 INITRANS 2 MAXTRANS 255 COMPUTE STATISTICS 
  TABLESPACE "USERS"  ENABLE
   ) SEGMENT CREATION IMMEDIATE 
  PCTFREE 10 PCTUSED 40 INITRANS 1 MAXTRANS 255 
 NOCOMPRESS LOGGING
  STORAGE(INITIAL 65536 NEXT 1048576 MINEXTENTS 1 MAXEXTENTS 2147483645
  PCTINCREASE 0 FREELISTS 1 FREELIST GROUPS 1
  BUFFER_POOL DEFAULT FLASH_CACHE DEFAULT CELL_FLASH_CACHE DEFAULT)
  TABLESPACE "USERS" ;

--------------------------------------------------------
--  DDL for Table AuditLog
--------------------------------------------------------

  CREATE TABLE "APP"."AuditLog" 
   (	"LOG_ID" NUMBER(12,0), 
	"EMP_ID" NUMBER(9,0), 
	"LOGGED_AT" TIMESTAMP (6), 
	"DETAIL" VARCHAR2(200 BYTE), 
	 CONSTRAINT "AUDITLOG_PK" PRIMARY KEY ("LOG_ID")
  USING INDEX PCTFREE 10 INITRANS 2 MAXTRANS 255 COMPUTE STATISTICS 
  TABLESPACE "USERS"  ENABLE
   ) SEGMENT CREATION IMMEDIATE 
  PCTFREE 10 PCTUSED 40 INITRANS 1 MAXTRANS 255 
 NOCOMPRESS LOGGING
  STORAGE(INITIAL 65536 NEXT 1048576 MINEXTENTS 1 MAXEXTENTS 2147483645
  PCTINCREASE 0 FREELISTS 1 FREELIST GROUPS 1
  BUFFER_POOL DEFAULT FLASH_CACHE DEFAULT CELL_FLASH_CACHE DEFAULT)
  TABLESPACE "USERS" ;

  ALTER TABLE "APP"."AuditLog" MODIFY ("LOGGED_AT" NOT NULL ENABLE);

--------------------------------------------------------
--  DDL for Trigger AUDITLOG_BI
--------------------------------------------------------

  CREATE OR REPLACE EDITIONABLE TRIGGER "APP"."AUDITLOG_BI" 
  BEFORE INSERT ON "APP"."AuditLog" FOR EACH ROW
BEGIN
  NULL;
END;
/
ALTER TRIGGER "APP"."AUDITLOG_BI" ENABLE;

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

REM INSERTING into APP.DEPARTMENTS
SET DEFINE OFF;
/* Rows of APP.DEPARTMENTS; a ';' or -- in here is not SQL */
INSERT ALL
  INTO "APP"."DEPARTMENTS" ("DEPT_ID","NAME","MANAGER_ID","COUNTRY_CODE") VALUES (10,'R&D 0',1000.0,'FI')
SELECT 1 FROM DUAL;
Insert into APP.DEPARTMENTS (DEPT_ID,NAME,MANAGER_ID,COUNTRY_CODE) values (61E0,'Extra dept 1',null,'XX');
INSERT ALL
  INTO "APP"."DEPARTMENTS" ("DEPT_ID",
    "NAME",
    "MANAGER_ID",
    "COUNTRY_CODE")
VALUES (20E0,
    'Myynti & markkinointi 1',
    '01007',
    'SE ')
SELECT 1 FROM DUAL;
insert into app.departments (dept_id,name,manager_id,country_code) values ('062','Myynti & markkinointi 1',null,null);
INSERT ALL
  INTO APP.DEPARTMENTS (DEPT_ID,NAME,MANAGER_ID,COUNTRY_CODE) VALUES (30E0,'Hallinto 2','1014','NO')
SELECT 1 FROM DUAL;
INSERT INTO "APP"."DEPARTMENTS" ("DEPT_ID",
    "NAME",
    "MANAGER_ID",
    "COUNTRY_CODE")
VALUES (30.0,
    'Extra dept 3',
    null,
    null);
INSERT ALL
  INTO "APP"."DEPARTMENTS" ("DEPT_ID","NAME","MANAGER_ID","COUNTRY_CODE") VALUES ('040','Logistiikka 3',null,'DK')
SELECT 1 FROM DUAL;
INSERT INTO "APP"."DEPARTMENTS" ("DEPT_ID","NAME","MANAGER_ID","COUNTRY_CODE") VALUES (null,'Extra dept 4',null,null);
INSERT ALL
  INTO "APP"."DEPARTMENTS" ("DEPT_ID",
    "NAME",
    "MANAGER_ID",
    "COUNTRY_CODE")
VALUES (50E0,
    'Tuotekehitys 4',
    1008,
    null)
SELECT 1 FROM DUAL;
INSERT INTO "APP"."DEPARTMENTS" ("DEPT_ID","NAME","MANAGER_ID","COUNTRY_CODE") VALUES ('065','Extra dept 5',-5E0,null);
-- end of APP.DEPARTMENTS
REM INSERTING into APP.EMPLOYEES
SET DEFINE OFF;
/* Rows of APP.EMPLOYEES; a ';' or -- in here is not SQL */
INSERT ALL
  INTO APP.EMPLOYEES (EMP_ID,
    FIRST_NAME,
    LAST_NAME,
    EMAIL,
    DEPT_ID,
    MANAGER_ID,
    HIRE_DATE,
    STATUS,
    CREATED_AT)
VALUES (1000,
    'Matti',
    'Mäkinen',
    'user0@example.com',
    10E0,
    null,
    to_date('21-NOV-08','DD-MON-RR'),
    'ACTIVE',
    to_timestamp('01-DEC-12 07.40.45.000000000 PM','DD-MON-RR HH.MI.SSXFF AM'))
  INTO "APP"."EMPLOYEES" ("EMP_ID","FIRST_NAME","LAST_NAME","EMAIL","DEPT_ID","MANAGER_ID","HIRE_DATE","CREATED_AT","PHONE") VALUES (1001.0,'Liisa','Mäkinen','user1@example.com','20',1000,to_date('02-MAR-07','DD-MON-RR'),to_timestamp('30-JUL-03 09.59.40.000000000 PM','DD-MON-RR HH.MI.SSXFF AM'),'+358 40 0000001')
SELECT 1 FROM DUAL;
INSERT INTO "APP"."EMPLOYEES" ("EMP_ID","FIRST_NAME","LAST_NAME","EMAIL","DEPT_ID","MANAGER_ID","HIRE_DATE","STATUS") VALUES ('1021','Extra','Row 1','extra1@example.com','-1',null,to_date('06-JAN-14','DD-MON-RR'),'ACTIVE');
INSERT ALL
  INTO app.employees (emp_id,first_name,last_name,email,dept_id,manager_id,hire_date,status,created_at,phone) VALUES (1002,'Jussi','Mäkinen','user2@example.com','030','1000',to_date('23-DEC-12','DD-MON-RR'),'ACTIVE',null,'+358 40 0000002')
SELECT 1 FROM DUAL;
insert into app.employees values (1003E0,'Aino','Mäkinen','user3@example.com','040',1001E0,to_date('03-APR-05','DD-MON-RR'),'ACTIVE',to_timestamp('31-MAY-18 06.35.53.000000000 PM','DD-MON-RR HH.MI.SSXFF AM'),null);
Insert into APP.EMPLOYEES (EMP_ID,FIRST_NAME,LAST_NAME,EMAIL,DEPT_ID,MANAGER_ID,HIRE_DATE,STATUS) values ('1022','Extra','Row 2','extra2@example.com',null,'-2',to_date('28-JUN-17','DD-MON-RR'),'ACTIVE');
INSERT ALL
  INTO "APP"."EMPLOYEES" ("EMP_ID",
    "FIRST_NAME",
    "LAST_NAME",
    "EMAIL",
    "DEPT_ID",
    "MANAGER_ID",
    "HIRE_DATE",
    "STATUS",
    "CREATED_AT",
    "PHONE")
VALUES ('1004',
    'Pekka',
    'Mäkinen',
    'user4@example.com',
    50,
    '01001',
    to_date('11-DEC-95','DD-MON-RR'),
    'ACTIVE',
    to_timestamp('03-JUL-00 12.49.03.000000000 AM','DD-MON-RR HH.MI.SSXFF AM'),
    '+358 40 0000004')
  INTO APP.EMPLOYEES (EMP_ID,
    FIRST_NAME,
    LAST_NAME,
    EMAIL,
    DEPT_ID,
    MANAGER_ID,
    HIRE_DATE,
    CREATED_AT,
    PHONE)
VALUES (1005,
    'Sanna',
    'Mäkinen',
    'user5@example.com',
    10E0,
    1005E0,
    to_date('18-SEP-12','DD-MON-RR'),
    to_timestamp('26-SEP-22 02.29.02.000000000 AM','DD-MON-RR HH.MI.SSXFF AM'),
    '+358 40 0000005')
SELECT 1 FROM DUAL;
INSERT INTO "APP"."EMPLOYEES" ("EMP_ID",
    "FIRST_NAME",
    "LAST_NAME",
    "EMAIL",
    "DEPT_ID",
    "MANAGER_ID",
    "HIRE_DATE",
    "STATUS")
VALUES ('1005',
    'Extra',
    'Row 3',
    'extra3@example.com',
    null,
    null,
    to_date('03-FEB-14','DD-MON-RR'),
    'ACTIVE');
INSERT ALL
  INTO "APP"."EMPLOYEES" ("EMP_ID","FIRST_NAME","LAST_NAME","EMAIL","DEPT_ID","MANAGER_ID","HIRE_DATE","STATUS","CREATED_AT") VALUES (1006E0,'Jörg','Mäkinen','user6@example.com','20','01002',to_date('31-DEC-96','DD-MON-RR'),'ACTIVE',to_timestamp('30-APR-21 08.49.39.000000000 PM','DD-MON-RR HH.MI.SSXFF AM'))
  INTO APP.EMPLOYEES (EMP_ID,FIRST_NAME,LAST_NAME,EMAIL,DEPT_ID,MANAGER_ID,HIRE_DATE,STATUS,CREATED_AT,PHONE) VALUES ('1007','Céline','Mäkinen','user7@example.com',30E0,'1002',to_date('25-NOV-24','DD-MON-RR'),'ACTIVE',to_timestamp('11-FEB-14 08.15.09.000000000 PM','DD-MON-RR HH.MI.SSXFF AM'),'+358 40 0000007')
SELECT 1 FROM DUAL;
INSERT INTO "APP"."EMPLOYEES" ("EMP_ID",
    "FIRST_NAME",
    "LAST_NAME",
    "EMAIL",
    "DEPT_ID",
    "MANAGER_ID",
    "HIRE_DATE",
    "STATUS")
VALUES ('01024',
    'Extra',
    'Row 4',
    'user7@example.com',
    null,
    null,
    to_date('04-MAR-11','DD-MON-RR'),
    'ACTIVE');
INSERT ALL
  INTO APP.EMPLOYEES (EMP_ID,FIRST_NAME,LAST_NAME,EMAIL,DEPT_ID,MANAGER_ID,HIRE_DATE,STATUS,CREATED_AT,PHONE) VALUES (1008E0,'José','Mäkinen','user8@example.com',null,1002E0,to_date('09-OCT-07','DD-MON-RR'),'ACTIVE',null,'+358 40 0000008')
  INTO "APP"."EMPLOYEES" ("EMP_ID","FIRST_NAME","LAST_NAME","EMAIL","DEPT_ID","MANAGER_ID","HIRE_DATE","CREATED_AT") VALUES ('01009','Åsa','Mäkinen','user9@example.com',50E0,1003.0,to_date('14-MAY-20','DD-MON-RR'),to_timestamp('16-DEC-00 12.46.30.000000000 AM','DD-MON-RR HH.MI.SSXFF AM'))
SELECT 1 FROM DUAL;
Insert into APP.EMPLOYEES (EMP_ID,
    FIRST_NAME,
    LAST_NAME,
    EMAIL,
    DEPT_ID,
    MANAGER_ID,
    HIRE_DATE,
    STATUS)
values (1025,
    'Extra',
    'Row 5',
    null,
    null,
    null,
    to_date('31-MAR-16','DD-MON-RR'),
    'ACTIVE');
insert into app.employees
values (1010,
    'Matti',
    'Virtanen',
    'user10@example.com',
    10E0,
    '1003',
    to_date('14-FEB-11','DD-MON-RR'),
    'ACTIVE',
    to_timestamp('09-MAR-00 03.48.55.000000000 PM','DD-MON-RR HH.MI.SSXFF AM'),
    '+358 40 0000010');
INSERT INTO "APP"."EMPLOYEES" ("EMP_ID",
    "FIRST_NAME",
    "LAST_NAME",
    "EMAIL",
    "DEPT_ID",
    "MANAGER_ID",
    "HIRE_DATE",
    "STATUS")
VALUES (1026,
    'Extra',
    'Row 6',
    '',
    null,
    null,
    to_date('17-DEC-12','DD-MON-RR'),
    'ACTIVE');
INSERT INTO "APP"."EMPLOYEES" ("EMP_ID",
    "FIRST_NAME",
    "LAST_NAME",
    "EMAIL",
    "DEPT_ID",
    "MANAGER_ID",
    "HIRE_DATE",
    "STATUS")
VALUES (1027,
    'Extra',
    'Row 7',
    'extra7@example.com',
    null,
    '1011',
    to_date('21-OCT-12','DD-MON-RR'),
    'ACTIVE');
INSERT ALL
  INTO app.employees (emp_id,first_name,last_name,email,dept_id,manager_id,hire_date,status,created_at,phone) VALUES ('1011','Liisa','Virtanen','user11@example.com',20E0,1003.0,to_date('11-JAN-95','DD-MON-RR'),'ACTIVE',to_timestamp('11-JUL-16 12.23.49.000000000 AM','DD-MON-RR HH.MI.SSXFF AM'),'+358 40 0000011')
  INTO "APP"."EMPLOYEES" ("EMP_ID",
    "FIRST_NAME",
    "LAST_NAME",
    "EMAIL",
    "DEPT_ID",
    "MANAGER_ID",
    "HIRE_DATE",
    "STATUS",
    "CREATED_AT")
VALUES (1012,
    'Jussi',
    'Virtanen',
    'user12@example.com',
    '30',
    '1004',
    to_date('20-APR-98','DD-MON-RR'),
    'ACTIVE',
    to_timestamp('09-AUG-18 05.10.34.000000000 AM','DD-MON-RR HH.MI.SSXFF AM'))
SELECT 1 FROM DUAL;
INSERT INTO "APP"."EMPLOYEES" ("EMP_ID","FIRST_NAME","LAST_NAME","EMAIL_ADDR","DEPT_ID","MANAGER_ID","HIRE_DATE","STATUS") VALUES (1028.0,'Extra','Row 8','extra8@example.com',null,null,to_date('06-MAR-10','DD-MON-RR'),'ACTIVE');
INSERT ALL
  INTO "APP"."EMPLOYEES" ("EMP_ID",
    "FIRST_NAME",
    "LAST_NAME",
    "EMAIL",
    "DEPT_ID",
    "MANAGER_ID",
    "HIRE_DATE",
    "CREATED_AT",
    "PHONE")
VALUES ('01013',
    'Aino',
    'Virtanen',
    'user13@example.com',
    40E0,
    1004.0,
    to_date('16-FEB-10','DD-MON-RR'),
    to_timestamp('23-JUL-16 11.13.26.000000000 AM','DD-MON-RR HH.MI.SSXFF AM'),
    '+358 40 0000013')
  INTO app.employees (emp_id,
    first_name,
    last_name,
    email,
    dept_id,
    manager_id,
    hire_date,
    status,
    created_at,
    phone)
VALUES (1014E0,
    'Pekka',
    'Virtanen',
    'user14@example.com',
    50.0,
    1004.0,
    to_date('25-AUG-97','DD-MON-RR'),
    'ACTIVE',
    null,
    '+358 40 0000014')
SELECT 1 FROM DUAL;
Insert into APP.EMPLOYEES (EMP_ID,
    FIRST_NAME,
    LAST_NAME,
    EMAIL,
    DEPT_ID,
    MANAGER_ID,
    HIRE_DATE,
    STATUS)
values ('01029',
    'Extra',
    'Row 9',
    'extra9@example.com',
    null,
    null,
    to_date('06-AUG-16','DD-MON-RR'));
INSERT ALL
  INTO app.employees (emp_id,first_name,last_name,email,dept_id,manager_id,hire_date,status,created_at) VALUES (1015,'Sanna','Virtanen','user15@example.com',10,1005.0,to_date('01-AUG-07','DD-MON-RR'),'ACTIVE',to_timestamp('16-MAR-21 11.25.50.000000000 AM','DD-MON-RR HH.MI.SSXFF AM'))
  INTO app.employees (emp_id,first_name,last_name,email,dept_id,manager_id,hire_date,status,created_at,phone) VALUES ('01016','Jörg','Virtanen','user16@example.com',20.0,1005E0,to_date('27-MAR-99','DD-MON-RR'),'ACTIVE',to_timestamp('05-DEC-00 06.10.04.000000000 AM','DD-MON-RR HH.MI.SSXFF AM'),'+358 40 0000016')
SELECT 1 FROM DUAL;
insert into app.employees_archive (emp_id,
    first_name,
    last_name,
    email,
    dept_id,
    manager_id,
    hire_date,
    status)
values ('1030',
    'Extra',
    'Row 10',
    'extra10@example.com',
    null,
    null,
    to_date('18-MAY-14','DD-MON-RR'),
    'ACTIVE');
insert into app.employees
values (1017.0,
    'Céline',
    'Virtanen',
    'user17@example.com',
    null,
    '1005',
    to_date('24-JUN-07','DD-MON-RR'),
    'ACTIVE',
    to_timestamp('10-JUN-07 09.23.12.000000000 AM','DD-MON-RR HH.MI.SSXFF AM'),
    '+358 40 0000017');
INSERT ALL
  INTO "APP"."EMPLOYEES" ("EMP_ID","FIRST_NAME","LAST_NAME","EMAIL","DEPT_ID","MANAGER_ID","HIRE_DATE","STATUS","CREATED_AT") VALUES ('1018','José','Virtanen','user18@example.com','40',1018.0,to_date('17-DEC-23','DD-MON-RR'),'ACTIVE',to_timestamp('15-MAR-12 12.34.05.000000000 PM','DD-MON-RR HH.MI.SSXFF AM'))
SELECT 1 FROM DUAL;
Insert into APP.EMPLOYEES (EMP_ID,
    FIRST_NAME,
    LAST_NAME,
    EMAIL,
    DEPT_ID,
    MANAGER_ID,
    HIRE_DATE,
    STATUS)
values (1031,
    'Extra',
    'Row 11',
    'extra11@example.com',
    null,
    1019.0,
    to_date('08-JUL-12','DD-MON-RR'),
    'ACTIVE');
INSERT ALL
  INTO "APP"."EMPLOYEES" ("EMP_ID","FIRST_NAME","LAST_NAME","EMAIL","DEPT_ID","MANAGER_ID","HIRE_DATE","STATUS","CREATED_AT","PHONE") VALUES ('01019','Åsa','Virtanen','user19@example.com','50',1006E0,to_date('25-OCT-16','DD-MON-RR'),'LEFT',to_timestamp('07-APR-11 11.44.00.000000000 AM','DD-MON-RR HH.MI.SSXFF AM'),'+358 40 0000019')
SELECT 1 FROM DUAL;
-- end of APP.EMPLOYEES
REM INSERTING into APP.PROJECTS
SET DEFINE OFF;
/* Rows of APP.PROJECTS; a ';' or -- in here is not SQL */
INSERT ALL
  INTO APP.PROJECTS (PROJECT_CODE,NAME,DEPT_NAME,EXTERNAL_REF,START_DATE) VALUES ('P00000','Projekti 0','R&D 0','EXT-0',to_date('06-MAR-23','DD-MON-RR'))
  INTO app.projects (project_code,name,dept_name,external_ref,start_date) VALUES ('P00001','Customer''s portal 1','Myynti & markkinointi 1',null,to_date('21-APR-01','DD-MON-RR'))
SELECT 1 FROM DUAL;
insert into app.projects (project_code,
    name,
    dept_name,
    external_ref)
values ('X00001',
    'Extra project 1',
    'Lakkautettu 1',
    null);
INSERT ALL
  INTO APP.PROJECTS (PROJECT_CODE,NAME,DEPT_NAME,EXTERNAL_REF,START_DATE) VALUES ('P00002','Varasto € 2','Hallinto 2','EXT-2',to_date('01-SEP-11','DD-MON-RR'))
SELECT 1 FROM DUAL;
Insert into APP.PROJECTS (PROJECT_CODE,NAME,DEPT_NAME,EXTERNAL_REF) values ('P00002','Extra project 2',null,null);
INSERT ALL
  INTO "APP"."PROJECTS" ("PROJECT_CODE",
    "NAME",
    "DEPT_NAME",
    "EXTERNAL_REF",
    "START_DATE")
VALUES ('P00003',
    'Projekti 3',
    'Logistiikka 3',
    'EXT-3',
    to_date('24-MAY-22','DD-MON-RR'))
SELECT 1 FROM DUAL;
insert into app.projects (project_code,
    name,
    dept_name,
    external_ref)
values ('X00003',
    'Extra project 3',
    null,
    'EXT-3');
INSERT ALL
  INTO APP.PROJECTS (PROJECT_CODE,NAME,DEPT_NAME,EXTERNAL_REF,START_DATE) VALUES ('P00004','Projekti 4','Tuotekehitys 4','EXT-4',to_date('01-JAN-12','DD-MON-RR'))
SELECT 1 FROM DUAL;
-- end of APP.PROJECTS
REM INSERTING into APP.ASSIGNMENTS
SET DEFINE OFF;
/* Rows of APP.ASSIGNMENTS; a ';' or -- in here is not SQL */
INSERT ALL
  INTO app.assignments (emp_id,project_code,role,assigned_on) VALUES (1000E0,'P00000','LEAD',to_date('30-APR-01','DD-MON-RR'))
  INTO "APP"."ASSIGNMENTS" ("EMP_ID",
    "PROJECT_CODE",
    "ROLE",
    "ASSIGNED_ON")
VALUES (1000E0,
    'P00001',
    null,
    to_date('05-MAY-19','DD-MON-RR'))
  INTO app.assignments (emp_id,
    project_code,
    role,
    assigned_on)
VALUES ('1001',
    'P00001',
    'DEV',
    to_date('19-SEP-19','DD-MON-RR'))
  INTO app.assignments (emp_id,project_code,role,assigned_on) VALUES (1001.0,'P00002','QA',to_date('14-AUG-19','DD-MON-RR'))
SELECT 1 FROM DUAL;
INSERT ALL
  INTO app.assignments (emp_id,
    project_code,
    role,
    assigned_on)
VALUES ('1002',
    'P00002',
    'DEV',
    to_date('25-NOV-22','DD-MON-RR'))
  INTO "APP"."ASSIGNMENTS" ("EMP_ID",
    "PROJECT_CODE",
    "ROLE",
    "ASSIGNED_ON")
VALUES (1002,
    'P00003',
    'QA',
    to_date('29-APR-19','DD-MON-RR'))
  INTO "APP"."ASSIGNMENTS" ("EMP_ID","PROJECT_CODE","ROLE","ASSIGNED_ON") VALUES (1003,'P00003','DEV',to_date('06-JUN-23','DD-MON-RR'))
SELECT 1 FROM DUAL;
INSERT ALL
  INTO app.assignments (emp_id,project_code,role,assigned_on) VALUES (1003E0,'P00004',null,to_date('28-NOV-08','DD-MON-RR'))
  INTO "APP"."ASSIGNMENTS" ("EMP_ID","PROJECT_CODE","ROLE","ASSIGNED_ON") VALUES ('01004','P00004','DEV',to_date('11-OCT-01','DD-MON-RR'))
  INTO APP.ASSIGNMENTS (EMP_ID,PROJECT_CODE,ROLE,ASSIGNED_ON) VALUES ('1004','P00000',null,to_date('24-FEB-22','DD-MON-RR'))
SELECT 1 FROM DUAL;
insert into app.assignments (emp_id,
    project_code,
    role)
values ('1004',
    'P00001',
    null);
INSERT ALL
  INTO app.assignments (emp_id,project_code,role,assigned_on) VALUES (1005E0,'P00000','LEAD',to_date('12-JAN-15','DD-MON-RR'))
  INTO "APP"."ASSIGNMENTS" ("EMP_ID",
    "PROJECT_CODE",
    "ROLE",
    "ASSIGNED_ON")
VALUES (1005.0,
    'P00001',
    'QA',
    to_date('23-MAR-21','DD-MON-RR'))
  INTO "APP"."ASSIGNMENTS" ("EMP_ID","PROJECT_CODE","ROLE","ASSIGNED_ON") VALUES ('01006','P00001','DEV',to_date('25-AUG-01','DD-MON-RR'))
  INTO "APP"."ASSIGNMENTS" ("EMP_ID",
    "PROJECT_CODE",
    "ROLE",
    "ASSIGNED_ON")
VALUES (1006.0,
    'P00002',
    null,
    to_date('05-NOV-06','DD-MON-RR'))
SELECT 1 FROM DUAL;
INSERT ALL
  INTO "APP"."ASSIGNMENTS" ("EMP_ID","PROJECT_CODE","ROLE","ASSIGNED_ON") VALUES (1007E0,'P00002','DEV',to_date('24-MAR-22','DD-MON-RR'))
  INTO "APP"."ASSIGNMENTS" ("EMP_ID",
    "PROJECT_CODE",
    "ROLE",
    "ASSIGNED_ON")
VALUES ('01007',
    'P00003',
    'QA',
    to_date('18-JAN-18','DD-MON-RR'))
  INTO "APP"."ASSIGNMENTS" ("EMP_ID",
    "PROJECT_CODE",
    "ROLE",
    "ASSIGNED_ON")
VALUES (1008E0,
    'P00003',
    'DEV',
    to_date('22-JAN-01','DD-MON-RR'))
SELECT 1 FROM DUAL;
INSERT ALL
  INTO APP.ASSIGNMENTS (EMP_ID,PROJECT_CODE,ROLE,ASSIGNED_ON) VALUES (1008E0,'P00004','QA',to_date('02-MAR-20','DD-MON-RR'))
SELECT 1 FROM DUAL;
insert into app.assignments (emp_id,
    project_code,
    role)
values ('01008',
    'P00003',
    'OPS');
INSERT ALL
  INTO app.assignments (emp_id,project_code,role,assigned_on) VALUES (1009.0,'P00004','DEV',to_date('15-MAR-12','DD-MON-RR'))
  INTO app.assignments (emp_id,
    project_code,
    role,
    assigned_on)
VALUES (1009.0,
    'P00000',
    null,
    to_date('27-JUL-15','DD-MON-RR'))
SELECT 1 FROM DUAL;
INSERT ALL
  INTO app.assignments (emp_id,
    project_code,
    role,
    assigned_on)
VALUES (1010E0,
    'P00000',
    'LEAD',
    to_date('25-OCT-02','DD-MON-RR'))
  INTO "APP"."ASSIGNMENTS" ("EMP_ID","PROJECT_CODE","ROLE","ASSIGNED_ON") VALUES (1010E0,'P00001','QA',to_date('01-JUN-22','DD-MON-RR'))
  INTO app.assignments (emp_id,
    project_code,
    role,
    assigned_on)
VALUES ('01011',
    'P00001',
    'DEV',
    to_date('13-MAR-18','DD-MON-RR'))
SELECT 1 FROM DUAL;
INSERT ALL
  INTO "APP"."ASSIGNMENTS" ("EMP_ID",
    "PROJECT_CODE",
    "ROLE",
    "ASSIGNED_ON")
VALUES (1011.0,
    'P00002',
    'QA',
    to_date('19-MAR-09','DD-MON-RR'))
  INTO "APP"."ASSIGNMENTS" ("EMP_ID",
    "PROJECT_CODE",
    "ROLE",
    "ASSIGNED_ON")
VALUES (1012.0,
    'P00002',
    'DEV',
    to_date('23-NOV-13','DD-MON-RR'))
SELECT 1 FROM DUAL;
INSERT ALL
  INTO "APP"."ASSIGNMENTS" ("EMP_ID","PROJECT_CODE","ROLE","ASSIGNED_ON") VALUES ('01012','P00003',null,to_date('14-MAY-01','DD-MON-RR'))
SELECT 1 FROM DUAL;
insert into app.assignments (emp_id,project_code,role) values (-3.0,'P00002','DEV');
INSERT ALL
  INTO APP.ASSIGNMENTS (EMP_ID,
    PROJECT_CODE,
    ROLE,
    ASSIGNED_ON)
VALUES (1013E0,
    'P00003',
    'DEV',
    to_date('27-DEC-07','DD-MON-RR'))
  INTO "APP"."ASSIGNMENTS" ("EMP_ID","PROJECT_CODE","ROLE","ASSIGNED_ON") VALUES (1013E0,'P00004','QA',to_date('09-DEC-09','DD-MON-RR'))
  INTO APP.ASSIGNMENTS (EMP_ID,
    PROJECT_CODE,
    ROLE,
    ASSIGNED_ON)
VALUES (1014E0,
    'P00004',
    'DEV',
    to_date('23-OCT-05','DD-MON-RR'))
  INTO "APP"."ASSIGNMENTS" ("EMP_ID","PROJECT_CODE","ROLE","ASSIGNED_ON") VALUES (1014,'P00000','QA',to_date('26-NOV-09','DD-MON-RR'))
SELECT 1 FROM DUAL;
INSERT ALL
  INTO app.assignments (emp_id,project_code,role,assigned_on) VALUES ('01015','P00000','LEAD',to_date('02-JUL-01','DD-MON-RR'))
  INTO "APP"."ASSIGNMENTS" ("EMP_ID",
    "PROJECT_CODE",
    "ROLE",
    "ASSIGNED_ON")
VALUES ('1015',
    'P00001',
    null,
    to_date('18-DEC-11','DD-MON-RR'))
  INTO "APP"."ASSIGNMENTS" ("EMP_ID",
    "PROJECT_CODE",
    "ROLE",
    "ASSIGNED_ON")
VALUES (1016.0,
    'P00001',
    'DEV',
    to_date('18-MAY-12','DD-MON-RR'))
SELECT 1 FROM DUAL;
INSERT ALL
  INTO APP.ASSIGNMENTS (EMP_ID,PROJECT_CODE,ROLE,ASSIGNED_ON) VALUES ('1016','P00002','QA',to_date('19-DEC-21','DD-MON-RR'))
SELECT 1 FROM DUAL;
insert into app.assignments (emp_id,
    project_code,
    role)
values ('1016',
    'NOPE4',
    'R4');
INSERT ALL
  INTO APP.ASSIGNMENTS (EMP_ID,
    PROJECT_CODE,
    ROLE,
    ASSIGNED_ON)
VALUES (1017,
    'P00002',
    'DEV',
    to_date('26-NOV-04','DD-MON-RR'))
  INTO APP.ASSIGNMENTS (EMP_ID,PROJECT_CODE,ROLE,ASSIGNED_ON) VALUES ('01017','P00003','QA',to_date('12-SEP-17','DD-MON-RR'))
  INTO "APP"."ASSIGNMENTS" ("EMP_ID",
    "PROJECT_CODE",
    "ROLE",
    "ASSIGNED_ON")
VALUES (1018.0,
    'P00003',
    'DEV',
    to_date('31-JUL-08','DD-MON-RR'))
SELECT 1 FROM DUAL;
INSERT ALL
  INTO app.assignments (emp_id,
    project_code,
    role,
    assigned_on)
VALUES (1018,
    'P00004',
    null,
    to_date('03-MAR-22','DD-MON-RR'))
  INTO "APP"."ASSIGNMENTS" ("EMP_ID",
    "PROJECT_CODE",
    "ROLE",
    "ASSIGNED_ON")
VALUES (1019E0,
    'P00004',
    'DEV',
    to_date('01-MAY-02','DD-MON-RR'))
SELECT 1 FROM DUAL;
INSERT ALL
  INTO "APP"."ASSIGNMENTS" ("EMP_ID","PROJECT_CODE","ROLE","ASSIGNED_ON") VALUES ('01019','P00000','QA',to_date('20-OCT-01','DD-MON-RR'))
SELECT 1 FROM DUAL;
-- end of APP.ASSIGNMENTS
REM INSERTING into APP.TIMESHEETS
SET DEFINE OFF;
/* Rows of APP.TIMESHEETS; a ';' or -- in here is not SQL */
INSERT ALL
  INTO APP.TIMESHEETS (TS_ID,
    EMP_ID,
    PROJECT_CODE,
    WORK_DATE,
    HOURS)
VALUES ('01',
    '01000',
    'P00000',
    to_date('02-JAN-00','DD-MON-RR'),
    8.0)
  INTO app.timesheets (ts_id,emp_id,project_code,work_date,hours) VALUES ('02','01000','P00000',to_date('03-JAN-00','DD-MON-RR'),'4.25')
  INTO APP.TIMESHEETS (TS_ID,EMP_ID,PROJECT_CODE,WORK_DATE,HOURS) VALUES ('03','1000','P00001',to_date('04-JAN-00','DD-MON-RR'),'6')
  INTO app.timesheets (ts_id,emp_id,project_code,work_date,hours) VALUES ('04',1000E0,'P00001',to_date('05-JAN-00','DD-MON-RR'),'7.75')
SELECT 1 FROM DUAL;
INSERT ALL
  INTO "APP"."TIMESHEETS" ("TS_ID","EMP_ID","PROJECT_CODE","WORK_DATE","HOURS") VALUES (5E0,1000.0,'P00001',to_date('06-JAN-00','DD-MON-RR'),'7.5')
  INTO app.timesheets (ts_id,
    emp_id,
    project_code,
    work_date,
    hours)
VALUES ('06',
    '1001',
    'P00001',
    to_date('07-JAN-00','DD-MON-RR'),
    '08')
SELECT 1 FROM DUAL;
INSERT ALL
  INTO app.timesheets (ts_id,
    emp_id,
    project_code,
    work_date,
    hours)
VALUES (7.0,
    1001.0,
    null,
    to_date('08-JAN-00','DD-MON-RR'),
    '4.25')
  INTO APP.TIMESHEETS (TS_ID,
    EMP_ID,
    PROJECT_CODE,
    WORK_DATE,
    HOURS)
VALUES (8E0,
    1001,
    'P00002',
    to_date('09-JAN-00','DD-MON-RR'),
    6)
SELECT 1 FROM DUAL;
INSERT ALL
  INTO "APP"."TIMESHEETS" ("TS_ID",
    "EMP_ID",
    "PROJECT_CODE",
    "WORK_DATE",
    "HOURS")
VALUES (9,
    1001.0,
    'P00002',
    to_date('10-JAN-00','DD-MON-RR'),
    '7.75')
  INTO app.timesheets (ts_id,emp_id,project_code,work_date,hours) VALUES (10E0,'01001','P00002',to_date('11-JAN-00','DD-MON-RR'),'7.5')
  INTO "APP"."TIMESHEETS" ("TS_ID","EMP_ID","PROJECT_CODE","WORK_DATE","HOURS") VALUES (11,'01002','P00002',to_date('12-JAN-00','DD-MON-RR'),8E0)
  INTO APP.TIMESHEETS (TS_ID,
    EMP_ID,
    PROJECT_CODE,
    WORK_DATE,
    HOURS)
VALUES ('012',
    '1002',
    'P00002',
    to_date('13-JAN-00','DD-MON-RR'),
    '4.25')
SELECT 1 FROM DUAL;
INSERT ALL
  INTO "APP"."TIMESHEETS" ("TS_ID","EMP_ID","PROJECT_CODE","WORK_DATE","HOURS") VALUES (13.0,1002.0,'P00003',to_date('14-JAN-00','DD-MON-RR'),6E0)
  INTO app.timesheets (ts_id,emp_id,project_code,work_date,hours) VALUES ('014',1002,'P00003',to_date('15-JAN-00','DD-MON-RR'),'7.75')
  INTO app.timesheets (ts_id,emp_id,project_code,work_date,hours) VALUES (15,'01002','P00003',to_date('16-JAN-00','DD-MON-RR'),7.5)
SELECT 1 FROM DUAL;
INSERT ALL
  INTO "APP"."TIMESHEETS" ("TS_ID",
    "EMP_ID",
    "PROJECT_CODE",
    "WORK_DATE",
    "HOURS")
VALUES (16,
    1003.0,
    'P00003',
    to_date('17-JAN-00','DD-MON-RR'),
    '8')
  INTO app.timesheets (ts_id,emp_id,project_code,work_date,hours) VALUES ('17','1003','P00003',to_date('18-JAN-00','DD-MON-RR'),'4.25')
  INTO "APP"."TIMESHEETS" ("TS_ID",
    "EMP_ID",
    "PROJECT_CODE",
    "WORK_DATE",
    "HOURS")
VALUES (18,
    1003E0,
    null,
    to_date('19-JAN-00','DD-MON-RR'),
    6)
  INTO app.timesheets (ts_id,emp_id,project_code,work_date,hours) VALUES (19.0,'01003','P00004',to_date('20-JAN-00','DD-MON-RR'),7.75)
SELECT 1 FROM DUAL;
INSERT ALL
  INTO "APP"."TIMESHEETS" ("TS_ID","EMP_ID","PROJECT_CODE","WORK_DATE","HOURS") VALUES (20.0,1003.0,'P00004',to_date('21-JAN-00','DD-MON-RR'),'7.5')
  INTO "APP"."TIMESHEETS" ("TS_ID",
    "EMP_ID",
    "PROJECT_CODE",
    "WORK_DATE",
    "HOURS")
VALUES (21E0,
    1003.0,
    'P00004',
    to_date('22-JAN-00','DD-MON-RR'),
    8.0)
SELECT 1 FROM DUAL;
INSERT ALL
  INTO APP.TIMESHEETS (TS_ID,
    EMP_ID,
    PROJECT_CODE,
    WORK_DATE,
    HOURS)
VALUES (22,
    '1004',
    'P00004',
    to_date('23-JAN-00','DD-MON-RR'),
    '4.25')
  INTO app.timesheets (ts_id,
    emp_id,
    project_code,
    work_date,
    hours)
VALUES (23,
    1004,
    'P00004',
    to_date('24-JAN-00','DD-MON-RR'),
    '6')
SELECT 1 FROM DUAL;
INSERT ALL
  INTO "APP"."TIMESHEETS" ("TS_ID","EMP_ID","PROJECT_CODE","WORK_DATE","HOURS") VALUES ('24',1004,'P00004',to_date('25-JAN-00','DD-MON-RR'),'7.75')
  INTO APP.TIMESHEETS (TS_ID,
    EMP_ID,
    PROJECT_CODE,
    WORK_DATE,
    HOURS)
VALUES (25,
    '1004',
    'P00000',
    to_date('26-JAN-00','DD-MON-RR'),
    7.5)
  INTO "APP"."TIMESHEETS" ("TS_ID","EMP_ID","PROJECT_CODE","WORK_DATE","HOURS") VALUES ('026','1004','P00000',to_date('27-JAN-00','DD-MON-RR'),'08')
  INTO APP.TIMESHEETS (TS_ID,EMP_ID,PROJECT_CODE,WORK_DATE,HOURS) VALUES ('27','1005','P00000',to_date('28-JAN-00','DD-MON-RR'),4.25)
SELECT 1 FROM DUAL;
INSERT ALL
  INTO app.timesheets (ts_id,
    emp_id,
    project_code,
    work_date,
    hours)
VALUES (28.0,
    1005,
    'P00000',
    to_date('29-JAN-00','DD-MON-RR'),
    6.0)
SELECT 1 FROM DUAL;
INSERT INTO "APP"."TIMESHEETS" ("TS_ID","EMP_ID","PROJECT_CODE","WORK_DATE","HOURS") VALUES (121.0,1005,'P00003',to_date('23-AUG-24','DD-MON-RR'),'1');
INSERT ALL
  INTO APP.TIMESHEETS (TS_ID,
    EMP_ID,
    PROJECT_CODE,
    WORK_DATE,
    HOURS)
VALUES (29.0,
    '1005',
    null,
    to_date('30-JAN-00','DD-MON-RR'),
    '7.75')
  INTO APP.TIMESHEETS (TS_ID,EMP_ID,PROJECT_CODE,WORK_DATE,HOURS) VALUES (30,'01005','P00001',to_date('31-JAN-00','DD-MON-RR'),7.5)
  INTO APP.TIMESHEETS (TS_ID,
    EMP_ID,
    PROJECT_CODE,
    WORK_DATE,
    HOURS)
VALUES (31.0,
    '01006',
    'P00001',
    to_date('01-FEB-00','DD-MON-RR'),
    '08')
SELECT 1 FROM DUAL;
INSERT ALL
  INTO APP.TIMESHEETS (TS_ID,EMP_ID,PROJECT_CODE,WORK_DATE,HOURS) VALUES ('032','1006','P00001',to_date('02-FEB-00','DD-MON-RR'),'4.25')
  INTO app.timesheets (ts_id,emp_id,project_code,work_date,hours) VALUES (33E0,'1006','P00002',to_date('03-FEB-00','DD-MON-RR'),'6')
  INTO APP.TIMESHEETS (TS_ID,EMP_ID,PROJECT_CODE,WORK_DATE,HOURS) VALUES (34,1006,'P00002',to_date('04-FEB-00','DD-MON-RR'),'7.75')
SELECT 1 FROM DUAL;
INSERT ALL
  INTO app.timesheets (ts_id,emp_id,project_code,work_date,hours) VALUES ('35',1006E0,'P00002',to_date('05-FEB-00','DD-MON-RR'),'7.5')
  INTO "APP"."TIMESHEETS" ("TS_ID","EMP_ID","PROJECT_CODE","WORK_DATE","HOURS") VALUES ('036',1007E0,'P00002',to_date('06-FEB-00','DD-MON-RR'),'08')
SELECT 1 FROM DUAL;
INSERT ALL
  INTO APP.TIMESHEETS (TS_ID,
    EMP_ID,
    PROJECT_CODE,
    WORK_DATE,
    HOURS)
VALUES ('37',
    '01007',
    'P00002',
    to_date('07-FEB-00','DD-MON-RR'),
    '4.25')
  INTO "APP"."TIMESHEETS" ("TS_ID","EMP_ID","PROJECT_CODE","WORK_DATE","HOURS") VALUES (38,1007E0,'P00003',to_date('08-FEB-00','DD-MON-RR'),6)
  INTO APP.TIMESHEETS (TS_ID,
    EMP_ID,
    PROJECT_CODE,
    WORK_DATE,
    HOURS)
VALUES ('39',
    '01007',
    'P00003',
    to_date('09-FEB-00','DD-MON-RR'),
    '7.75')
  INTO "APP"."TIMESHEETS" ("TS_ID",
    "EMP_ID",
    "PROJECT_CODE",
    "WORK_DATE",
    "HOURS")
VALUES (40.0,
    1007,
    null,
    to_date('10-FEB-00','DD-MON-RR'),
    '7.5')
SELECT 1 FROM DUAL;
INSERT ALL
  INTO "APP"."TIMESHEETS" ("TS_ID","EMP_ID","PROJECT_CODE","WORK_DATE","HOURS") VALUES ('41','01008','P00003',to_date('11-FEB-00','DD-MON-RR'),8.0)
  INTO app.timesheets (ts_id,emp_id,project_code,work_date,hours) VALUES ('042','1008','P00003',to_date('12-FEB-00','DD-MON-RR'),'4.25')
  INTO app.timesheets (ts_id,
    emp_id,
    project_code,
    work_date,
    hours)
VALUES (43.0,
    1008,
    'P00004',
    to_date('13-FEB-00','DD-MON-RR'),
    '6')
  INTO "APP"."TIMESHEETS" ("TS_ID",
    "EMP_ID",
    "PROJECT_CODE",
    "WORK_DATE",
    "HOURS")
VALUES (44,
    1008,
    'P00004',
    to_date('14-FEB-00','DD-MON-RR'),
    '7.75')
SELECT 1 FROM DUAL;
INSERT ALL
  INTO "APP"."TIMESHEETS" ("TS_ID",
    "EMP_ID",
    "PROJECT_CODE",
    "WORK_DATE",
    "HOURS")
VALUES (45,
    '1009',
    'P00004',
    to_date('15-FEB-00','DD-MON-RR'),
    '7.5')
  INTO "APP"."TIMESHEETS" ("TS_ID",
    "EMP_ID",
    "PROJECT_CODE",
    "WORK_DATE",
    "HOURS")
VALUES (46E0,
    '01009',
    'P00004',
    to_date('16-FEB-00','DD-MON-RR'),
    8E0)
  INTO app.timesheets (ts_id,emp_id,project_code,work_date,hours) VALUES (47.0,1009.0,'P00004',to_date('17-FEB-00','DD-MON-RR'),'4.25')
  INTO APP.TIMESHEETS (TS_ID,EMP_ID,PROJECT_CODE,WORK_DATE,HOURS) VALUES (48.0,'1009','P00000',to_date('18-FEB-00','DD-MON-RR'),'6')
SELECT 1 FROM DUAL;
INSERT ALL
  INTO APP.TIMESHEETS (TS_ID,
    EMP_ID,
    PROJECT_CODE,
    WORK_DATE,
    HOURS)
VALUES ('049',
    '1009',
    'P00000',
    to_date('19-FEB-00','DD-MON-RR'),
    '7.75')
  INTO "APP"."TIMESHEETS" ("TS_ID",
    "EMP_ID",
    "PROJECT_CODE",
    "WORK_DATE",
    "HOURS")
VALUES (50,
    '1009',
    'P00000',
    to_date('20-FEB-00','DD-MON-RR'),
    '7.5')
  INTO app.timesheets (ts_id,
    emp_id,
    project_code,
    work_date,
    hours)
VALUES (51.0,
    1010E0,
    null,
    to_date('21-FEB-00','DD-MON-RR'),
    '8')
  INTO app.timesheets (ts_id,
    emp_id,
    project_code,
    work_date,
    hours)
VALUES (52E0,
    1010E0,
    'P00000',
    to_date('22-FEB-00','DD-MON-RR'),
    4.25)
SELECT 1 FROM DUAL;
INSERT ALL
  INTO app.timesheets (ts_id,
    emp_id,
    project_code,
    work_date,
    hours)
VALUES (53.0,
    '1010',
    'P00000',
    to_date('23-FEB-00','DD-MON-RR'),
    6E0)
SELECT 1 FROM DUAL;
Insert into APP.TIMESHEETS (TS_ID,EMP_ID,PROJECT_CODE,WORK_DATE,HOURS) values ('0122',1010,'P00000',to_date('24-AUG-24','DD-MON-RR'),null);
INSERT ALL
  INTO app.timesheets (ts_id,emp_id,project_code,work_date,hours) VALUES (54.0,1010.0,'P00001',to_date('24-FEB-00','DD-MON-RR'),'7.75')
  INTO app.timesheets (ts_id,
    emp_id,
    project_code,
    work_date,
    hours)
VALUES (55,
    1010.0,
    'P00001',
    to_date('25-FEB-00','DD-MON-RR'),
    '7.5')
  INTO app.timesheets (ts_id,
    emp_id,
    project_code,
    work_date,
    hours)
VALUES (56,
    '01010',
    'P00001',
    to_date('26-FEB-00','DD-MON-RR'),
    '8')
  INTO APP.TIMESHEETS (TS_ID,EMP_ID,PROJECT_CODE,WORK_DATE,HOURS) VALUES (57E0,'1011','P00001',to_date('27-FEB-00','DD-MON-RR'),'4.25')
SELECT 1 FROM DUAL;
INSERT ALL
  INTO "APP"."TIMESHEETS" ("TS_ID","EMP_ID","PROJECT_CODE","WORK_DATE","HOURS") VALUES ('058','01011','P00001',to_date('28-FEB-00','DD-MON-RR'),'6')
  INTO "APP"."TIMESHEETS" ("TS_ID","EMP_ID","PROJECT_CODE","WORK_DATE","HOURS") VALUES (59.0,1011E0,'P00001',to_date('29-FEB-00','DD-MON-RR'),'7.75')
  INTO "APP"."TIMESHEETS" ("TS_ID","EMP_ID","PROJECT_CODE","WORK_DATE","HOURS") VALUES (60.0,'01011','P00002',to_date('01-MAR-00','DD-MON-RR'),'7.5')
SELECT 1 FROM DUAL;
INSERT ALL
  INTO app.timesheets (ts_id,
    emp_id,
    project_code,
    work_date,
    hours)
VALUES (61.0,
    1011,
    'P00002',
    to_date('02-MAR-00','DD-MON-RR'),
    '8')
  INTO "APP"."TIMESHEETS" ("TS_ID",
    "EMP_ID",
    "PROJECT_CODE",
    "WORK_DATE",
    "HOURS")
VALUES ('62',
    '01011',
    null,
    to_date('03-MAR-00','DD-MON-RR'),
    4.25)
SELECT 1 FROM DUAL;
INSERT ALL
  INTO "APP"."TIMESHEETS" ("TS_ID",
    "EMP_ID",
    "PROJECT_CODE",
    "WORK_DATE",
    "HOURS")
VALUES (63.0,
    '01012',
    'P00002',
    to_date('04-MAR-00','DD-MON-RR'),
    '6')
  INTO APP.TIMESHEETS (TS_ID,
    EMP_ID,
    PROJECT_CODE,
    WORK_DATE,
    HOURS)
VALUES ('64',
    '1012',
    'P00002',
    to_date('05-MAR-00','DD-MON-RR'),
    '7.75')
SELECT 1 FROM DUAL;
INSERT ALL
  INTO APP.TIMESHEETS (TS_ID,EMP_ID,PROJECT_CODE,WORK_DATE,HOURS) VALUES (65E0,'01012','P00003',to_date('06-MAR-00','DD-MON-RR'),7.5)
  INTO app.timesheets (ts_id,emp_id,project_code,work_date,hours) VALUES (66,'01012','P00003',to_date('07-MAR-00','DD-MON-RR'),8)
SELECT 1 FROM DUAL;
INSERT ALL
  INTO "APP"."TIMESHEETS" ("TS_ID","EMP_ID","PROJECT_CODE","WORK_DATE","HOURS") VALUES ('67','01013','P00003',to_date('08-MAR-00','DD-MON-RR'),'4.25')
  INTO APP.TIMESHEETS (TS_ID,EMP_ID,PROJECT_CODE,WORK_DATE,HOURS) VALUES ('068',1013E0,'P00003',to_date('09-MAR-00','DD-MON-RR'),6)
  INTO APP.TIMESHEETS (TS_ID,
    EMP_ID,
    PROJECT_CODE,
    WORK_DATE,
    HOURS)
VALUES (69,
    1013E0,
    'P00003',
    to_date('10-MAR-00','DD-MON-RR'),
    '7.75')
  INTO "APP"."TIMESHEETS" ("TS_ID",
    "EMP_ID",
    "PROJECT_CODE",
    "WORK_DATE",
    "HOURS")
VALUES ('70',
    1013.0,
    'P00004',
    to_date('11-MAR-00','DD-MON-RR'),
    7.5)
SELECT 1 FROM DUAL;
INSERT ALL
  INTO APP.TIMESHEETS (TS_ID,
    EMP_ID,
    PROJECT_CODE,
    WORK_DATE,
    HOURS)
VALUES (71E0,
    1013.0,
    'P00004',
    to_date('12-MAR-00','DD-MON-RR'),
    8)
  INTO app.timesheets (ts_id,emp_id,project_code,work_date,hours) VALUES (72.0,1014.0,'P00004',to_date('13-MAR-00','DD-MON-RR'),4.25)
  INTO "APP"."TIMESHEETS" ("TS_ID","EMP_ID","PROJECT_CODE","WORK_DATE","HOURS") VALUES (73,'1014',null,to_date('14-MAR-00','DD-MON-RR'),'6')
SELECT 1 FROM DUAL;
INSERT ALL
  INTO app.timesheets (ts_id,
    emp_id,
    project_code,
    work_date,
    hours)
VALUES ('074',
    '01014',
    'P00004',
    to_date('15-MAR-00','DD-MON-RR'),
    '7.75')
  INTO "APP"."TIMESHEETS" ("TS_ID","EMP_ID","PROJECT_CODE","WORK_DATE","HOURS") VALUES ('75','1014','P00000',to_date('16-MAR-00','DD-MON-RR'),'7.5')
  INTO app.timesheets (ts_id,
    emp_id,
    project_code,
    work_date,
    hours)
VALUES ('076',
    1014.0,
    'P00000',
    to_date('17-MAR-00','DD-MON-RR'),
    '8')
  INTO APP.TIMESHEETS (TS_ID,EMP_ID,PROJECT_CODE,WORK_DATE,HOURS) VALUES (77E0,'1014','P00000',to_date('18-MAR-00','DD-MON-RR'),'4.25')
SELECT 1 FROM DUAL;
INSERT ALL
  INTO app.timesheets (ts_id,
    emp_id,
    project_code,
    work_date,
    hours)
VALUES ('78',
    '1015',
    'P00000',
    to_date('19-MAR-00','DD-MON-RR'),
    '6')
  INTO app.timesheets (ts_id,emp_id,project_code,work_date,hours) VALUES (79.0,1015.0,'P00000',to_date('20-MAR-00','DD-MON-RR'),'7.75')
  INTO "APP"."TIMESHEETS" ("TS_ID",
    "EMP_ID",
    "PROJECT_CODE",
    "WORK_DATE",
    "HOURS")
VALUES (80E0,
    1015.0,
    'P00000',
    to_date('21-MAR-00','DD-MON-RR'),
    '7.5')
SELECT 1 FROM DUAL;
INSERT INTO "APP"."TIMESHEETS" ("TS_ID",
    "EMP_ID",
    "PROJECT_CODE",
    "WORK_DATE",
    "HOURS")
VALUES (80.0,
    '01015',
    'P00000',
    to_date('25-AUG-24','DD-MON-RR'),
    1E0);
INSERT ALL
  INTO "APP"."TIMESHEETS" ("TS_ID","EMP_ID","PROJECT_CODE","WORK_DATE","HOURS") VALUES ('81',1015,'P00001',to_date('22-MAR-00','DD-MON-RR'),'8')
  INTO "APP"."TIMESHEETS" ("TS_ID","EMP_ID","PROJECT_CODE","WORK_DATE","HOURS") VALUES ('082','01015','P00001',to_date('23-MAR-00','DD-MON-RR'),'4.25')
  INTO APP.TIMESHEETS (TS_ID,EMP_ID,PROJECT_CODE,WORK_DATE,HOURS) VALUES (83.0,1015,'P00001',to_date('24-MAR-00','DD-MON-RR'),'6')
  INTO "APP"."TIMESHEETS" ("TS_ID","EMP_ID","PROJECT_CODE","WORK_DATE","HOURS") VALUES (84E0,1016E0,null,to_date('25-MAR-00','DD-MON-RR'),'7.75')
SELECT 1 FROM DUAL;
INSERT ALL
  INTO "APP"."TIMESHEETS" ("TS_ID","EMP_ID","PROJECT_CODE","WORK_DATE","HOURS") VALUES ('85','01016','P00001',to_date('26-MAR-00','DD-MON-RR'),'7.5')
  INTO APP.TIMESHEETS (TS_ID,EMP_ID,PROJECT_CODE,WORK_DATE,HOURS) VALUES ('86',1016E0,'P00001',to_date('27-MAR-00','DD-MON-RR'),'8')
  INTO app.timesheets (ts_id,emp_id,project_code,work_date,hours) VALUES ('087','01016','P00002',to_date('28-MAR-00','DD-MON-RR'),'4.25')
  INTO APP.TIMESHEETS (TS_ID,
    EMP_ID,
    PROJECT_CODE,
    WORK_DATE,
    HOURS)
VALUES ('88',
    1016,
    'P00002',
    to_date('29-MAR-00','DD-MON-RR'),
    '06')
SELECT 1 FROM DUAL;
INSERT ALL
  INTO "APP"."TIMESHEETS" ("TS_ID","EMP_ID","PROJECT_CODE","WORK_DATE","HOURS") VALUES (89E0,1016,'P00002',to_date('30-MAR-00','DD-MON-RR'),'7.75')
  INTO APP.TIMESHEETS (TS_ID,
    EMP_ID,
    PROJECT_CODE,
    WORK_DATE,
    HOURS)
VALUES (90.0,
    1017.0,
    'P00002',
    to_date('31-MAR-00','DD-MON-RR'),
    7.5)
  INTO "APP"."TIMESHEETS" ("TS_ID","EMP_ID","PROJECT_CODE","WORK_DATE","HOURS") VALUES (91E0,1017E0,'P00002',to_date('01-APR-00','DD-MON-RR'),8.0)
  INTO APP.TIMESHEETS (TS_ID,EMP_ID,PROJECT_CODE,WORK_DATE,HOURS) VALUES ('92',1017E0,'P00002',to_date('02-APR-00','DD-MON-RR'),'4.25')
SELECT 1 FROM DUAL;
INSERT ALL
  INTO APP.TIMESHEETS (TS_ID,
    EMP_ID,
    PROJECT_CODE,
    WORK_DATE,
    HOURS)
VALUES (93,
    '1017',
    'P00003',
    to_date('03-APR-00','DD-MON-RR'),
    6)
  INTO app.timesheets (ts_id,emp_id,project_code,work_date,hours) VALUES (94,1017E0,'P00003',to_date('04-APR-00','DD-MON-RR'),'7.75')
  INTO app.timesheets (ts_id,emp_id,project_code,work_date,hours) VALUES (95.0,1018.0,null,to_date('05-APR-00','DD-MON-RR'),'7.5')
  INTO app.timesheets (ts_id,emp_id,project_code,work_date,hours) VALUES ('96',1018E0,'P00003',to_date('06-APR-00','DD-MON-RR'),8E0)
SELECT 1 FROM DUAL;
INSERT ALL
  INTO APP.TIMESHEETS (TS_ID,
    EMP_ID,
    PROJECT_CODE,
    WORK_DATE,
    HOURS)
VALUES ('097',
    '1018',
    'P00004',
    to_date('07-APR-00','DD-MON-RR'),
    '4.25')
  INTO APP.TIMESHEETS (TS_ID,EMP_ID,PROJECT_CODE,WORK_DATE,HOURS) VALUES ('098',1018,'P00004',to_date('08-APR-00','DD-MON-RR'),6E0)
SELECT 1 FROM DUAL;
INSERT ALL
  INTO APP.TIMESHEETS (TS_ID,
    EMP_ID,
    PROJECT_CODE,
    WORK_DATE,
    HOURS)
VALUES (99,
    1019.0,
    'P00004',
    to_date('09-APR-00','DD-MON-RR'),
    '7.75')
  INTO "APP"."TIMESHEETS" ("TS_ID","EMP_ID","PROJECT_CODE","WORK_DATE","HOURS") VALUES (100,'01019','P00004',to_date('10-APR-00','DD-MON-RR'),'7.5')
SELECT 1 FROM DUAL;
INSERT ALL
  INTO APP.TIMESHEETS (TS_ID,
    EMP_ID,
    PROJECT_CODE,
    WORK_DATE,
    HOURS)
VALUES (101,
    1019E0,
    'P00000',
    to_date('11-APR-00','DD-MON-RR'),
    '08')
  INTO "APP"."TIMESHEETS" ("TS_ID","EMP_ID","PROJECT_CODE","WORK_DATE","HOURS") VALUES ('102','1019','P00000',to_date('12-APR-00','DD-MON-RR'),'4.25')
SELECT 1 FROM DUAL;
-- end of APP.TIMESHEETS
REM INSERTING into APP.LEGACY_NOTES
SET DEFINE OFF;
/* Rows of APP.LEGACY_NOTES; a ';' or -- in here is not SQL */
INSERT ALL
  INTO "APP"."LEGACY_NOTES" ("NOTE_ID","EMP_ID","NOTE_TEXT") VALUES (1E0,-1.0,'Rivi 1
Rivi 2: muistiinpano 0')
  INTO APP.LEGACY_NOTES (NOTE_ID,
    EMP_ID,
    NOTE_TEXT)
VALUES (2E0,
    '01001',
    'Rivi 1
Rivi 2: muistiinpano 1')
SELECT 1 FROM DUAL;
INSERT ALL
  INTO app.legacy_notes (note_id,emp_id,note_text) VALUES (3E0,1002,'Rivi 1
Rivi 2: muistiinpano 2')
SELECT 1 FROM DUAL;
-- end of APP.LEGACY_NOTES
REM INSERTING into APP."AuditLog"
SET DEFINE OFF;
/* Rows of APP."AuditLog"; a ';' or -- in here is not SQL */
Insert into APP."AuditLog" (LOG_ID,
    EMP_ID,
    LOGGED_AT,
    DETAIL)
values (APP.AUDIT_SEQ.NEXTVAL,
    '01000',
    to_timestamp('22-JUN-01 01.26.36.000000000 AM','DD-MON-RR HH.MI.SSXFF AM'),
    'login; ok -- via SSO');
Insert into APP."AuditLog" (LOG_ID,
    EMP_ID,
    LOGGED_AT,
    DETAIL)
values (APP.AUDIT_SEQ.NEXTVAL,
    1001,
    to_timestamp('25-OCT-02 05.00.21.000000000 PM','DD-MON-RR HH.MI.SSXFF AM'),
    'login; ok -- via SSO');
Insert into APP."AuditLog" (LOG_ID,EMP_ID,LOGGED_AT,DETAIL) values (APP.AUDIT_SEQ.NEXTVAL,-1E0,to_timestamp('29-MAR-10 10.40.12.000000000 PM','DD-MON-RR HH.MI.SSXFF AM'),'orphan');
Insert into APP."AuditLog" (LOG_ID,
    EMP_ID,
    LOGGED_AT,
    DETAIL)
values (APP.AUDIT_SEQ.NEXTVAL,
    1002.0,
    to_timestamp('16-MAY-12 06.26.43.000000000 AM','DD-MON-RR HH.MI.SSXFF AM'),
    'login; ok -- via SSO');
-- end of APP."AuditLog"
