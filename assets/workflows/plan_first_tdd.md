---
name: plan_first_tdd
description: The default workflow. Brainstorm → plan → approve → TDD → implement → verify → security review → PR prep.
---

## plan
- provider: claude

Create a comprehensive implementation plan before writing any code.

**Phase 1 — Understand:**
1. Read the task/issue carefully. Restate requirements in your own words.
2. Explore the codebase — read relevant files, understand architecture and conventions.
3. Ask clarifying questions if requirements are ambiguous (one at a time).

**Phase 2 — Design:**
4. Propose 2-3 approaches with trade-offs and your recommendation.
5. Break down into phases with specific, actionable steps.
6. Map files — list every file that will be created or modified with exact paths.
7. Identify risks, dependencies, and potential blockers.
8. Define testing strategy — what tests to write, what to cover.

**Phase 3 — Present:**
9. Output the plan as a numbered list of concrete steps.
10. Include estimated complexity (High/Medium/Low) for each phase.

<HARD-GATE>
Do NOT write any code, scaffold any project, or take any implementation action.
Only produce the plan. The user will review and approve it before you proceed.
This applies to EVERY task regardless of perceived simplicity.
</HARD-GATE>

## approve_plan
- gate: manual_approval

## write_failing_tests
- provider: claude
- runtime: default

Write failing tests that cover the approved plan. Follow strict TDD:

1. **Define interfaces** for inputs/outputs before any implementation.
2. **Write one failing test** at a time — clear name, tests real behavior, one thing per test.
3. **Run the test** — verify it FAILS for the expected reason (missing function, not typo).
4. **Commit tests** separately before implementation.

**Coverage targets:**
- 80% minimum for all new code
- 100% for critical business logic, auth, and financial calculations

**Test quality rules:**
- Test behavior, not implementation details
- Use real code, not mocks (mock only at system boundaries)
- Include: happy path, edge cases, error conditions, boundary values
- Clear test names that describe the expected behavior

Do NOT implement any production code in this stage — only tests.

## implement
- provider: claude
- runtime: default

Execute the approved plan using strict RED-GREEN-REFACTOR:

For each task:
1. **GREEN** — write minimal code to make the failing test pass
2. **Run ALL tests** — verify everything passes (not just new tests)
3. **REFACTOR** — improve code while keeping tests green
4. **Commit** with a descriptive message

**The Iron Law: NO PRODUCTION CODE WITHOUT A FAILING TEST FIRST.**
If you wrote code before the test, delete it and start over. No exceptions.

**Rules:**
- Minimal changes — don't add features beyond what tests require
- Don't refactor unrelated code — stay focused
- Stop and ask if you hit a blocker — don't guess
- Follow existing project patterns and conventions

## review
- provider: claude

Two-stage review of all changes:

**Stage 1 — Spec Compliance:**
- Re-read the original plan and requirements
- Create a checklist of ALL requirements
- Verify each requirement is met with evidence (test name, file path)
- Flag any deviations from the plan

**Stage 2 — Code Quality + Security:**
- Functions > 50 lines → suggest splitting
- Missing error handling → flag
- Console.log / debug statements → remove
- Hardcoded credentials, API keys → CRITICAL
- SQL/XSS injection risks → CRITICAL
- Missing input validation → HIGH
- Type safety issues (any types, missing null checks) → flag

Fix all CRITICAL and HIGH issues before proceeding.

## verify
- provider: codex
- runtime: default

**Verification before completion — evidence before claims, always.**

Run verification in this exact order:

1. **Build** — run the build command, check exit code
2. **Types** — run type checker, report errors with file:line
3. **Lint** — run linter, report all issues
4. **Tests** — run ALL tests, report pass/fail count and coverage
5. **Secret scan** — grep for potential secrets in changed files
6. **Git status** — show all changes since branch start

**Output structured report:**
```
VERIFICATION: [PASS/FAIL]
Build:     [OK/FAIL]
Types:     [OK/X errors]
Lint:      [OK/X issues]
Tests:     [X/Y passed, Z% coverage]
Secrets:   [OK/X found]
Ready for PR: [YES/NO]
```

Do NOT claim completion without fresh verification evidence.
"Should work" is not evidence. Run the command, read the output, THEN claim.

## prepare_pr
- provider: claude

Prepare the work for review:

1. Ensure all commits have descriptive messages
2. Write a PR description with:
   - Summary of changes (what and why)
   - List of files changed
   - Testing evidence (test names, coverage)
   - Risks or areas needing careful review
3. Verify branch is clean and ready
