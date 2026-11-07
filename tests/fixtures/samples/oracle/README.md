# Oracle sample schemas

HR (`human_resources/`) and CO (`customer_orders/`) from
[oracle-samples/db-sample-schemas](https://github.com/oracle-samples/db-sample-schemas) at commit
`6660bad68c07bd143430ace58565b3f727e17263`, under the MIT licence in `LICENSE.txt`. The data is fictional.
`make sample-fixtures` (`scripts/oracle/sample-fixtures.sh`) rewrites `hr/`, `co/` and `LICENSE.txt` from that
commit.

- `hr_create.sql` and `co_create.sql` are Oracle's files, byte for byte.
- `hr_populate.sql` and `co_populate.sql` lose the lines that are only `BEGIN`, `END;` or `/`: Oracle runs each
  table's INSERTs as one PL/SQL block, which `sqlca` doesn't follow.
- CO's product 4 builds its JSON in a PL/SQL variable, since the value is longer than SQL's
  `UTL_RAW.CAST_TO_RAW` returns. Its `DECLARE` and assignment are gone, and the INSERT gets the JSON as a literal,
  keeping as many of its reviews, in order, as fit in 2000 bytes.

Every other line is as Oracle wrote it. Oracle Free loads each schema's two files without an error: 216 rows for
HR, 8,783 for CO.
