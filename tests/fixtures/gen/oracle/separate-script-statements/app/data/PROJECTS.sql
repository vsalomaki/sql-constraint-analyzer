REM INSERTING into APP.PROJECTS
SET DEFINE OFF;
Insert into APP.PROJECTS (PROJECT_CODE,NAME,DEPT_NAME,EXTERNAL_REF,START_DATE) values ('P00000','Projekti 0','R&D 0','EXT-0',to_date('23-AUG-11','DD-MON-RR'));
Insert into APP.PROJECTS (PROJECT_CODE,NAME,DEPT_NAME,EXTERNAL_REF,START_DATE) values ('P00001','Customer''s portal 1','Myynti & markkinointi 1',null,to_date('04-AUG-10','DD-MON-RR'));
Insert into APP.PROJECTS (PROJECT_CODE,NAME,DEPT_NAME,EXTERNAL_REF) values ('X00001','Extra project 1','Lakkautettu 1',null);
Insert into APP.PROJECTS (PROJECT_CODE,NAME,DEPT_NAME,EXTERNAL_REF,START_DATE) values ('P00002','Varasto € 2','Hallinto 2','EXT-2',to_date('06-JUN-10','DD-MON-RR'));
Insert into APP.PROJECTS (PROJECT_CODE,NAME,DEPT_NAME,EXTERNAL_REF) values ('P00002','Extra project 2',null,null);
Insert into APP.PROJECTS (PROJECT_CODE,NAME,DEPT_NAME,EXTERNAL_REF,START_DATE) values ('P00003','Projekti 3','Logistiikka 3','EXT-3',to_date('05-FEB-23','DD-MON-RR'));
Insert into APP.PROJECTS (PROJECT_CODE,NAME,DEPT_NAME,EXTERNAL_REF) values ('X00003','Extra project 3',null,'EXT-3');
Insert into APP.PROJECTS (PROJECT_CODE,NAME,DEPT_NAME,EXTERNAL_REF,START_DATE) values ('P00004','Projekti 4','Tuotekehitys 4','EXT-4',to_date('05-SEP-11','DD-MON-RR'));
