REM INSERTING into APP."AuditLog"
SET DEFINE OFF;
Insert into APP."AuditLog" (LOG_ID,EMP_ID,LOGGED_AT,DETAIL) values ('1','1000',to_timestamp('22-JUN-01 01.26.36.000000000 AM','DD-MON-RR HH.MI.SSXFF AM'),'login ok');
Insert into APP."AuditLog" (LOG_ID,EMP_ID,LOGGED_AT,DETAIL) values ('2','1001',to_timestamp('25-OCT-02 05.00.21.000000000 PM','DD-MON-RR HH.MI.SSXFF AM'),'login ok');
Insert into APP."AuditLog" (LOG_ID,EMP_ID,LOGGED_AT,DETAIL) values ('4','-1',to_timestamp('29-MAR-10 10.40.12.000000000 PM','DD-MON-RR HH.MI.SSXFF AM'),'orphan');
Insert into APP."AuditLog" (LOG_ID,EMP_ID,LOGGED_AT,DETAIL) values ('3','1002',to_timestamp('16-MAY-12 06.26.43.000000000 AM','DD-MON-RR HH.MI.SSXFF AM'),'login ok');
