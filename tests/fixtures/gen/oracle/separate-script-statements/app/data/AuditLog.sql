REM INSERTING into APP."AuditLog"
SET DEFINE OFF;
Insert into APP."AuditLog" (LOG_ID,EMP_ID,LOGGED_AT,DETAIL) values ('1','1000',to_timestamp('09-FEB-19 05.08.48.000000000 PM','DD-MON-RR HH.MI.SSXFF AM'),'login ok');
Insert into APP."AuditLog" (LOG_ID,EMP_ID,LOGGED_AT,DETAIL) values ('2','1001',to_timestamp('04-MAY-16 01.28.43.000000000 PM','DD-MON-RR HH.MI.SSXFF AM'),'login ok');
Insert into APP."AuditLog" (LOG_ID,EMP_ID,LOGGED_AT,DETAIL) values ('4','-1',to_timestamp('05-OCT-15 11.51.35.000000000 PM','DD-MON-RR HH.MI.SSXFF AM'),'orphan');
Insert into APP."AuditLog" (LOG_ID,EMP_ID,LOGGED_AT,DETAIL) values ('3','1002',to_timestamp('05-FEB-17 08.40.26.000000000 PM','DD-MON-RR HH.MI.SSXFF AM'),'login ok');
