- prefer enums over trait
- prefer inlining functions over make small function helper. Make separate function if it really used in several places
- prefer asserts over defensive programming (returning errors). Return error only if it make sence (e.g. we return error to user)
  if some code _should_ work, add assert for that. But don't make assert with side-effects
- Do not add comments or doc comments by default, including module-level and public-API documentation. Prefer clear names and straightforward code.
  Add a comment only when documenting non-obvious PostgreSQL behavior or an invariant that cannot be made clear through code structure and naming.
  Keep it short and explain why, not how.
- do not afraid breaking change. Prefer to change current function over making wrappers or new function that slightly
  different from existing
- prefer property tests against small local unit tests
- do not make links from code to specification
