-- Rows planted after hr_populate.sql, each breaking one constraint but the last; ../expected.json lists what they break.
INSERT INTO job_history VALUES (200, TO_DATE('01-07-2012', 'dd-MM-yyyy'), TO_DATE('31-12-2013', 'dd-MM-yyyy'), 'AD_ASST', 10);
INSERT INTO employees VALUES (300, 'Sam', 'Kingsley', 'SKING', NULL, TO_DATE('01-03-2020', 'dd-MM-yyyy'), 'IT_PROG', 5000, NULL, 103, 60);
INSERT INTO employees VALUES (301, 'Dana', 'Nowhere', 'DNOWHERE', NULL, TO_DATE('01-03-2020', 'dd-MM-yyyy'), 'IT_PROG', 5000, NULL, 103, 999);
INSERT INTO jobs VALUES ('XX_NONE', NULL, 1000, 2000);
INSERT ALL INTO regions VALUES (70, 'Atlantis') INTO regions VALUES (80, 'Lemuria') INTO countries (country_id, country_name, capital) VALUES ('ZZ', 'Nowhere', 'Z') SELECT 1 FROM DUAL;
INSERT INTO countries VALUES ('ZY', 'Lyonesse', 70);
INSERT INTO regions VALUES (80, 'Lemuria');
COMMIT;
