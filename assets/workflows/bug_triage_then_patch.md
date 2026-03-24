---
name: bug_triage_then_patch
description: Systematic debugging — analyze issue, identify the right test layer, write regression tests, minimal fix, red-green verify.
---

## triage
- provider: claude

Investigate the bug from the GitHub issue and codebase:

1. **Read the issue** — understand reproduction steps, expected vs actual behavior, affected module
2. **Identify the layer** — where does the bug live?
   - **Backend API** → Java code in `openmetadata-service/`
   - **Ingestion** → Python code in `ingestion/src/`
   - **UI** → TypeScript/React in `openmetadata-ui/src/main/resources/ui/`
   - **Cross-layer** → multiple layers involved
3. **Find the code** — trace from the issue's reproduction steps to the specific code path
4. **Identify root cause** — the specific file, function, and line responsible
5. **Explain WHY** — the mechanism causing the bug, not just symptoms

**Output your findings as:**
- Layer: [backend / ingestion / UI / cross-layer]
- Root cause: `[file:line]` — [mechanism]
- Reproduction path: [entry point → failure point]
- Test strategy: [which test types to write — see below]

**IMPORTANT: Do NOT fix the bug yet. Only diagnose and identify where tests should go.**

## write_regression_tests
- provider: claude
- runtime: default

Write regression tests that demonstrate the bug. Place tests at the RIGHT layer:

**Backend API bugs:**
- **Unit tests** in `openmetadata-service/src/test/` — test the specific method/class
- **Integration tests** in `openmetadata-integration-tests/` — test the full API endpoint
  - Use `OpenMetadataApplicationTest` as the base class for integration tests
  - Tests hit real database and search — no mocking internal classes

**Ingestion bugs:**
- **Unit tests** in `ingestion/tests/unit/` using pytest
  - `assert x == y` style, NOT `unittest.TestCase`
  - Use `unittest.mock` only for external boundaries
- **Integration tests** in `ingestion/tests/integration/` using pytest
  - These test against a running OpenMetadata server

**UI bugs:**
- **Unit tests** in `openmetadata-ui/.../ui/src/` as `*.test.tsx` files using Jest
  - Test component behavior, not implementation details
  - Use `@testing-library/react` for rendering
- **E2E tests** using Playwright for user-flow reproduction

**For each test:**
1. Write the test based on the issue's reproduction steps
2. The test MUST fail right now (proving it catches the bug)
3. Run it — verify it fails for the expected reason
4. Commit the failing test separately

If the bug can't be reproduced with an automated test (e.g., race condition, environment-specific), document why and write the closest approximation.

## plan_patch
- provider: claude

Based on the diagnosis and failing tests, produce a patch plan:

1. List the specific files and functions to change
2. Describe each change and why it fixes the root cause
3. Assess risk — what else could break?
4. Keep the fix surgical — minimum changes only

**IMPORTANT: Do NOT start implementing — only produce the plan.**

## approve_patch
- gate: manual_approval

## implement
- provider: claude
- runtime: default

Fix the bug with the minimum change needed:

1. The failing regression tests already exist from the previous stage
2. Write the minimal code change to make them pass
3. Run the regression tests — verify they pass
4. Run the FULL test suite for the affected layer:
   - Backend: `mvn test -pl openmetadata-service` + `mvn spotless:apply`
   - Ingestion: `source env/bin/activate && cd ingestion && python -m pytest tests/unit/ -v`
   - UI: `cd openmetadata-ui/src/main/resources/ui && yarn test`
5. Commit with a message referencing the issue number

Do NOT refactor surrounding code or add unrelated improvements.

## verify
- provider: codex
- runtime: default

**Red-green verification — the fix must be proven correct:**

1. Run the regression tests — confirm they PASS with the fix
2. **Revert your fix** temporarily (`git stash`)
3. Run the regression tests again — confirm they FAIL (proves the tests catch the bug)
4. **Restore the fix** (`git stash pop`)
5. Run the regression tests — confirm they pass again
6. Run the FULL test suite for the affected layer — no regressions
7. Run formatting: `mvn spotless:apply` (Java) or `make py_format` (Python) or `yarn lint` (UI)

**Output:**
```
VERIFICATION: [PASS/FAIL]
Layer:                          [backend/ingestion/UI]
Regression test (with fix):     [PASS/FAIL]
Regression test (fix reverted): [PASS/FAIL] (must FAIL to prove test works)
Full test suite:                [X/Y passed]
Formatting:                     [clean/X issues]
Ready for PR: [YES/NO]
```

Evidence before claims. Show the test output.
