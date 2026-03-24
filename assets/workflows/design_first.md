---
name: design_first
description: For big features. Research → interactive brainstorming → design approval → detailed plan → TDD implementation → review → verify.
---

## research
- provider: claude

Deep-dive into the codebase and problem space before proposing anything:

1. **Read the task/issue** — understand what's being asked and why
2. **Explore the codebase** — find all related code, trace data flows, understand existing patterns
3. **Check history** — read recent commits and PRs in the relevant area
4. **Map dependencies** — what systems, services, or modules are involved?
5. **Identify constraints** — performance requirements, backwards compatibility, API contracts

**Output a structured research summary:**
- Current architecture (how it works today)
- Key files and entry points
- Constraints and non-negotiables
- Open questions that need user input

Do NOT propose solutions yet — only gather and present findings.

## brainstorm
- provider: claude

Interactive design session. Turn the idea into a fully formed design through dialogue.

**Rules for this stage:**
- Ask questions **ONE AT A TIME** — don't overwhelm
- Prefer **multiple choice** questions when possible
- After understanding the requirements, propose **2-3 approaches** with trade-offs
- Lead with your recommended option and explain why
- Present the design **in sections** — ask for approval after each section
- Scale each section to its complexity (a few sentences if simple, up to 300 words if nuanced)

**Cover these areas (as sections):**
1. Architecture — high-level structure, components, how they interact
2. Data model — schemas, state, storage
3. API/interfaces — how components communicate
4. Error handling — failure modes and recovery
5. Testing strategy — what to test, how, coverage goals

<HARD-GATE>
Do NOT write any code, scaffold any project, or take any implementation action.
Only produce the design. The user will review and approve it before you proceed.
Every feature goes through this — even ones that seem simple.
"Simple" features are where unexamined assumptions cause the most wasted work.
</HARD-GATE>

**Key principles:**
- YAGNI ruthlessly — remove unnecessary features from all designs
- Design for isolation — each unit should have one clear purpose
- Follow existing codebase patterns — don't introduce new paradigms without good reason
- If the feature is too large for one spec, decompose into sub-features first

## approve_design
- gate: manual_approval

## plan
- provider: claude

Based on the approved design, write a detailed implementation plan.

Assume the engineer implementing this has **zero context** for the codebase.
Document everything: which files to touch, exact changes, testing strategy.

**Structure as bite-sized tasks (2-5 minutes each):**

```
### Task N: [Component Name]
Files: create/modify/test paths
- [ ] Step 1: Write the failing test
- [ ] Step 2: Run test — verify it fails
- [ ] Step 3: Write minimal implementation
- [ ] Step 4: Run test — verify it passes
- [ ] Step 5: Commit
```

**Include for each task:**
- Exact file paths to create or modify
- Complete code specifications (not "add validation" — show what)
- Exact test commands with expected output
- Dependencies on other tasks

**IMPORTANT: Do NOT start implementing — only produce the plan.**

## approve_plan
- gate: manual_approval

## implement
- provider: claude
- runtime: default

Execute the approved plan task by task using strict TDD:

**The Iron Law: NO PRODUCTION CODE WITHOUT A FAILING TEST FIRST.**

For each task:
1. Write the failing test
2. Run it — verify it FAILS (not errors, fails for the right reason)
3. Write minimal code to make it pass
4. Run ALL tests — verify everything passes
5. Refactor while keeping tests green
6. Commit with a descriptive message

**If you wrote code before the test, delete it. Start over.**
- Don't keep it as "reference"
- Don't "adapt" it while writing tests
- Delete means delete

Stop and ask for help if:
- You hit a blocker or unclear instruction
- Verification fails repeatedly
- The plan needs adjustment based on what you've learned

## review
- provider: claude

Two-stage review:

**Stage 1 — Design Compliance:**
- Re-read the approved design and plan
- Verify every design decision is implemented correctly
- Flag any deviations — are they intentional improvements or drift?

**Stage 2 — Quality + Security:**
- Code quality: long functions, missing error handling, dead code
- Security: credentials, injection, missing auth checks, input validation
- Test quality: are tests testing behavior or implementation details?

Fix all CRITICAL and HIGH issues before proceeding.

## verify
- provider: codex
- runtime: default

Final verification — evidence before claims:

1. **Build** — run build, check exit code
2. **Types** — run type checker
3. **Lint** — run linter
4. **Tests** — run ALL tests, report pass/fail and coverage
5. **Secret scan** — check for hardcoded credentials

```
VERIFICATION: [PASS/FAIL]
Build:     [OK/FAIL]
Types:     [OK/X errors]
Lint:      [OK/X issues]
Tests:     [X/Y passed, Z% coverage]
Secrets:   [OK/X found]
Ready for PR: [YES/NO]
```

## prepare_pr
- provider: claude

Prepare for review:
1. Write PR description: summary, files changed, testing evidence, risks
2. Link back to the design doc and plan
3. Ensure all commits have descriptive messages
4. Verify branch is clean
