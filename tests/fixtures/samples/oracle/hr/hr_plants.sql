-- Rows planted after hr_populate.sql, and a table with a virtual key and an identity column; ../expected.json lists what they break.
INSERT INTO job_history VALUES (200, TO_DATE('01-07-2012', 'dd-MM-yyyy'), TO_DATE('31-12-2013', 'dd-MM-yyyy'), 'AD_ASST', 10);
INSERT INTO employees VALUES (300, 'Sam', 'Kingsley', 'SKING', NULL, TO_DATE('01-03-2020', 'dd-MM-yyyy'), 'IT_PROG', 5000, NULL, 103, 60);
INSERT INTO employees VALUES (301, 'Dana', 'Nowhere', 'DNOWHERE', NULL, TO_DATE('01-03-2020', 'dd-MM-yyyy'), 'IT_PROG', 5000, NULL, 103, 999);
INSERT INTO jobs VALUES ('XX_NONE', NULL, 1000, 2000);
INSERT ALL INTO regions VALUES (70, 'Atlantis') INTO regions VALUES (80, 'Lemuria') INTO countries (country_id, country_name, capital) VALUES ('ZZ', 'Nowhere', 'Z') SELECT 1 FROM DUAL;
INSERT INTO countries VALUES ('ZY', 'Lyonesse', 70);
INSERT INTO regions VALUES (80, 'Lemuria');
CREATE TABLE badges (badge_id NUMBER GENERATED ALWAYS AS IDENTITY, employee_id NUMBER(6) NOT NULL, code VARCHAR2(10) NOT NULL, code_uc VARCHAR2(10) GENERATED ALWAYS AS (UPPER(code)) VIRTUAL, CONSTRAINT badges_code_uk UNIQUE (employee_id, code_uc));
INSERT INTO badges (employee_id, code) VALUES (100, 'a1');
INSERT INTO badges (employee_id, code) VALUES (100, 'b2');
INSERT INTO badges VALUES (DEFAULT, 101, 'a1', DEFAULT);
INSERT INTO badges (badge_id, employee_id, code) VALUES (7, 102, 'c3');
INSERT INTO badges (employee_id, code, code_uc) VALUES (103, 'd4', 'D4');
COMMIT;
