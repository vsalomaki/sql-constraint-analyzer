-- Exports schema &1 into /out/&2 the way SQL Developer's separate files do: DDL from DBMS_METADATA (keys in the
-- table DDL, foreign keys in their own files, `;` terminators, storage clauses) and rows as INSERT statements. The
-- object list comes from the dictionary: it writes /work/export_&2..sql and runs it.
SET DEFINE ON
SET FEEDBACK OFF
SET VERIFY OFF
SET LONG 1000000
SET LONGCHUNKSIZE 1000000
SET PAGESIZE 0
SET LINESIZE 32767
SET HEADING OFF
SET TRIMSPOOL ON
BEGIN
  DBMS_METADATA.SET_TRANSFORM_PARAM(DBMS_METADATA.SESSION_TRANSFORM, 'SQLTERMINATOR', TRUE);
  DBMS_METADATA.SET_TRANSFORM_PARAM(DBMS_METADATA.SESSION_TRANSFORM, 'PRETTY', TRUE);
  DBMS_METADATA.SET_TRANSFORM_PARAM(DBMS_METADATA.SESSION_TRANSFORM, 'REF_CONSTRAINTS', FALSE);
END;
/

SPOOL /work/export_&2..sql
SELECT 'SPOOL /out/&2/tables/' || table_name || '.sql' || CHR(10)
    || 'SELECT DBMS_METADATA.GET_DDL(''TABLE'', ''' || table_name || ''', ''&1'') FROM DUAL;'
  FROM all_tables WHERE owner = '&1' ORDER BY table_name;
SELECT 'SPOOL /out/&2/sequences/' || sequence_name || '.sql' || CHR(10)
    || 'SELECT DBMS_METADATA.GET_DDL(''SEQUENCE'', ''' || sequence_name || ''', ''&1'') FROM DUAL;'
  FROM all_sequences
 WHERE sequence_owner = '&1' AND sequence_name NOT LIKE 'ISEQ$$%' ORDER BY sequence_name;
-- Indexes of their own; an index a key uses comes with the table.
SELECT 'SPOOL /out/&2/indexes/' || index_name || '.sql' || CHR(10)
    || 'SELECT DBMS_METADATA.GET_DDL(''INDEX'', ''' || index_name || ''', ''&1'') FROM DUAL;'
  FROM all_indexes i
 WHERE owner = '&1' AND index_type <> 'LOB'
   AND NOT EXISTS (SELECT 1 FROM all_constraints c WHERE c.owner = i.owner AND c.index_name = i.index_name)
 ORDER BY index_name;
SELECT 'SET SQLFORMAT INSERT' FROM DUAL;
SELECT 'SPOOL /out/&2/data/' || t.table_name || '.sql' || CHR(10)
    || 'SELECT * FROM "&1"."' || t.table_name || '" ORDER BY '
    || NVL((SELECT LISTAGG('"' || cc.column_name || '"', ', ') WITHIN GROUP (ORDER BY cc.position)
              FROM all_constraints c
              JOIN all_cons_columns cc ON cc.owner = c.owner AND cc.constraint_name = c.constraint_name
             WHERE c.owner = t.owner AND c.table_name = t.table_name AND c.constraint_type = 'P'), '1') || ';'
  FROM all_tables t WHERE owner = '&1' ORDER BY table_name;
SELECT 'SET SQLFORMAT' FROM DUAL;
SELECT 'SPOOL /out/&2/ref_constraints/' || table_name || '.sql' || CHR(10)
    || 'SELECT DBMS_METADATA.GET_DEPENDENT_DDL(''REF_CONSTRAINT'', ''' || table_name || ''', ''&1'') FROM DUAL;'
  FROM all_tables t
 WHERE owner = '&1'
   AND EXISTS (SELECT 1 FROM all_constraints c
                WHERE c.owner = t.owner AND c.table_name = t.table_name AND c.constraint_type = 'R')
 ORDER BY table_name;
SELECT 'SET SQLFORMAT CSV' || CHR(10) || 'SET HEADING ON' || CHR(10) || 'SET PAGESIZE 50000' || CHR(10)
    || 'SPOOL /out/&2/counts.csv' FROM DUAL;
SELECT LISTAGG('SELECT ''&1..' || table_name || ''' AS "TABLE", COUNT(*) AS "ROWS" FROM "&1"."' || table_name || '"',
               CHR(10) || 'UNION ALL ') WITHIN GROUP (ORDER BY table_name) || ';'
  FROM all_tables WHERE owner = '&1';
SELECT 'SPOOL OFF' || CHR(10) || 'SET SQLFORMAT' || CHR(10) || 'SET HEADING OFF' || CHR(10) || 'SET PAGESIZE 0'
  FROM DUAL;
SPOOL OFF
@/work/export_&2..sql
