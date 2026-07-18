- prefer enums over trait
- prefer inlining functions over make small function helper. Make separate function if it really used in several places
- prefer asserts over defensive programming (returning errors). Return error only if it make sence (e.g. we return error to user)
  if some code _should_ work, add assert for that
- prefer good naming against comments. Add comments only then we should document some non trivial postgres internals
- do not afraid breaking change. Prefer to change current function over making wrappers or new function that slightly
  different from existing
- prefer property tests against small local unit tests
- do not make links from code to specification
