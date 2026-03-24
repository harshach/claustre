---
name: review_fix_loop
description: Triage PR review comments, implement accepted fixes with tests, verify, and prepare response.
---

## triage_review
- provider: claude

Read all PR review comments and triage each one:

For each comment:
1. **Understand the feedback** — what is the reviewer asking for?
2. **Assess validity** — is this a real issue or a style preference?
3. **Categorize**: ACCEPT (will fix), REJECT (with reason), or DISCUSS (needs clarification)
4. **Estimate effort** for accepted items

**Output a numbered list:**
```
1. [ACCEPT] file:line — "reviewer comment" → planned fix
2. [REJECT] file:line — "reviewer comment" → reason for rejection
3. [DISCUSS] file:line — "reviewer comment" → question for reviewer
```

## apply_fixes
- provider: claude
- runtime: default

Implement the accepted review feedback using TDD:

For each accepted comment:
1. Write a failing test if the fix changes behavior
2. Make the code change
3. Run tests — verify all pass
4. Commit with message referencing the review comment number

**Rules:**
- One commit per review comment for clear traceability
- Don't combine fixes — each should be independently reviewable
- For rejected comments, add a code comment explaining the decision only if it's non-obvious

## verify
- provider: codex
- runtime: default

Verify all fixes:

1. Run the full test suite — all tests must pass
2. Run linting/formatting — no new issues
3. Check that each accepted comment has a corresponding commit

**Output:**
```
REVIEW FIXES: [X/Y addressed]
Tests:        [all passing / X failures]
Lint:         [clean / X issues]
```

## prepare_response
- provider: claude

Write the PR review response:

For each comment:
- **Accepted**: "Fixed in [commit SHA]. [brief description of change]"
- **Rejected**: "Keeping as-is because [reason]. [optional: link to docs/convention]"
- **Discussed**: "Question: [your question]. Waiting for clarification."

Keep responses concise and professional. Link to commits where possible.
