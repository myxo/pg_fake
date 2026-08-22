# PostgreSQL SQL grammar reference

This is a compact grammar reference for SQL that `pg_fake` implements or may
implement according to `docs/spec.md`. It is intended for comparing and
structuring property-test generators. It is not an acceptance contract and is
not used to generate parser or test code.

The grammar is adapted from
[`gmr/tree-sitter-postgres/postgres/grammar.js`](https://github.com/gmr/tree-sitter-postgres/blob/main/postgres/grammar.js),
which is generated from PostgreSQL's Bison grammar. The upstream Tree-sitter
grammar may target a different PostgreSQL release than `pg_fake`. That
difference is acceptable for this reference; PostgreSQL itself remains the
differential-test oracle.

The following PostgreSQL areas are intentionally omitted:

- server, database, role, privilege, and tablespace administration;
- extensions, foreign-data wrappers, publications, subscriptions, and
  replication;
- procedural languages, function and procedure definitions, triggers, and
  rules;
- maintenance and diagnostic commands such as `VACUUM`, `ANALYZE`, `REINDEX`,
  `CLUSTER`, and `EXPLAIN`;
- `COPY`, cursors, notifications, prepared-transaction commands, and psql
  client commands;
- indexes, domains, enums, partitions, inheritance, materialized views, and
  other objects outside the planned `pg_fake` feature set;
- the complete SQL/XML and SQL/JSON function grammars. JSON and array values,
  array constructors, access, casts, and ordinary function calls remain in
  scope.

## Notation

```text
x | y       choose x or y
[x]         x is optional
{x}         repeat x zero or more times
(x)         group grammar elements
'x'         literal punctuation or operator
UPPERCASE   SQL keyword
<name>      lexical token or value supplied by a generator
```

SQL keywords are case-insensitive. Parentheses shown without quotes are grammar
grouping; quoted parentheses are SQL punctuation.

## Entry points

```ebnf
sql                   ::= [statement] {';' [statement]}

statement             ::= create_table
                        | drop_table
                        | create_sequence
                        | drop_sequence
                        | create_view
                        | drop_view
                        | insert
                        | update
                        | delete
                        | select
                        | transaction_statement
                        | set_statement
                        | reset_statement
```

## Names and lexical values

```ebnf
identifier            ::= <unquoted_identifier> | <quoted_identifier>
qualified_name        ::= identifier {'.' identifier}
column_name           ::= identifier
table_name            ::= qualified_name
sequence_name         ::= qualified_name
view_name             ::= qualified_name
alias                 ::= [AS] identifier ['(' identifier {',' identifier} ')']

integer_literal       ::= <decimal_integer>
float_literal         ::= <decimal_or_scientific_number>
string_literal        ::= <single_quoted_string>
                        | <escape_string>
                        | <dollar_quoted_string>
bit_string_literal    ::= <bit_string>
hex_string_literal    ::= <hex_string>
parameter             ::= '$' integer_literal

literal               ::= integer_literal
                        | float_literal
                        | string_literal
                        | bit_string_literal
                        | hex_string_literal
                        | TRUE
                        | FALSE
                        | NULL
                        | typed_string_literal

typed_string_literal  ::= type_name string_literal
                        | INTERVAL string_literal [interval_fields]
```

Generators must choose identifiers using PostgreSQL's keyword categories. A
word accepted as a column label is not necessarily accepted as an unquoted
table or column name.

## Types

```ebnf
type_name             ::= simple_type {array_bound}
                        | simple_type ARRAY ['[' integer_literal ']']

array_bound           ::= '[' [integer_literal] ']'

simple_type           ::= SMALLINT | INT2
                        | INTEGER | INT | INT4
                        | BIGINT | INT8
                        | REAL | FLOAT4
                        | DOUBLE PRECISION | FLOAT8
                        | FLOAT ['(' integer_literal ')']
                        | NUMERIC [numeric_modifier]
                        | DECIMAL [numeric_modifier]
                        | BOOLEAN | BOOL
                        | TEXT
                        | VARCHAR ['(' integer_literal ')']
                        | CHARACTER VARYING ['(' integer_literal ')']
                        | CHAR ['(' integer_literal ')']
                        | CHARACTER ['(' integer_literal ')']
                        | BYTEA
                        | UUID
                        | DATE
                        | TIME ['(' integer_literal ')'] [time_zone]
                        | TIMESTAMP ['(' integer_literal ')'] [time_zone]
                        | INTERVAL ['(' integer_literal ')'] [interval_fields]
                        | JSON
                        | JSONB
                        | qualified_name [type_modifier]

numeric_modifier      ::= '(' integer_literal [',' integer_literal] ')'
type_modifier         ::= '(' expr_list ')'
time_zone             ::= WITH TIME ZONE | WITHOUT TIME ZONE

interval_fields       ::= YEAR
                        | MONTH
                        | DAY
                        | HOUR
                        | MINUTE
                        | SECOND ['(' integer_literal ')']
                        | YEAR TO MONTH
                        | DAY TO HOUR
                        | DAY TO MINUTE
                        | DAY TO SECOND ['(' integer_literal ')']
                        | HOUR TO MINUTE
                        | HOUR TO SECOND ['(' integer_literal ')']
                        | MINUTE TO SECOND ['(' integer_literal ')']
```

`SERIAL`, `SMALLSERIAL`, and `BIGSERIAL` are accepted in a column type position
as PostgreSQL shorthand backed by sequences.

## Tables and constraints

```ebnf
create_table          ::= CREATE [temporary_kind] TABLE [IF NOT EXISTS]
                          table_name '(' [table_element {',' table_element}] ')'

temporary_kind        ::= TEMP | TEMPORARY | UNLOGGED

table_element         ::= column_definition | table_constraint

column_definition     ::= column_name type_name {column_constraint}

column_constraint     ::= [CONSTRAINT identifier] column_constraint_body
                        | constraint_attribute

column_constraint_body
                      ::= NOT NULL
                        | NULL
                        | UNIQUE [unique_null_treatment]
                        | PRIMARY KEY
                        | CHECK '(' expression ')'
                        | DEFAULT expression
                        | generated_identity
                        | generated_column
                        | REFERENCES table_name [column_list]
                          [match_type] [referential_actions]

generated_identity    ::= GENERATED (ALWAYS | BY DEFAULT) AS IDENTITY
                          ['(' sequence_option {sequence_option} ')']

generated_column      ::= GENERATED ALWAYS AS '(' expression ')' [STORED]

table_constraint      ::= [CONSTRAINT identifier] table_constraint_body

table_constraint_body ::= CHECK '(' expression ')' [constraint_attributes]
                        | UNIQUE [unique_null_treatment] column_list
                          [constraint_attributes]
                        | PRIMARY KEY column_list [constraint_attributes]
                        | FOREIGN KEY column_list REFERENCES table_name
                          [column_list] [match_type] [referential_actions]
                          [constraint_attributes]

column_list           ::= '(' column_name {',' column_name} ')'
unique_null_treatment ::= NULLS DISTINCT | NULLS NOT DISTINCT
match_type            ::= MATCH FULL | MATCH PARTIAL | MATCH SIMPLE

referential_actions   ::= referential_action {referential_action}
referential_action    ::= ON UPDATE key_action | ON DELETE key_action
key_action            ::= NO ACTION
                        | RESTRICT
                        | CASCADE
                        | SET NULL [column_list]
                        | SET DEFAULT [column_list]

constraint_attributes ::= constraint_attribute {constraint_attribute}
constraint_attribute  ::= DEFERRABLE
                        | NOT DEFERRABLE
                        | INITIALLY DEFERRED
                        | INITIALLY IMMEDIATE

drop_table            ::= DROP TABLE [IF EXISTS]
                          table_name {',' table_name} [drop_behavior]

drop_behavior         ::= CASCADE | RESTRICT
```

## Sequences

```ebnf
create_sequence       ::= CREATE [TEMP | TEMPORARY | UNLOGGED] SEQUENCE
                          [IF NOT EXISTS] sequence_name
                          {sequence_option}

sequence_option       ::= AS simple_integer_type
                        | INCREMENT [BY] signed_numeric
                        | MINVALUE signed_numeric
                        | NO MINVALUE
                        | MAXVALUE signed_numeric
                        | NO MAXVALUE
                        | START [WITH] signed_numeric
                        | CACHE signed_numeric
                        | CYCLE
                        | NO CYCLE
                        | OWNED BY qualified_name
                        | OWNED BY NONE

simple_integer_type   ::= SMALLINT | INTEGER | BIGINT
signed_numeric        ::= ['+' | '-'] (integer_literal | float_literal)

drop_sequence         ::= DROP SEQUENCE [IF EXISTS]
                          sequence_name {',' sequence_name} [drop_behavior]
```

Sequence functions use the ordinary function grammar. Relevant calls include
`nextval(sequence)`, `currval(sequence)`, `lastval()`, and
`setval(sequence, value [, is_called])`.

## Views

```ebnf
create_view           ::= CREATE [OR REPLACE] [TEMP | TEMPORARY]
                          [RECURSIVE] VIEW view_name [column_list]
                          AS select [check_option]

check_option          ::= WITH CHECK OPTION
                        | WITH CASCADED CHECK OPTION
                        | WITH LOCAL CHECK OPTION

drop_view             ::= DROP VIEW [IF EXISTS]
                          view_name {',' view_name} [drop_behavior]
```

## Common table expressions

```ebnf
with_clause           ::= WITH [RECURSIVE] cte {',' cte}

cte                   ::= identifier [column_list] AS [materialization]
                          '(' preparable_statement ')'
                          [search_clause] [cycle_clause]

materialization       ::= MATERIALIZED | NOT MATERIALIZED
preparable_statement  ::= select | insert | update | delete

search_clause         ::= SEARCH (DEPTH | BREADTH) FIRST BY column_name_list
                          SET column_name

cycle_clause          ::= CYCLE column_name_list SET column_name
                          [TO literal DEFAULT literal] USING column_name

column_name_list      ::= column_name {',' column_name}
```

## INSERT

```ebnf
insert                ::= [with_clause] INSERT INTO table_name [AS identifier]
                          [column_list] [override_clause] insert_source
                          [on_conflict] [returning_clause]

override_clause       ::= OVERRIDING USER VALUE
                        | OVERRIDING SYSTEM VALUE

insert_source         ::= DEFAULT VALUES
                        | select

on_conflict           ::= ON CONFLICT [conflict_target] DO NOTHING
                        | ON CONFLICT [conflict_target] DO UPDATE
                          SET set_clause {',' set_clause} [where_clause]

conflict_target       ::= '(' index_expression {',' index_expression} ')'
                          [where_clause]
                        | ON CONSTRAINT identifier

index_expression      ::= column_name | '(' expression ')'

returning_clause      ::= RETURNING target_list
```

`VALUES` is a form of `select`, so both `INSERT ... VALUES (...)` and
`INSERT ... SELECT ...` follow the same `insert_source` production.

## UPDATE and DELETE

```ebnf
update                ::= [with_clause] UPDATE table_name [alias]
                          SET set_clause {',' set_clause}
                          [from_clause] [where_clause] [returning_clause]

set_clause            ::= assignment_target '=' expression
                        | '(' assignment_target {',' assignment_target} ')'
                          '=' expression

assignment_target     ::= column_name {indirection}

delete                ::= [with_clause] DELETE FROM table_name [alias]
                          [using_clause] [where_clause] [returning_clause]

using_clause          ::= USING table_reference {',' table_reference}
where_clause          ::= WHERE expression
```

## SELECT and VALUES

```ebnf
select                ::= [with_clause] select_body
                          [order_by_clause]
                          [limit_offset_clause]
                          [locking_clause]
                        | '(' select ')'

select_body           ::= select_term
                          {(UNION | EXCEPT) [ALL | DISTINCT] select_term}

select_term           ::= select_primary
                          {INTERSECT [ALL | DISTINCT] select_primary}

select_primary        ::= select_core
                        | values_clause
                        | TABLE table_name
                        | '(' select ')'

select_core           ::= SELECT [ALL | distinct_clause] [target_list]
                          [from_clause]
                          [where_clause]
                          [group_by_clause]
                          [having_clause]
                          [window_clause]

distinct_clause       ::= DISTINCT
                        | DISTINCT ON '(' expr_list ')'

target_list           ::= target_element {',' target_element}
target_element        ::= '*'
                        | expression [AS identifier]
                        | expression identifier

values_clause         ::= VALUES values_row {',' values_row}
values_row            ::= '(' expr_list ')'

order_by_clause       ::= ORDER BY sort_item {',' sort_item}
sort_item             ::= expression [ASC | DESC] [nulls_order]
                        | expression USING operator [nulls_order]
nulls_order           ::= NULLS FIRST | NULLS LAST

limit_offset_clause   ::= limit_clause [offset_clause]
                        | offset_clause [limit_clause]

limit_clause          ::= LIMIT (expression | ALL)
                        | LIMIT expression ',' expression
                        | FETCH (FIRST | NEXT) [expression] (ROW | ROWS)
                          (ONLY | WITH TIES)

offset_clause         ::= OFFSET expression [ROW | ROWS]

group_by_clause       ::= GROUP BY [ALL | DISTINCT] group_item
                          {',' group_item}
group_item            ::= expression
                        | '('
                          ')'
                        | ROLLUP '(' expr_list ')'
                        | CUBE '(' expr_list ')'
                        | GROUPING SETS '(' group_item {',' group_item} ')'

having_clause         ::= HAVING expression

locking_clause        ::= locking_item {locking_item}
locking_item          ::= locking_strength
                          [OF qualified_name {',' qualified_name}]
                          [NOWAIT | SKIP LOCKED]
locking_strength      ::= FOR UPDATE
                        | FOR NO KEY UPDATE
                        | FOR SHARE
                        | FOR KEY SHARE
```

## FROM and joins

```ebnf
from_clause           ::= FROM table_reference {',' table_reference}

table_reference       ::= relation_reference
                        | function_table
                        | [LATERAL] '(' select ')' [alias]
                        | joined_table
                        | '(' joined_table ')' alias

relation_reference    ::= table_name [alias]

function_table        ::= [LATERAL] function_call
                          [WITH ORDINALITY] [alias]

joined_table          ::= table_reference CROSS JOIN table_reference
                        | table_reference [join_type] JOIN table_reference
                          join_qualification
                        | table_reference NATURAL [join_type] JOIN
                          table_reference
                        | '(' joined_table ')'

join_type             ::= INNER
                        | LEFT [OUTER]
                        | RIGHT [OUTER]
                        | FULL [OUTER]

join_qualification    ::= ON expression
                        | USING '(' column_name_list ')' [AS identifier]
```

## Expressions

The Tree-sitter source separates PostgreSQL expressions into `a_expr`,
`b_expr`, and `c_expr` and attaches explicit precedence to recursive
alternatives. For generator design, the same surface is represented below as
primary expressions plus operator and predicate forms. Parentheses should be
used when a generated tree's intended grouping would otherwise depend on
precedence.

```ebnf
expression            ::= primary_expression
                        | unary_expression
                        | binary_expression
                        | predicate_expression
                        | case_expression
                        | function_expression
                        | subquery_expression
                        | row_expression
                        | array_expression

primary_expression    ::= literal
                        | parameter
                        | column_reference
                        | '(' expression ')'
                        | expression '::' type_name
                        | CAST '(' expression AS type_name ')'
                        | expression COLLATE qualified_name
                        | expression {indirection}
                        | DEFAULT

column_reference      ::= column_name {'.' column_name} ['.' '*']

indirection           ::= '.' identifier
                        | '.' '*'
                        | '[' expression ']'
                        | '[' [expression] ':' [expression] ']'

unary_expression      ::= '+' expression
                        | '-' expression
                        | NOT expression
                        | operator expression

binary_expression     ::= expression arithmetic_operator expression
                        | expression comparison_operator expression
                        | expression operator expression
                        | expression AT TIME ZONE expression
                        | expression AT LOCAL
                        | expression AND expression
                        | expression OR expression

arithmetic_operator   ::= '+' | '-' | '*' | '/' | '%' | '^'
comparison_operator   ::= '=' | '<>' | '<' | '>' | '<=' | '>='
operator              ::= <postgres_operator_token>
                        | OPERATOR '(' qualified_name ')'

predicate_expression  ::= expression IS [NOT] NULL
                        | expression IS [NOT] TRUE
                        | expression IS [NOT] FALSE
                        | expression IS [NOT] UNKNOWN
                        | expression IS [NOT] DISTINCT FROM expression
                        | expression [NOT] BETWEEN [ASYMMETRIC]
                          expression AND expression
                        | expression [NOT] BETWEEN SYMMETRIC
                          expression AND expression
                        | expression [NOT] IN '(' expr_list ')'
                        | expression [NOT] IN '(' select ')'
                        | expression [NOT] (LIKE | ILIKE | SIMILAR TO)
                          expression [ESCAPE expression]
                        | expression subquery_operator (ANY | SOME | ALL)
                          '(' select ')'

subquery_operator     ::= comparison_operator | operator | LIKE | ILIKE

subquery_expression   ::= '(' select ')'
                        | EXISTS '(' select ')'
                        | ARRAY '(' select ')'

case_expression       ::= CASE [expression]
                          when_clause {when_clause}
                          [ELSE expression] END
when_clause           ::= WHEN expression THEN expression

row_expression        ::= ROW '(' [expr_list] ')'
                        | '(' expression ',' expression {',' expression} ')'

array_expression      ::= ARRAY '[' [expr_list] ']'
                        | ARRAY '[' array_expression
                          {',' array_expression} ']'

expr_list             ::= expression {',' expression}
```

### Operator precedence

From lowest binding strength to highest, the relevant Tree-sitter precedence
groups are:

```text
OR
AND
NOT
IS / ISNULL / NOTNULL
comparisons, BETWEEN, IN, LIKE, ILIKE, SIMILAR TO
generic PostgreSQL operators
+ and -
* / %
^
AT TIME ZONE / AT LOCAL
COLLATE
unary + and -
array and subscript forms
::
field selection
```

Generators do not need to reproduce every unparenthesized precedence case.
They should preserve the generated expression tree by adding parentheses.

## Function calls, aggregates, and windows

```ebnf
function_expression   ::= function_call
                          [within_group_clause]
                          [filter_clause]
                          [function_null_treatment]
                          [over_clause]
                        | common_function

function_call         ::= qualified_name '(' [function_arguments] ')'
                        | qualified_name '(' '*' ')'

function_arguments    ::= [ALL | DISTINCT] function_argument
                          {',' function_argument}
                          [order_by_clause]
                        | VARIADIC expression [order_by_clause]

function_argument     ::= expression
                        | identifier ':=' expression
                        | identifier '=>' expression

within_group_clause   ::= WITHIN GROUP '(' order_by_clause ')'
filter_clause         ::= FILTER '(' WHERE expression ')'
function_null_treatment
                      ::= IGNORE NULLS | RESPECT NULLS
over_clause           ::= OVER window_specification | OVER identifier

common_function       ::= CURRENT_DATE
                        | CURRENT_TIME ['(' integer_literal ')']
                        | CURRENT_TIMESTAMP ['(' integer_literal ')']
                        | LOCALTIME ['(' integer_literal ')']
                        | LOCALTIMESTAMP ['(' integer_literal ')']
                        | NULLIF '(' expression ',' expression ')'
                        | COALESCE '(' expr_list ')'
                        | GREATEST '(' expr_list ')'
                        | LEAST '(' expr_list ')'
                        | EXTRACT '(' extract_field FROM expression ')'
                        | POSITION '(' expression IN expression ')'
                        | SUBSTRING '(' substring_arguments ')'
                        | TRIM '(' trim_arguments ')'

extract_field         ::= YEAR | MONTH | DAY | HOUR | MINUTE | SECOND
                        | identifier | string_literal

substring_arguments   ::= expression FROM expression [FOR expression]
                        | expression FOR expression [FROM expression]
                        | expression SIMILAR expression ESCAPE expression

trim_arguments        ::= [(BOTH | LEADING | TRAILING)] trim_list
trim_list             ::= expression FROM expr_list
                        | FROM expr_list
                        | expr_list

window_clause         ::= WINDOW window_definition
                          {',' window_definition}
window_definition     ::= identifier AS window_specification

window_specification  ::= '(' [identifier]
                          [PARTITION BY expr_list]
                          [order_by_clause]
                          [frame_clause] ')'

frame_clause          ::= (RANGE | ROWS | GROUPS) frame_extent
                          [frame_exclusion]
frame_extent          ::= frame_bound
                        | BETWEEN frame_bound AND frame_bound
frame_bound           ::= UNBOUNDED PRECEDING
                        | UNBOUNDED FOLLOWING
                        | CURRENT ROW
                        | expression PRECEDING
                        | expression FOLLOWING
frame_exclusion       ::= EXCLUDE CURRENT ROW
                        | EXCLUDE GROUP
                        | EXCLUDE TIES
                        | EXCLUDE NO OTHERS
```

Ordinary scalar and aggregate functions that use `function_call` need no
additional grammar rule.

## Transactions and session settings

```ebnf
transaction_statement ::= BEGIN [WORK | TRANSACTION]
                          [transaction_modes]
                        | START TRANSACTION [transaction_modes]
                        | COMMIT [WORK | TRANSACTION] [chain]
                        | ROLLBACK [WORK | TRANSACTION] [chain]
                        | SAVEPOINT identifier
                        | RELEASE [SAVEPOINT] identifier
                        | ROLLBACK [WORK | TRANSACTION]
                          TO [SAVEPOINT] identifier

transaction_modes     ::= transaction_mode
                          { [','] transaction_mode }
transaction_mode      ::= ISOLATION LEVEL isolation_level
                        | READ ONLY
                        | READ WRITE
                        | DEFERRABLE
                        | NOT DEFERRABLE

isolation_level       ::= READ UNCOMMITTED
                        | READ COMMITTED
                        | REPEATABLE READ
                        | SERIALIZABLE

chain                 ::= AND CHAIN | AND NO CHAIN

set_statement         ::= SET [LOCAL | SESSION] setting
setting               ::= TRANSACTION transaction_modes
                        | SESSION CHARACTERISTICS AS TRANSACTION
                          transaction_modes
                        | variable_name (TO | '=') setting_value_list
                        | variable_name (TO | '=') DEFAULT
                        | TIME ZONE setting_value

reset_statement       ::= RESET variable_name
                        | RESET ALL
                        | RESET TIME ZONE
                        | RESET TRANSACTION ISOLATION LEVEL

variable_name         ::= identifier {'.' identifier}
setting_value_list    ::= setting_value {',' setting_value}
setting_value         ::= literal | identifier | signed_numeric
```

## Generator constraints outside the grammar

The formal syntax is only one layer of a useful SQL generator. The property
tests must continue to apply semantic constraints that the grammar does not
express:

- referenced schemas, tables, columns, constraints, sequences, and views must
  exist in the generated catalog state;
- operators and function arguments must be compatible with operand types;
- projection, grouping, aggregate, and window contexts must be valid;
- inserted and assigned expressions must be coercible to destination types;
- aliases must be unique and visible only in PostgreSQL-valid scopes;
- recursive CTEs, conflict targets, foreign keys, and locking clauses need
  context-specific restrictions;
- recursive grammar branches need explicit depth and collection-size limits.

The generator may deliberately choose a narrower semantic subset while still
following these productions for the shape of generated SQL.
