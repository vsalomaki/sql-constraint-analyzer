REM INSERTING into APP.DEPARTMENTS
SET DEFINE OFF;
Insert into APP.DEPARTMENTS (DEPT_ID,NAME,MANAGER_ID,COUNTRY_CODE) values ('10','R&D 0','1000','FI');
Insert into APP.DEPARTMENTS (DEPT_ID,NAME,MANAGER_ID,COUNTRY_CODE) values ('61','Extra dept 1',null,'XX');
Insert into APP.DEPARTMENTS (DEPT_ID,NAME,MANAGER_ID,COUNTRY_CODE) values ('20','Myynti & markkinointi 1','1007','SE ');
Insert into APP.DEPARTMENTS (DEPT_ID,NAME,MANAGER_ID,COUNTRY_CODE) values ('62','Myynti & markkinointi 1',null,null);
Insert into APP.DEPARTMENTS (DEPT_ID,NAME,MANAGER_ID,COUNTRY_CODE) values ('30','Hallinto 2','1014','NO');
Insert into APP.DEPARTMENTS (DEPT_ID,NAME,MANAGER_ID,COUNTRY_CODE) values ('30','Extra dept 3',null,null);
Insert into APP.DEPARTMENTS (DEPT_ID,NAME,MANAGER_ID,COUNTRY_CODE) values ('40','Logistiikka 3',null,'DK');
Insert into APP.DEPARTMENTS (DEPT_ID,NAME,MANAGER_ID,COUNTRY_CODE) values (null,'Extra dept 4',null,null);
Insert into APP.DEPARTMENTS (DEPT_ID,NAME,MANAGER_ID,COUNTRY_CODE) values ('82',null,null,null);
Insert into APP.DEPARTMENTS (DEPT_ID,NAME,MANAGER_ID,COUNTRY_CODE) values ('50','Tuotekehitys 4','1008',null);
Insert into APP.DEPARTMENTS (DEPT_ID,NAME,MANAGER_ID,COUNTRY_CODE) values ('65','Extra dept 5','-5',null);
COMMIT;
