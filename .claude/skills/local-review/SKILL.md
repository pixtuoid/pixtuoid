---
name: local-review
version: 4.0.0
description: "Run pixtuoid's review locally at either scope — a DIFF review (REVIEW.md with the CI review's own reviewers, plus the must-run items the read-only CI review can't run) or a whole-codebase AUDIT (subsystem × factor fan-out). Use when CONTRIBUTING's arc step 7 calls for a local review, when the owner asks, for a must-run item, on 'is this ready to merge', or on 'whole-codebase review' / pre-release / periodic audit."
metadata:
  scope: "pixtuoid repo only"
---

# local-review — local orchestration of REVIEW.md

Every rule is [`REVIEW.md`](../../../REVIEW.md)'s or the shared review's
contract; fill briefs from them, never a paraphrase here.

## When to run

- **Diff scope**: when [arc step 7](../../../docs/CONTRIBUTING.md#the-arc-loop)
  calls for it, and for each [must-run](../../../REVIEW.md#must-run) item a
  change touches.
- **Whole-codebase scope**: "audit the repo", a pre-release or periodic sweep.

## Diff scope

1. **Isolate** the head in a detached worktree at its sha (two sessions on
   one tree race on HEAD). Note `path` and the `head` sha; `base` is the
   branch the PR targets, `origin/main` unless stacked.
2. **Run the CI review's orchestration here**, so both runners judge alike.
   Take the review contract and the `correctness-reviewer`, `design-reviewer`
   and `verifier` prompts and models verbatim from the shared workflow
   (`gh api repos/pixtuoid/.github/contents/.github/workflows/claude-review.yml --jq .content | base64 -d`),
   with three overrides: their GitHub tools become local read-only ones
   (`git diff <base>...<head>`, the worktree's files), since no PR may exist
   yet; the scope is `<base>...<head>`; and a reviewer may fetch the docs a
   best-practice question needs, which CI can't. Spawn the two reviewers in
   parallel, then one verifier per candidate. Run each must-run item the
   change touches as its own lens, with Bash,
   `model: sonnet` when it is mechanical, reporting each exit code as
   observed, never through a pipe.
   Give every brief the change's claims, from the PR body or, before it
   exists, the spec: a lazily filled brief turns every lens generic.
3. **Collect and verify.** A one-word or placeholder return is a STUB, not a
   review: re-run that lens alone. Verify every kept finding's premise
   yourself before coding a fix — read the comments on the item it names
   first — and REFUTE one that REVIEW.md's Do not flag covers, or deliberate
   design, with its mechanism.
4. **Fold** accepted findings into ONE commit.
5. **Disposition** each finding in a review thread once the PR is open. The
   bots open theirs; open each local finding the same way, then reply and
   resolve:

   ```sh
   pr=<N>; head=$(gh pr view $pr --json headRefOid -q .headRefOid)
   # Off the diff's lines: `-f subject_type=file` instead of line + side; a path
   # outside the diff (or removed by it) goes on the first surviving changed file,
   # its location in the body.
   gh api repos/{owner}/{repo}/pulls/$pr/comments -f commit_id=$head \
     -f path=<path> -F line=<n> -f side=RIGHT -F body=@<finding file>
   threads='query($owner:String!,$name:String!,$n:Int!){repository(owner:$owner,name:$name){pullRequest(number:$n){reviewThreads(first:100){totalCount nodes{id isResolved isOutdated path line comments(first:50){nodes{body}}}}}}}'
   gh api graphql -F owner='{owner}' -F name='{repo}' -F n=$pr -f query="$threads"
   gh api graphql -f t=<thread id> -F b=@<disposition file> \
     -f query='mutation($t:ID!,$b:String!){addPullRequestReviewThreadReply(input:{pullRequestReviewThreadId:$t,body:$b}){comment{id}}}'
   gh api graphql -f t=<thread id> \
     -f query='mutation($t:ID!){resolveReviewThread(input:{threadId:$t}){thread{isResolved}}}'
   ```

6. **Before merge**, judge against
   [the gate](../../../docs/CONTRIBUTING.md#the-merge-gate).

## Whole-codebase scope

Population = the tree, where the aggregate-only classes live (cross-PR
interaction, drift, debt accretion, coverage-topology gaps, invariant erosion,
orphaned surface). Prefer a `Workflow`, else parallel `Agent`s; scale to the
ask. Scout the work-list → subsystem finders (site and Raycast RENDERED) plus
per-factor finders (arch invariants · concurrency/liveness · security threat
model · performance · mutation depth on hot logic · silent failure · drift ·
deep modules) plus a SYSTEM lens (decomposition, dependency directions) and a
DRY census → adversarial verify, default REFUTE, one skeptic per finding and
2–3 differentiated ones for security/concurrency, majority confirms →
loop until two consecutive rounds come back empty → a completeness critic
("what modality did we NOT run?" seeds the next round) → dedup and rank,
keeping the refuted-as-deliberate list → dispositions plus a stale-phrase
sweep (`rg --hidden`) at 0. Involved refactors land in-arc; each bigger item
the owner keeps becomes a FOLLOW-UP PR.

## Red flags

| Thought | Reality |
|---------|---------|
| "CI is green, that's enough" | CI can't see design, blast radius, drift, or a deliberate-looking real bug. |
| "The bots will render it" | A bot reads the diff and the base tree; a must-run item is yours. |
| "I'll note the finding and move on" | Every finding needs a terminal state. |
| "The diff looks clean, we're done" (audit) | Drift, debt accretion and erosion accumulate between PRs; only the audit sees them. |
| "The finder found it, report it" (audit) | A separate skeptic tries to REFUTE each survivor first. |
| "Just unify the duplication" | Some duplication is documented deliberate separation; read the item's comments first. |
