# pg_fake — Phase 4 compatibility implementation plan

**Status: draft for user approval. No Phase 4 task is complete.**

Phase 4 completes the planned PostgreSQL 18 SQL compatibility work for the
in-process engine. This plan contains implementation tasks, dependencies and
acceptance criteria. Phase 4 is the final planned phase.

The completed Phase 3 plan is [plan_phase3_complete.md](plan_phase3_complete.md).
The [specification](spec.md), fidelity tiers, deterministic time/RNG,
transactional behavior, native API and SQLx adapter remain the foundation.

## Evidence and baseline

This inventory was assembled from the specification, README, source map,
[Phase 3 coverage](phase3_coverage.md), [release audit](phase3_release_audit.md),
[known bugs](known_bugs.md), Phase 3 task boundaries, Rust statement/type/function
handling and rejection paths, and the checked-in upstream regression corpus.
It is a static planning audit; no new PostgreSQL test campaign was run to create
this document. Some existing support is broader than the Phase 3 summary:
`oid`, `pg_lsn`, `regclass`, session advisory locks, bounded procedural SQL,
index DDL, and temporary objects already exist and need extension/audit rather
than implementation from zero.

The recorded Phase 3 release baseline is 850 matching upstream statements,
141 skipped scripts, 32/32 Phase 2
cases and 96/96 Phase 3 cases. These are historical measurements, not newly
verified counts. Task 01 records the current baseline. A first blocker is not
an inventory of everything missing later in that script.

Cross-check the inventory against the PostgreSQL 18 reference, rather than only
against what our parser or tests already recognize:

