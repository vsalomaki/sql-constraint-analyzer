# Oracle sample schemas

HR (`human_resources/`) and CO (`customer_orders/`) from
[oracle-samples/db-sample-schemas](https://github.com/oracle-samples/db-sample-schemas) at commit
`6660bad68c07bd143430ace58565b3f727e17263`, under the MIT licence in `LICENSE.txt`. The data is fictional.
`make sample-fixtures` (`scripts/oracle/sample-fixtures.sh`) rewrites the four scripts below and `LICENSE.txt`
from that commit.

- `hr_create.sql` and `co_create.sql` are Oracle's files, byte for byte.
- `hr_populate.sql` and `co_populate.sql` lose the lines that are only `BEGIN`, `END;` or `/`: Oracle runs each
  table's INSERTs as one PL/SQL block, which `sqlca` doesn't follow.
- CO's product 4 builds its JSON in a PL/SQL variable, since the value is longer than SQL's
  `UTL_RAW.CAST_TO_RAW` returns. Its `DECLARE` and assignment are gone, and the INSERT gets the JSON as a literal,
  keeping as many of its reviews, in order, as fit in 2000 bytes.

Every other line is as Oracle wrote it. Oracle Free loads each schema's two files without an error: 216 rows for
HR, 8,783 for CO.

`sqlcl/` (`make sample-sqlcl-fixtures`, needs Docker) is what Oracle Free holds after loading `hr/` and `co/`,
exported again by SQLcl in SQL Developer's separate-files layout: `tables/`, `sequences/`, `indexes/`, `data/` and
`ref_constraints/` per schema, with `order.txt` and `counts.csv`.

`hr/hr_plants.sql` and `co/co_plants.sql` are ours: rows that break the schemas' constraints, planted after each
populate script. `order.txt` loads both schemas with their plants, and `expected.json`, written by hand in the
generator's manifest format, lists what they break and what `sqlca fix` removes: a duplicate primary and unique
key and a NULL in a NOT NULL column in each schema, a missing department in HR (a nullable foreign key, which
`--on-fk null` sets to NULL), and in CO an order with a missing customer and two order lines on it, which go with
it. A duplicate's first row is a line of a populate script, so a new sample commit can move it.
