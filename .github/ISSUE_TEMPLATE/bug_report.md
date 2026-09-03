---
name: Bug report
about: Report a compatibility gap or an incorrect result
title: ''
labels: bug
assignees: ''
---

**Describe the bug**
A clear and concise description of what the bug is.

**Expression JSON**
```json
["..."]
```

**Expected behavior (per maplibre-gl-js)**
What `maplibre-gl-js` returns / how it errors for the same input.
If possible, include a link to a JSFiddle / snippet demonstrating the reference behavior.

**Actual behavior (maplibre-expr)**
What this crate returns / how it errors.
Include the full error message and location `key` if it is a parse/type/eval error.

**Environment**
- crate version:
- rustc version:
- target (native / wasm32-unknown-unknown / other):

**Additional context**
Anything else that might help — feature type in an EvaluationContext, zoom, etc.