- [SQL commands](https://www.postgresql.org/docs/18/sql-commands.html).
- [Data types](https://www.postgresql.org/docs/18/datatype.html).
- [Functions and operators](https://www.postgresql.org/docs/18/functions.html).
- [SQL syntax](https://www.postgresql.org/docs/18/sql-syntax.html),
  [queries](https://www.postgresql.org/docs/18/queries.html),
  [catalogs](https://www.postgresql.org/docs/18/catalogs.html), and
  [information schema](https://www.postgresql.org/docs/18/information-schema.html).

## Execution and acceptance rules

1. Work on the first unfinished task in order. Every task below is pending.
   Dependencies refer to tasks in this plan and the completed Phase 3 baseline.
   Letter-suffixed tasks follow their numbered task. Numeric dependency ranges
   include letter-suffixed tasks within the range only when they precede the
   dependent task; a task never depends on itself. Substeps are ordered within their task;
   do not hide partially finished work
   by marking the parent complete.
2. Task 01 expands the implementation inventory into a versioned manifest of
   command variants, types, function/operator signatures, settings and catalogs.
   Newly discovered implementation gaps inherit their owning family task.
3. Track implementation status, parser readiness, fixtures and conformance
   evidence separately. Validate both successful execution and PostgreSQL errors.
4. Valid PostgreSQL syntax must be represented by the sqlparser dependency.
   Fix the parser fork, add parser tests, and pin a reproducible revision; do not
   add SQL text rewriting/reparsing workarounds in pg_fake. Parser changes for
   later features belong to their owning task as well as the initial parser audit.
5. Each implementation task adds native and PostgreSQL 18 differential cases:
   positive results, empty results, NULLs, invalid input/SQLSTATE, parameters,
   metadata (including empty results), affected counts, and rollback/savepoints.
   Add SQLx codecs and typed round trips when representable by its public traits;
   expose native representations and document SQLx limits where it cannot encode
   the full PostgreSQL domain. Compare custom types by identity/definition, not
   equal installation-specific OID numbers. Built-in OIDs and typmods must match.
6. Extend differential property generators for meaningful combinations. For
   concurrency, use controlled schedules, verify committed histories and legal
   outcomes, and do not require the same unspecified deadlock/SSI victim.
   Preserve Tier A; keep Tier B formatting/messages and Tier C planning/order
   separate. Never relabel a deterministic in-scope mismatch as a planner issue.
7. Feature tasks run relevant focused tests, formatting, strict workspace Clippy,
   workspace regression, and the established extended property gate:
   `CHAOS_THEORY_CHECK_ITERS=10000 CHAOS_THEORY_CHECK_TIME=600s cargo test -p pg_fake_sqlx --features time --test property_tests _long`.
   Documentation-only work uses link/manifest checks instead. Benchmark affected
   common workloads and report regressions; do not claim the historical speed
   target is already met. Run PostgreSQL in isolated test databases.
8. Report changes, validation and newly working SQL examples; wait for user
   approval before marking a task complete. Do not run git commands or make
   commits without an explicit user request.

## Implementation ownership

Each family covers its documented PostgreSQL 18 forms and overloads for the
selected types. Existing support is retained and extended by the owning task.

| Area and current gap | Decision and owner |
| --- | --- |
| First-blocker-only corpus, weak accounting of untested forms and metadata | Implement audit/manifest and runner improvements, 01–02 |
| Parser omissions, e.g. sequence alteration, window exclusions, domain/DDL forms | Implement parser support for included SQL, 03 plus owning tasks |
| Omitted trailing INSERT values; timestamp offset input without seconds | Fix known compatibility bugs, 04 |
| Numeric special values, precision/scale bounds, integer/float input and casts | Implement, 05 |
| SQL `name`, internal `"char"`, incomplete OID alias/`pg_lsn` behavior | Implement value semantics and aliases for modeled objects, 06 |
| `bit`, `varbit`, `money`, network address types | Implement, 07–08 |
| Temporal input/output/range, `timetz`, formatting, date arithmetic, zone forms | Implement, 09 |
| Binary-only text ordering and missing collation propagation | Implement C/POSIX plus English/Russian profiles with case-insensitive, accent-insensitive and combined variants, 04 |
| Locale-dependent date, number and currency formatting | Implement C/POSIX, US English and Russian profiles, 07/09/16/40 |
| Named enums, domains, composites/records, row constructors, polymorphic resolution | Implement, 10–12 |
| Multidimensional/non-one-based arrays, slices, composite/domain elements | Implement, 13 |
| Built-in ranges/multiranges and SQL-defined ranges over included subtypes | Implement, 14 |
| Incomplete math/text/bytea/UUID/conditional and type utility functions | Implement, 15–16 |
| Restricted regular expressions, Unicode matching, LIKE variants | Implement within supported collation, 17 |
| Missing casts/operator overloads/common-type and parameter resolution | Implement across included types, 05–17 and 40 |
| `FETCH FIRST/NEXT`, ties, `TABLE`, row comparisons, query composition | Implement, 18 |
| TABLESAMPLE BERNOULLI and REPEATABLE | Implement, 18 |
| Grouping sets, ROLLUP/CUBE/GROUPING, aggregate DISTINCT/ordered sets/statistics | Implement, 19 |
| Ordinary user-defined aggregates with transition/initial/final state | Implement, 34A after general functions |
| Window EXCLUDE, direct final-order expressions, remaining valid frames | Implement, 20 |
| General SRFs, multi-array unnest, ROWS FROM, SELECT-list SRFs, lateral functions | Implement, 21 |
| Recursive SEARCH/CYCLE and remaining valid CTE/materialization forms | Implement, 22 |
| JSON operators/functions/record conversion, JSONPath and SQL/JSON | Implement, 23–24 |
| Broader INSERT/UPDATE/DELETE assignments and PG18 OLD/NEW RETURNING | Implement, 25 |
| MERGE, including PG18 action and RETURNING surface | Implement, 26 |
| CREATE TABLE AS/LIKE, SELECT INTO, ON COMMIT DELETE ROWS | Implement, 27 |
| Dependency-aware DROP/CASCADE/RESTRICT, ALTER/schema/comment lifecycle | Implement for included objects, 28 |
| ALTER SEQUENCE, identity alterations, ownership/default dependencies | Implement, 29 |
| Stored/virtual generated columns | Implement PostgreSQL 18 semantics, 30 |
| Index expressions, key-count limits, predicates, uniqueness/null options and built-in methods | Implement logical semantics and method validation with one shared index implementation, 31 |
| Deferrable unique/PK/exclusion constraints and temporal constraints | Implement, 32 |
| Read-only views extended to recursive/updatable/materialized views | Implement, 33 |
| SQL functions and broader PL/pgSQL, procedures and trigger events | Implement bounded, enumerated language surface, 34–35 |
| Schema event triggers and inspection functions | Implement DDL start/end, sql_drop and table_rewrite for included DDL, 35A |
| Internal COMMIT/ROLLBACK in procedures/DO, including AND CHAIN | Implement PostgreSQL-permitted contexts and cross-transaction execution, 36A |
| Transaction access modes, chaining, snapshot import/export | Implement, 36 |
| Remaining LOCK TABLE modes/NOWAIT; advisory and cancellation fidelity | Implement, 37 |
| SQL PREPARE/EXECUTE/DEALLOCATE, DISCARD, cursors | Implement, 38 |
| COPY streams and LISTEN/NOTIFY within one Db | Implement, 39 |
| Session settings, metadata, catalogs and information_schema for modeled objects | Implement, 40 |
| Deferred Phase 3 Squirrel campaign triage/dedup/regression work | Implement, 41 |
| Cross-feature conformance and implementation documentation | Implement, 42 |

## Milestone A — establish the closure contract

### Task 01 — Inventory and executable support manifest [PENDING]

**Dependencies:** Phase 3.

- [ ] Capture current regression/property/application baselines and promote the
  three known bugs into failing reproductions.
- [ ] Create `docs/compatibility.md` and a checked-in implementation manifest
  with stable feature IDs, PostgreSQL source/version, exact form/signature,
  current behavior, owning task and test references.
- [ ] Enumerate the planned command variants, built-in types, function/operator
  overloads, settings and catalog families against a pinned PostgreSQL export.
- [ ] Inspect corpus statements beyond their first blocker, executor fallthroughs,
  function-resolution failures, adapter integration and Phase 3 fuzzing findings
  for gaps in the planned features.
- [ ] Check manifest IDs, task links and test references; retain existing matching
  SQL as conformance evidence.

**DoD:** each implementation gap has an owning task and concrete acceptance
cases. Check in the catalog export and manifest for reproducible follow-up work.

### Task 02 — Conformance runner and corpus reachability [PENDING]

**Dependencies:** 01.

- [ ] Compare metadata for zero-row queries, built-in OIDs/typmods, parameter
  types, ordered ties, row multiplicity, SQLSTATE and affected-row counts.
- [ ] Supply explicit isolated fixtures for psql variables, COPY input and
  prerequisite objects; preserve upstream SQL sources and statement provenance.
- [ ] Exercise statements beyond first blockers using independent valid setups;
  never continue inside a poisoned transaction or silently rewrite tested SQL.
- [ ] Add Phase 4 conformance cases and reports separating parser, fixture,
  execution and result mismatches.

**DoD:** incorrect success, result metadata, SQLSTATE or skipped implementation
cases fail the gate. Resolve parser and fixture blockers before phase exit.

### Task 03 — Parser backlog [PENDING]

**Dependencies:** 01–02.

- [ ] Reproduce the 15 recorded parser blockers against the pinned fork and
  PostgreSQL 18 for the planned SQL forms.
- [ ] Add AST/parser/display/visitor tests for currently missing included forms,
  starting with ALTER SEQUENCE, domains, window EXCLUDE and new RETURNING forms.
- [ ] Record dependency revision and parser capability by manifest ID. Later
  tasks extend the fork whenever their syntax is still missing.

**DoD:** no known initial included parser blocker lacks a fix or an explicit
owning later task with its concrete syntax listed in the manifest. Do not build
a project-local substitute parser.

### Task 04 — Known correctness bugs and selected collations [PENDING]

**Dependencies:** 01–03.

- [ ] Accept omitted trailing INSERT values with defaults; retain errors for
  invalid arity, explicit target lists and default/identity combinations.
- [ ] Accept timestamptz input such as `2024-07-01 12:00+00`.
- [ ] Implement C/POSIX and English/Russian collation profiles, using PostgreSQL
  18 `en-x-icu` and `ru-x-icu` as the language oracles. Pin/document the oracle's
  provider and data versions and preserve the pure Rust, in-process architecture;
  never substitute host-default sorting or hand-written alphabet ordering.
- [ ] Carry collation identity through database defaults, column definitions,
  expression COLLATE, derived expressions, prepared metadata and catalog objects;
  match implicit/explicit resolution and conflicting-collation errors.
- [ ] Implement case-insensitive, accent-insensitive and combined English/Russian
  variants using PostgreSQL's nondeterministic-collation semantics. Compare
  equivalent strings consistently in
  equality, joins, grouping, DISTINCT, unique constraints and ON CONFLICT;
  ensure hash keys agree with collation equality. Do not emulate this with
  blanket lowercasing or accent stripping. Test permitted and prohibited pattern operations against
  PostgreSQL 18 for each profile.
- [ ] Support named CREATE COLLATION definitions for the selected profiles,
  including FROM aliases and the corresponding rename/drop/dependency behavior.
  Enumerate accepted locale/strength/case options and provider versions in the
  manifest.
- [ ] Test ordering, comparisons, grouping, DISTINCT, unique keys and pattern/case
  operations, including English accents, Russian Cyrillic/ё, mixed scripts and
  normalization. Later type/query tasks extend this behavior to their features.
  Verify that normalization and Russian letter distinctions follow each
  profile's PostgreSQL behavior.

**DoD:** the trailing-value and timestamp bugs are fixed with differential
evidence; the selected collation profiles pass comparisons and constraint tests.

## Milestone B — values, casts and functions

### Task 05 — Numeric and primitive type fidelity [PENDING]

**Dependencies:** 04.

- [ ] Model numeric NaN and infinities, PostgreSQL precision/scale ranges,
  negative scales, rounding, comparison and equality/hash consistency.
- [ ] Close Boolean/integer/float/numeric input, overflow, casts, signed zero,
  infinity/NaN and arithmetic/operator gaps found in the upstream files.
- [ ] Propagate behavior into grouping, unique keys, aggregates and codecs;
  use an explicit native representation when BigDecimal cannot carry a value.

**DoD:** generated boundary/round-trip cases cover all cast contexts and type
modifiers; SQLx inability to represent special values is an explicit codec
error/limitation, not lossy conversion.

### Task 06 — Names, object identifiers and LSN values [PENDING]

**Dependencies:** 05.

- [ ] Implement name and internal `"char"` input, length/encoding and comparison;
  extend existing oid, regclass and pg_lsn without regressing current support.
- [ ] Implement regtype/regproc/regprocedure/regoper/regoperator/regnamespace
  and their array/I/O helpers for modeled objects and built-in operators.
- [ ] Check numeric OID input, ambiguous names, search_path, renames/drops,
  catalog visibility and prepared metadata.

**DoD:** catalog names have correct type metadata; no text-cast workaround is
required for SQLx paths that support these native types.

### Task 07 — Bit strings and money [PENDING]

**Dependencies:** 05.

- [ ] Implement bit/varbit lengths, literals, casts, binary/string operators,
  indexing functions and aggregates, including padding and overflow cases.
- [ ] Implement money value semantics, arithmetic, casts and input/output under
  C/POSIX, US English and Russian `lc_monetary` profiles, including currency
  symbols, separators and placement. Add the semantic setting when this type
  support lands and test rollback.
- [ ] Include arrays, parameter inference, native values and adapter capability
  documentation; later Task 13 generalizes array shape.

**DoD:** complete the included bit/money signature lists with differential
boundary and storage round trips, not just literal parsing.

### Task 08 — Network address types [PENDING]

**Dependencies:** 05.

- [ ] Implement inet/cidr, macaddr/macaddr8, PostgreSQL text/binary forms and casts.
- [ ] Implement address/subnet arithmetic, comparisons, containment and documented
  functions, with IPv4/IPv6 boundaries and invalid masks/host bits.
- [ ] Integrate storage, keys, arrays and native/SQLx type support.

**DoD:** every included signature has positive and negative differential cases.

### Task 09 — Temporal values and session-dependent formats [PENDING]

**Dependencies:** 05–08.

- [ ] Remove chrono's narrower calendar range as a core fidelity limitation;
  support PostgreSQL date/timestamp ranges, BC, infinities and typmods.
- [ ] Implement timetz, full included date/time/interval input forms, DateStyle,
  IntervalStyle, interval-valued/POSIX time zones and DST transition semantics.
- [ ] Complete arithmetic, EXTRACT/date_part, date_trunc/date_bin, age,
  OVERLAPS, constructors, to_date/to_timestamp/to_char and formatting overloads.
- [ ] Implement `lc_time` and English/Russian localized month/day names in the
  PostgreSQL format patterns that request them. Test ordinary versus translated
  patterns, case/abbreviations and parsing where PostgreSQL accepts those forms;
  keep DateStyle, TimeZone and locale settings semantically distinct.

**DoD:** mock time and transaction/statement/clock hierarchy remain correct;
codec range limits are explicit, and both parsing and semantic setting
rollback are checked across the selected regional profiles.

### Task 10 — User type identity and enums [PENDING]

**Dependencies:** 06–09.

- [ ] Generalize type identity/catalog lookup to registered types, without
  scattering casts outside the central coercion layer.
- [ ] Implement enum CREATE/ALTER/DROP, labels/order, before/after insertion,
  comparisons, casts, arrays, dependencies and transaction visibility.
- [ ] Match PostgreSQL restrictions on using enum labels added in a transaction.

**DoD:** enum values survive prepared queries and snapshots with coherent type
metadata; renames/drops invalidate dependencies correctly.

### Task 11 — Domains [PENDING]

**Dependencies:** 10.

- [ ] Implement CREATE/ALTER/DROP DOMAIN over included types, constraints,
  defaults, NOT NULL, validation and dependency checks.
- [ ] Implement domain/base resolution in expressions, assignments, parameters,
  arrays and NULL cases; test changing constraints over existing data.

**DoD:** domain violations match PostgreSQL SQLSTATE and timing, including
savepoints and arrays; no domain constraint is silently erased by coercion.

### Task 12 — Composite values and row semantics [PENDING]

**Dependencies:** 10–11.

- [ ] Implement named composite types, table row types and anonymous ROW records,
  field access/assignment, expansion, text I/O and ALTER TYPE lifecycle.
- [ ] Implement row comparisons, row IS NULL/IS NOT NULL, composite equality and
  ordering, row/subquery assignment and polymorphic type resolution.
- [ ] Integrate nested composite/domain values, arrays and parameter/result
  metadata; preserve dropped-field and dependency behavior.

**DoD:** distinguish NULL composite values from rows whose fields are all NULL;
exercise nested type round trips and ambiguous/invalid row operations.

### Task 13 — Complete array representation [PENDING]

**Dependencies:** 05–12.

- [ ] Represent PostgreSQL dimensions/lower bounds independently of flat elements;
  support explicit bounds, multidimensional literals/constructors and slices.
- [ ] Implement subscript/slice reads and writes, extension and NULL/empty shapes,
  dimension-sensitive comparison, concatenation and all included array functions.
- [ ] Support every included element type, including domains/composites, and
  multidimensional native I/O; make SQLx Vec dimensional limits explicit.

**DoD:** no one-based/one-dimensional assumption remains in included core
operations. Multi-array unnest execution belongs to 21.

### Task 14 — Ranges and multiranges [PENDING]

**Dependencies:** 09–13.

- [ ] Implement built-in range/multirange types, bounds, empty/infinite cases,
  canonicalization, constructors, casts and included operators/functions.
- [ ] Implement SQL-defined ranges over included ordered subtypes, their generated
  multirange types and supported collation.
- [ ] Integrate range aggregation, arrays, indexes/constraints and codecs.

**DoD:** boundary intersection/adjacency/normalization tests compare PostgreSQL;
range values are ready for exclusion and temporal constraints in 32.

### Task 15 — Scalar math, binary and UUID function closure [PENDING]

**Dependencies:** 05–14.

- [ ] Implement remaining documented mathematical/trigonometric, bytea/hash,
  conversion and UUID signatures for included types, including PostgreSQL 18
  UUID generation/inspection forms and seeded random distributions.
- [ ] Verify overflow, domains, strictness, NULL propagation and overload resolution;
  preserve deterministic mock clock/RNG behavior and classify nondeterminism.

**DoD:** all implementation-manifest signatures in these families pass their
differential cases.

### Task 16 — Text, formatting and conditional functions [PENDING]

**Dependencies:** 09, 12, 15.

- [ ] Close Unicode string, substring/overlay/trim/padding, quoting, encoding,
  format and byte/character length behavior under the supported collations and
  UTF-8, with C/POSIX, US English and Russian localized formatting.
- [ ] Implement `lc_numeric` and complete localized date/number/currency format
  patterns across C/POSIX, US English and Russian profiles, including decimal
  and group separators, signs, currency and padding. Pin the PostgreSQL oracle's
  locale-data versions; keep pg_fake behavior independent of the host locale.
  Text returned by formatting functions is a SQL result subject to Tier A;
  Tier B numeric/float default rendering does not waive these result checks.
- [ ] Complete conditional/common-type behavior (CASE, COALESCE, NULLIF,
  GREATEST/LEAST), unknown literals and short-circuit evaluation.
- [ ] Cover statement/transaction-stable versus volatile function evaluation in
  scalar subqueries, prepared queries and resumed lock waits.

**DoD:** the manifest enumerates every overload; invalid UTF-8/NUL and quoting
errors match PostgreSQL rather than relying on Rust string defaults.

### Task 17 — PostgreSQL pattern semantics [PENDING]

**Dependencies:** 04, 16.

- [ ] Complete LIKE/ILIKE/SIMILAR TO, escapes and ANY/ALL operator forms.
- [ ] Implement PostgreSQL-compatible regular expression syntax, flags, captures
  and scalar/set-returning regex functions, including Unicode with the selected
  collation profiles.
- [ ] Preserve PostgreSQL rejection behavior for invalid patterns and parameter
  types; register regex SRFs for the general execution path in 21.

**DoD:** remove the current ASCII-only regex/ILIKE boundary; do not assume the
Rust regex dialect is PostgreSQL's dialect.

## Milestone C — query and JSON closure

### Task 18 — Query clauses and composition [PENDING]

**Dependencies:** 12–17.

- [ ] Implement FETCH FIRST/NEXT ONLY/WITH TIES, TABLE query sources and ORDER BY
  USING built-in operators, with NULL/parameter/offset cases.
- [ ] Close remaining DISTINCT ON, VALUES, set-operation, alias, subquery and
  row-expression gaps using upstream select/join/subselect/union cases.
- [ ] Implement TABLESAMPLE BERNOULLI (percentage) [REPEATABLE (seed)] with
  independent row selection before WHERE/join filtering, PostgreSQL argument
  coercion/validation and legal relation restrictions. Include ordinary and
  temporary tables here, integrating materialized views in Task 33. Match MVCC
  visibility and rescan behavior; evaluate parameters/volatile expressions with
  PostgreSQL's timing. Keep sampling state scoped to the session/query and
  honor deterministic RNG configuration.
- [ ] Verify 0/100 percent, empty relations, NULL/invalid arguments, prepared
  parameters and repeated samples of unchanged data with the same seed. Use
  statistical/property checks for intermediate probabilities, seed variation
  and WHERE composition; do not compare exact random row identities across
  engines. Document this fidelity exception explicitly in the manifest.
- [ ] Verify legal nested query combinations, output types and binding on empty
  inputs; distinguish PostgreSQL errors from cross-dialect AST branches.

**DoD:** all included query-clause manifest entries pass in one-shot and
prepared execution. Grouping, window, SRF and CTE extensions follow below.

### Task 19 — Grouping and aggregates [PENDING]

**Dependencies:** 14, 18.

- [ ] Implement GROUPING SETS, ROLLUP, CUBE, GROUPING and valid GROUP BY modifiers.
- [ ] Complete aggregate DISTINCT/ORDER BY/FILTER combinations and built-in
  statistical, Boolean, bit, collection, ordered-set and hypothetical-set
  aggregates for included types; ordinary user-defined aggregates follow in
  Task 34A, after general function execution.
- [ ] Check empty groups, duplicate grouping sets, NULL keys, HAVING, numeric
  precision and functional dependencies.

**DoD:** grouped output types, multiplicity and error cases match PostgreSQL;
aggregate work includes nested-query and window interactions.

### Task 20 — Window closure [PENDING]

**Dependencies:** 18–19.

- [ ] Implement EXCLUDE CURRENT ROW/GROUP/TIES/NO OTHERS and remaining valid
  offset/frame type combinations.
- [ ] Audit named windows, expressions used only in final ORDER BY, peer groups,
  default frames and interactions with DISTINCT/grouping/subqueries.
- [ ] Match PostgreSQL 18 SQLSTATEs for invalid window definitions.

**DoD:** regression/property cases exercise every frame mode and exclusion;
window evaluation is not accidentally tied only to projection expressions.

### Task 21 — General set-returning execution [PENDING]

**Dependencies:** 13, 17, 18–20.

- [ ] Unify table-function binding/execution, ROWS FROM, WITH ORDINALITY,
  multiple-array unnest and implicit/explicit LATERAL.
- [ ] Implement SELECT-list SRFs and PostgreSQL row-expansion/NULL-padding rules;
  reject illegal aggregate/conditional/SRF combinations.
- [ ] Implement generate_series, generate_subscripts and included text/regex/array
  SRFs with limits, cancellation and correct correlation.

**DoD:** correlated SRFs, empty sets and multiple SRFs compose with joins,
aggregates/windows and prepared metadata without repeated volatile execution.

### Task 22 — Recursive CTE closure [PENDING]

**Dependencies:** 12–13, 18–21.

- [ ] Implement SEARCH and CYCLE with their generated columns and type behavior.
- [ ] Audit recursive type/collation resolution, UNION duplicate elimination,
  demand-driven termination and nested references.
- [ ] Close valid MATERIALIZED/NOT MATERIALIZED and data-modifying CTE combinations;
  retain PostgreSQL restrictions on recursion and multiple writes.

**DoD:** cyclic/acyclic graph properties, timeouts and mutation rollback match
PostgreSQL; never promise identical visitation order without ORDER BY.

### Task 23 — Existing JSON/JSONB surface closure [PENDING]

**Dependencies:** 05, 12–13, 19, 21.

- [ ] Complete operators, constructors, mutation/path functions, aggregation,
  conversion and json/jsonb record/recordset population for included types.
- [ ] Match raw-json preservation versus jsonb normalization, duplicate keys,
  numeric limits, Unicode, null/missing values and error behavior.

**DoD:** every non-JSONPath/non-SQL-JSON signature in the manifest passes,
including composite records, prepared parameters and set-returning contexts.

### Task 24 — JSONPath and SQL/JSON [PENDING]

**Dependencies:** 21, 23.

- [ ] Implement jsonpath values, strict/lax path evaluation, variables,
  predicates, documented methods and jsonb path operators/functions.
- [ ] Implement PostgreSQL 18 SQL/JSON constructors, predicates, query/value/exists
  expressions, serialization and JSON_TABLE, including nested columns.
- [ ] Match SQL NULL versus JSON null, RETURNING conversions, ON EMPTY/ON ERROR,
  wrappers, uniqueness and timezone-aware overload behavior.

**DoD:** cover the complete included syntax/signature matrix, not just `@?`/`@@`.
See [PostgreSQL 18 JSON reference](https://www.postgresql.org/docs/18/functions-json.html)
for the versioned acceptance surface.

## Milestone D — mutations and schema semantics

### Task 25 — DML closure [PENDING]

**Dependencies:** 12–24.

- [ ] Implement tuple/subquery/field/slice assignments and remaining default,
  identity override, INSERT SELECT, UPDATE FROM and DELETE USING combinations.
- [ ] Implement PostgreSQL 18 OLD/NEW RETURNING and output aliases across INSERT,
  UPDATE, DELETE and ON CONFLICT, with correct expression visibility.
- [ ] Audit ON CONFLICT arbiter inference, duplicate source rows, NULL semantics,
  cardinality errors and post-wait predicate/expression reevaluation.

**DoD:** atomicity, counts, RETURNING metadata and controlled concurrent
outcomes match; later index/generated/view tasks add their new combinations.

### Task 26 — MERGE [PENDING]

**Dependencies:** 25.

- [ ] Implement all PostgreSQL 18 MATCHED/BY TARGET/BY SOURCE action forms,
  ordered conditions, source scopes and duplicate-match errors on ordinary tables.
- [ ] Implement RETURNING OLD/NEW and merge_action(), defaults/identity overrides,
  trigger/constraint hooks and transaction/lock-wait behavior.
- [ ] Add updatable-view targets when 33–35 land; mark that integration explicitly
  owned there rather than treating initial table support as all MERGE support.

**DoD:** action combinations, no-op rows, counts, concurrency and error rollback
match the [PostgreSQL 18 MERGE reference](https://www.postgresql.org/docs/18/sql-merge.html)
for the planned table and view targets.

### Task 27 — Table creation and temporary lifecycle [PENDING]

**Dependencies:** 25–26.

- [ ] Implement CREATE TABLE AS, SELECT INTO, WITH [NO] DATA and CREATE TABLE LIKE
  options for included objects/features.
- [ ] Complete temporary ON COMMIT PRESERVE/DELETE/DROP behavior, namespaces,
  session cleanup and transaction/savepoint interaction.
- [ ] Implement logical unlogged-table behavior.

**DoD:** defaults/constraints/identities are copied or newly allocated exactly
as the requested LIKE options require; snapshot forks exclude session objects.

### Task 28 — Object lifecycle and dependency closure [PENDING]

**Dependencies:** 10–14, 27.

- [ ] Generalize dependency tracking for included objects; implement explicit
  RESTRICT/CASCADE and multi-object drops with proper transaction visibility.
- [ ] Close CREATE SCHEMA embedded elements, schema moves/renames/drop (including
  public), ALTER TABLE column/type/default/constraint variants and comments on
  included object kinds.
- [ ] Make prepared/view/function dependencies survive renames and fail/rebind
  correctly after incompatible alterations.

**DoD:** dependency cycles, savepoints, cross-session DDL waits and snapshots
are covered; newly added objects in later tasks register in the same machinery.

### Task 29 — Sequences and identity lifecycle [PENDING]

**Dependencies:** 03, 25, 28.

- [ ] Implement ALTER SEQUENCE, RESTART, OWNED BY, data types, bounds/cycle/cache
  options, identity ADD/SET/DROP and restart/options in ALTER TABLE.
- [ ] Complete computed sequence names and compound defaults with correct
  early/late binding, dependency drops and cross-persistence restrictions.
- [ ] Model observable multi-session allocation/cache behavior, transactional
  restart versus nontransactional nextval/setval, currval/lastval and snapshots.

**DoD:** concurrent allocation, restart blocking, rollback and identity override
cases pass; never ignore CACHE if it changes observable allocation.

### Task 30 — Generated columns [PENDING]

**Dependencies:** 25, 27–29.

- [ ] Implement stored and virtual generated columns, expression restrictions,
  dependency tracking and PostgreSQL 18 defaults for omitted generation kind.
- [ ] Integrate ALTER, LIKE, writes, RETURNING, constraints, indexes and trigger
  order; verify ordinary versus generated/default values across updates.

**DoD:** follow the [PostgreSQL 18 generated-column contract](https://www.postgresql.org/docs/18/ddl-generated-columns.html),
including invalid expressions and supported type/function restrictions; do not
assume older PostgreSQL releases' stored-only behavior.

### Task 31 — Logical index semantics [PENDING]

**Dependencies:** 17, 28–30.

- [ ] Remove the artificial four-key limit up to PostgreSQL's supported limit;
  support expressions, included columns, valid predicates and key options.
- [ ] Accept built-in USING btree/hash/gist/spgist/gin/brin for included types,
  with btree as the omitted-method default. Use one shared internal index
  implementation for every method; retain the declared method in catalog
  metadata. Do not require data to support B-tree ordering merely because the
  internal representation is shared. Correct scans may fall back to evaluating
  predicates over rows while preserving query results.
- [ ] Build a PostgreSQL 18 capability matrix for each method and its built-in
  operator classes over included types: default/explicit class resolution,
  schema qualification, type compatibility, multicolumn/INCLUDE support,
  ordering/NULL-placement options, UNIQUE and exclusion-constraint eligibility.
  Reject invalid combinations with matching SQLSTATE; for example, a UNIQUE
  GIN index must not become legal because the shared implementation can enforce
  uniqueness. Include built-in class logical comparison/equality semantics.
- [ ] Complete unique NULLS DISTINCT/NOT DISTINCT, arbiter inference and included
  built-in comparison/collation behavior; preserve key/equality consistency.
- [ ] Implement ALTER/DROP index dependency behavior for the built-in methods.

**DoD:** uniqueness and conflict detection agree across ordinary writes,
ON CONFLICT, MERGE, generated values, savepoints and concurrent sessions.
Differential fixtures cover valid and invalid definitions for all six methods,
arrays/JSONB/ranges and other included types, catalog method/class identities,
rollback/dependencies and constraints through the shared index implementation.

### Task 32 — Constraint timing and temporal constraints [PENDING]

**Dependencies:** 14, 25, 28–31.

- [ ] Complete deferrable primary/unique/exclusion constraints and SET CONSTRAINTS
  timing, names, validation and interactions with conflict arbitration.
- [ ] Implement exclusion predicates/operators for included types using the shared
  index machinery.
- [ ] Implement PostgreSQL 18 WITHOUT OVERLAPS keys and PERIOD foreign keys;
  complete valid FK actions/match modes and NOT VALID/VALIDATE variants.

**DoD:** generated histories cover immediate/deferred violations, rollback and
multi-session constraints. Consult the versioned
[CREATE TABLE reference](https://www.postgresql.org/docs/18/sql-createtable.html)
for temporal and constraint syntax; do not implement SQL-standard modes that
PostgreSQL itself rejects.

### Task 33 — View families [PENDING]

**Dependencies:** 22, 26, 28–32.

- [ ] Implement recursive views and automatically updatable views, defaults,
  LOCAL/CASCADED CHECK OPTION and mutations/RETURNING through nested views.
- [ ] Implement materialized view creation, WITH NO DATA, REFRESH (including
  CONCURRENTLY's logical locking/unique-index requirements), indexes and lifecycle.
- [ ] Integrate Task 18's BERNOULLI sampling on materialized views, including
  populated/unpopulated state and visibility after refresh.
- [ ] Integrate MERGE targets where PostgreSQL permits them; trigger-based
  updates follow in Task 35.

**DoD:** check-option failures, stale versus refreshed contents, concurrent
visibility and rollback behave correctly; no mutation silently bypasses checks.

## Milestone E — routines and session behavior

### Task 34 — SQL and PL/pgSQL routines [PENDING]

**Dependencies:** 12, 21, 28, 33.

- [ ] Implement SQL scalar/set-returning functions and SQL procedures, argument
  defaults/names/IN/OUT/INOUT/VARIADIC, overloads, RETURNS TABLE, search_path,
  strictness, volatility, ALTER/DROP ROUTINE and CALL output behavior.
- [ ] Extend PL/pgSQL functions/procedures/DO with typed and %TYPE/%ROWTYPE local
  variables, assignment, SELECT INTO [STRICT], PERFORM, nested blocks, IF/CASE,
  LOOP/WHILE/FOR/FOREACH, EXIT/CONTINUE, RETURN/NEXT/QUERY and RAISE.
- [ ] Implement ASSERT condition [, message] and plpgsql.check_asserts
  (default on). False or NULL conditions raise ASSERT_FAILURE (P0004); true
  conditions do not evaluate the message expression, and disabled assertions
  evaluate neither expression. Preserve ordinary errors during expression
  evaluation and PostgreSQL's default message when the supplied message is NULL.
  Match exception semantics: WHEN OTHERS does not catch ASSERT_FAILURE, but
  an explicit assertion handler does; preserve subtransaction rollback and
  diagnostics. Add differential cases for true/false/NULL, custom/NULL messages,
  volatile side effects, evaluation errors, nested handlers and disabled checks.
- [ ] Implement EXECUTE ... USING/INTO and exception blocks (SQLSTATE/SQLERRM),
  FOUND; dynamic SQL uses the normal parser/binder. Explicit procedural cursor
  operations follow in Task 38.
- [ ] Implement RECORD locals whose field names, types and arity follow each
  assigned row, including SELECT/RETURNING INTO, EXECUTE INTO, FOR query results
  and whole-record assignment. Re-resolve field access after shape changes;
  distinguish unassigned records, known structures containing NULLs and missing
  fields, matching PostgreSQL errors and no-row/STRICT behavior. Support field
  reads/writes with normal coercion and keep fixed %ROWTYPE/composite targets
  subject to their declared structure. Preserve local record state through
  caught exceptions according to PL/pgSQL variable semantics.
- [ ] Implement GET [CURRENT] DIAGNOSTICS (ROW_COUNT, PG_CONTEXT,
  PG_ROUTINE_OID) and GET STACKED DIAGNOSTICS (RETURNED_SQLSTATE, COLUMN_NAME,
  CONSTRAINT_NAME, PG_DATATYPE_NAME, MESSAGE_TEXT, TABLE_NAME, SCHEMA_NAME,
  PG_EXCEPTION_DETAIL, PG_EXCEPTION_HINT, PG_EXCEPTION_CONTEXT). Support multiple
  assignments with = or :=, target coercion, absent fields as empty strings,
  and errors for stacked diagnostics outside an exception handler.
  Carry structured metadata from engine errors and RAISE ... USING, preserving
  caller-supplied fields, nested handler restoration and bare RAISE rethrows.
  Distinguish current call stacks from the stack captured at error time; resolve
  routine OIDs against the catalog rather than promising equal numeric OIDs
  across databases. Preserve SQLSTATE, object identities and row counts exactly;
  generated message/detail/hint wording and stack rendering remain Tier B.
- [ ] Preserve subtransaction semantics and routine dependency/definition checks.
  Procedure/DO transaction control follows in Task 36A.

**DoD:** every enumerated language form has differential control-flow/error
coverage; procedural transaction control is assigned to Task 36A, explicit
procedural cursors to Task 38.
Add differential cases for constraint-specific handling, absent metadata,
user-raised fields, nested exceptions/rethrows, dynamic SQL row counts and
current versus captured stacks; normalize only Tier B text and database-local
OIDs, preserving catalog identity checks.
Test sequential changes of record field names/types/count, dynamic and static
assignments, missing/unassigned field access, no-row results and exception
recovery; repeat expressions across shape changes to catch stale bindings.
Split implementation into the substeps above without declaring unrestricted
PL/pgSQL support or silently accepting unused definition attributes.

### Task 34A — Ordinary user-defined aggregates [PENDING]

**Dependencies:** 10–13, 19–20, 28, 34. Execute directly after Task 34.

- [ ] Extend sqlparser for included CREATE [OR REPLACE] AGGREGATE forms and
  ALTER/DROP AGGREGATE; support concrete fixed signatures, zero/multiple inputs
  and equivalent legacy BASETYPE syntax. Definition options are exactly SFUNC,
  STYPE, optional INITCOND and optional FINALFUNC.
- [ ] Store definitions in the transactional catalog; resolve schemas/overloads
  and parameter/result types, validate transition/final function signatures,
  and track dependencies through replace/rename/schema moves/drop, prepared
  statements, savepoints and database snapshots.
- [ ] Generalize hardcoded aggregate recognition and input/state handling to
  invoke supported built-in or SQL/PL/pgSQL transition/final functions. Match
  initial-state parsing, strict/non-strict NULL handling, first-row initialization,
  empty groups, state/result coercion and final-function invocation.
- [ ] Reuse grouping, DISTINCT, FILTER and argument ORDER BY execution, including
  collations and composite/domain/array states. Support ordinary aggregate OVER
  calls by evaluating the applicable frame.
- [ ] Add generated differential cases plus focused tests for overload ambiguity,
  type errors, volatile/side-effecting callbacks, timeout/lock-wait resumption,
  rollback and dependency invalidation. Verify callback invocation counts where
  PostgreSQL guarantees them.
- [ ] Benchmark representative grouped and windowed aggregates and add native,
  prepared and SQLx fixtures. Task 40 integrates aggregate catalog introspection.

**DoD:** ordinary custom aggregates compose with existing SQL and routines;
an example acceptance fixture
defines an SQL multiplication function, registers `product(numeric)` with initial
state `1`, and checks grouped, filtered, ordered and windowed products, NULLs,
empty input and transaction rollback against PostgreSQL 18.

### Task 35 — DML trigger closure [PENDING]

**Dependencies:** 26, 30, 32–34, 34A.

- [ ] Implement BEFORE/AFTER/INSTEAD OF events, row/statement triggers,
  INSERT/UPDATE/DELETE/TRUNCATE, UPDATE OF, WHEN, arguments and TG variables.
- [ ] Implement transition tables, constraint-trigger deferral, firing order,
  recursive effects, enable/disable modes and lifecycle/dependencies.
- [ ] Integrate view mutations and MERGE action triggers, generated columns,
  cascades and exception rollback; schema event triggers follow in 35A.

**DoD:** controlled trigger chains and statement/row transitions match PostgreSQL;
routine bodies are validated even when a test never fires the trigger.

### Task 35A — Schema event triggers [PENDING]

**Dependencies:** 21, 28–35, including 34A. Execute directly after Task 35.

- [ ] Extend the parser and catalog for CREATE/ALTER/DROP EVENT TRIGGER,
  rename/enable/disable modes, dependency tracking and PL/pgSQL functions with
  return type event_trigger. Validate signatures, event names and command-tag
  filters.
- [ ] Invoke ddl_command_start, ddl_command_end, sql_drop and table_rewrite for
  included DDL. Use PostgreSQL's command/event eligibility matrix, alphabetical
  firing order, TG_EVENT/TG_TAG and nested-command contexts. Event-trigger DDL
  itself must follow PostgreSQL's exemption from DDL hooks.
- [ ] Implement pg_event_trigger_ddl_commands(),
  pg_event_trigger_dropped_objects(), pg_event_trigger_table_rewrite_oid() and
  pg_event_trigger_table_rewrite_reason(), with correct metadata, per-command
  object identities, cascade-drop lists and context restrictions. Preserve the
  opaque pg_ddl_command column's type and PostgreSQL restrictions on direct
  output.
- [ ] Model table_rewrite events/reason bits when included ALTER TABLE/TYPE
  operations would rewrite in PostgreSQL, regardless of the fake's storage
  implementation.
- [ ] Integrate semantic event-trigger enablement settings and transactional
  catalog visibility. Hook errors must cancel/roll back the DDL and its hook
  side effects; cover savepoint recovery, exceptions, nested DDL, timeouts and
  lock-wait resumption without duplicate effects.
- [ ] Add native/prepared/SQLx differential fixtures for DDL audit logs, blocking
  DROP TABLE, tag filtering, ordering, disabled hooks, cascading drops and
  rollback. Add generated DDL histories and hook-overhead benchmarks.

**DoD:** schema hooks behave coherently with existing transactions and routine
execution. Task 40 exposes event-trigger metadata. Follow the
[PostgreSQL event-trigger behavior](https://www.postgresql.org/docs/18/event-trigger-definition.html)
and [inspection-function contract](https://www.postgresql.org/docs/18/functions-event-triggers.html)
for the included surface.

### Task 36 — Transaction modes and exported snapshots [PENDING]

**Dependencies:** 28–35, including 34A and 35A.

- [ ] Implement READ ONLY/READ WRITE, DEFERRABLE/NOT DEFERRABLE, session defaults
  and correct restrictions on changing transaction characteristics.
- [ ] Implement COMMIT/ROLLBACK AND CHAIN and audit aliases/batch atomicity.
- [ ] Implement pg_export_snapshot and SET TRANSACTION SNAPSHOT within one Db,
  with lifetime, isolation and session validation distinct from db.snapshot().

**DoD:** safe read-only serializable snapshots, temporary-object exceptions,
waiting, timeout, cleanup and savepoint restrictions match PostgreSQL.

### Task 36A — Transaction control within procedures and DO [PENDING]

**Dependencies:** 34–36, including 34A and 35A. Execute directly after Task 36.

- [ ] Parse and bind COMMIT/ROLLBACK and AND CHAIN inside supported procedural
  bodies. Track PostgreSQL's permitted top-level/nested CALL/DO contexts;
  reproduce rejection inside explicit caller transactions, intervening function
  calls, active exception subtransactions and other prohibited contexts. Function,
  data/event-trigger and aggregate callbacks must not bypass these restrictions.
- [ ] Keep procedure variables, control-flow position and output state alive
  across transaction end. Replace transaction ID/snapshots, release transaction
  locks, check deferred constraints and run commit/abort cleanup through the
  existing transaction machinery; preserve session state where PostgreSQL does.
- [ ] Start the next transaction with defaults or chained characteristics as
  appropriate. Test timestamps, SET LOCAL, constraints, sequences, temporary
  ON COMMIT actions, session/transaction advisory locks and catalog visibility.
- [ ] Preserve earlier committed batches after a later exception, timeout or
  failed commit; roll back only the affected current transaction. Ensure outer
  statement error handling cannot restore state from before earlier commits.
  Verify resulting session state and subsequent commands against PostgreSQL.
- [ ] Model implicit read-only query-loop cursor materialization at the first
  transaction boundary, remaining iteration and lock release. Reject transaction
  commands in PostgreSQL-prohibited mutation-driven loops. Explicit procedural
  cursors and SQL cursor integration follow in Task 38.
- [ ] Add native and SQLx differential fixtures, including parameterized/prepared
  CALL where PostgreSQL permits it, nested calls, explicit transaction guards,
  exception recovery and two-session visibility between internal commits. Extend
  generated execution histories and measure representative batched calls.

**DoD:** one CALL can span multiple ordinary transactions without changing their
isolation/locking semantics or losing procedural execution state. A call that
commits batch 1 and fails in batch 2 leaves batch 1 visible and batch 2 rolled
back, matching PostgreSQL. Invalid transaction termination matches its SQLSTATE.
Use the [PostgreSQL procedural transaction rules](https://www.postgresql.org/docs/18/plpgsql-transactions.html)
as the acceptance reference, including restrictions on nested contexts.

### Task 37 — Locking and cancellation closure [PENDING]

**Dependencies:** 25–36, including 34A, 35A and 36A.

- [ ] Implement the full included relation-lock mode matrix and NOWAIT; audit
  row strengths, OF, SKIP LOCKED, DDL locks, lock upgrade and reentrancy.
- [ ] Audit existing transaction/session advisory locks, shared/exclusive keys,
  release/savepoint/drop behavior and deadlock detection with relation/row locks.
- [ ] Apply statement/lock deadlines and cancellation consistently inside new
  SRFs, routines, COPY and long expression execution as those paths are integrated.

**DoD:** controlled schedules cover deadlock, timeouts, abort cleanup, refreshed
predicates and no double execution of side effects across waits.

### Task 38 — Prepared SQL, cursors and session reset [PENDING]

**Dependencies:** 21, 28, 34, 36–37.

- [ ] Implement SQL PREPARE/EXECUTE/DEALLOCATE, parameter signatures and catalog
  invalidation using the existing native prepared machinery.
- [ ] Implement DECLARE/FETCH/MOVE/CLOSE, scroll/non-scroll and holdable cursors,
  cursor lifetime and WHERE CURRENT OF for legal update/delete targets.
- [ ] Implement PL/pgSQL refcursor variables and bound/parameterized cursor
  declarations; OPEN with positional/named arguments, OPEN FOR query and
  OPEN FOR EXECUTE ... USING. Preserve variable binding at open time, generated
  and explicit portal names, and SCROLL/NO SCROLL restrictions.
- [ ] Implement procedural FETCH INTO, MOVE, CLOSE, bound-cursor FOR loops,
  FOUND/NULL target behavior and WHERE CURRENT OF. Support passing and returning
  refcursor values so callers can fetch through SQL in the same transaction.
  Reuse session cursor state; do not build a separate procedural cursor engine.
  Integrate Task 34's changing RECORD targets when fetching from cursors with
  different result structures, including no-row and field-access behavior.
- [ ] Integrate SQL and procedural cursor lifetime, snapshots and held results
  with internal procedure/DO transaction boundaries from Task 36A; retain its
  implicit query-loop behavior without making explicit cursors holdable by
  default. Match savepoint/exception rollback, cursor position and resource
  cleanup, including failures and lock waits during fetch.
- [ ] Implement DISCARD ALL/TEMP/SEQUENCES and accepted plan reset behavior,
  including transaction restrictions and session resource cleanup.

**DoD:** cursor position, snapshots, invalid operations and pool reuse match
PostgreSQL; plan-dependent replan choices remain Tier C, result behavior Tier A.
Differential native/SQLx cases cover returned cursors, multiple simultaneous
cursors, dynamic/parameterized opens, scroll restrictions, end-of-results,
positioned mutations and transaction/subtransaction lifetime.

### Task 39 — In-memory COPY and notifications [PENDING]

**Dependencies:** 25, 30, 32, 35–38.

- [ ] Add explicit native and SQLx-compatible COPY input/output stream APIs;
  implement text/CSV/binary, table/query forms, columns, options, errors and
  PostgreSQL 18 error-handling options for included types, plus COPY WHERE.
- [ ] Implement LISTEN/UNLISTEN/NOTIFY and pg_notify with per-Db sessions,
  commit-time delivery, payload validation, duplicate folding and rollback.
- [ ] Expose an in-process notification receive API; define and test adapter
  limitations without claiming compatibility with socket-bound PgListener.

**DoD:** partial stream failures are atomic as PostgreSQL requires, generated
columns/defaults/triggers are respected, and notifications never escape rollback.

### Task 40 — Settings, catalog and adapter closure [PENDING]

**Dependencies:** 05–39, including 34A, 35A and 36A.

- [ ] Complete semantic GUCs for included behavior: DateStyle, IntervalStyle,
  lc_time, lc_numeric, lc_monetary for the selected profiles,
  bytea_output, extra_float_digits, standard_conforming_strings/escape handling,
  transaction access modes and zone behavior; audit all registered settings.
  Integrate Task 34's plpgsql.check_asserts with SET/SET LOCAL/RESET/SHOW,
  transaction/savepoint rollback and SQLx session reuse; never treat this
  semantic setting as an ignored planner option.
- [ ] Extend pg_catalog and information_schema for included schemas, relations,
  columns/types, constraints, indexes, sequences, views, routines, aggregates,
  data/event triggers,
  prepared statements and settings, with correct column metadata/visibility.
- [ ] Complete included format_type, reg*/to_reg* and pg_get_* introspection
  helpers, comments/object identity and logical tableoid; enumerate exact
  supported columns/signatures.
- [ ] Audit SQLx describe, argument/result codecs, errors, nested transactions,
  cancellation, pooling and migration behavior across every new type/statement.

**DoD:** user-type metadata and empty-result introspection work without casts;
semantic settings, session reuse and native/SQLx integration pass their tests.

## Milestone F — validate and publish compatibility

### Task 41 — Finish deferred fuzzing deliverables [PENDING]

**Dependencies:** 02–40; deterministic regressions accumulate throughout.

- [ ] Retain Squirrel + AFL++; do not restart the discarded SQLancer approach.
  Expand grammar-compatible seeds and use the existing generated-SQL
  differential target for additional syntax.
- [ ] Implement stable mismatch classification/deduplication, reproducible state
  capture, reduction and a checked-in deterministic regression corpus.
- [ ] Add bounded smoke and documented longer campaigns, with reports for total
  inputs, PG-accepted cases, feature replays, infrastructure errors
  and unique mismatches. Explicitly cover commits, sequences and session state
  that a rollback-only harness cannot reset completely.

**DoD:** no untriaged in-scope mismatch remains in the release campaign; each
fixed finding runs in ordinary tests without Squirrel. Do not claim coverage
from mutated statements that PostgreSQL or the Squirrel grammar never accepted.

### Task 42 — Final compatibility closure and documentation [PENDING]

**Dependencies:** 01–41.

- [ ] Recheck every planned feature against PostgreSQL 18 and the implementation
  manifest. Resolve remaining parser, fixture and execution gaps.
- [ ] Run Phase 2/3/4 conformance, application migration/workload gates, full
  workspace checks, extended properties, concurrent schedules and fuzz regressions.
  Exercise cross-feature combinations as well as individual SQL forms.
- [ ] Publish coverage counts with denominators and evidence for every feature.
- [ ] Publish `docs/compatibility.md` and a Phase 4 release audit; update README,
  spec, known bugs, source map and grammar reference. Keep historical Phase 3
  references pointing to plan_phase3_complete.md.
- [ ] Record representative benchmark comparisons, codec behavior and environment
  assumptions with reproducible examples.
- [ ] Present the outcome and obtain user approval before marking the phase
  complete or archiving this plan.

**DoD:** every implementation entry has passing conformance evidence, documented
behavior and validated native/SQLx integration.

## Planned acceptance examples

These are target examples, not claims that this planning change adds support.
Every task adds more exhaustive differential fixtures.

```sql
-- 04: omitted trailing columns receive defaults
CREATE TABLE example (id integer, value integer DEFAULT 7);
INSERT INTO example VALUES (1);
SELECT TIMESTAMPTZ '2024-07-01 12:00+00';

-- 13: non-default array bounds and slices
SELECT ('[0:2]={10,20,30}'::integer[])[0:1];

-- 18–19: tied limiting and grouping sets
SELECT value FROM example ORDER BY value FETCH FIRST 1 ROW WITH TIES;
SELECT id, sum(value), grouping(id)
FROM example GROUP BY GROUPING SETS ((id), ());

-- 30: PostgreSQL 18 virtual and stored generation
CREATE TABLE totals (
    quantity integer,
    doubled integer GENERATED ALWAYS AS (quantity * 2) VIRTUAL,
    tripled integer GENERATED ALWAYS AS (quantity * 3) STORED
);

-- 25: old/new row values from an UPDATE
UPDATE example SET value = value + 1 RETURNING old.value, new.value;
```

## Planning handoff

- [ ] User approves the Phase 4 implementation plan.

All implementation tasks remain pending; approval of this document does not
mark any implementation task complete.
