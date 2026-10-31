---
name: reconcile-understanding
description: The correction loop — the owner fixes something Claude got wrong, and the wrong belief is hunted down in every place it lives (memories, docs, plan, glossary, skills, the snapshot). Use when the owner says "that's wrong", "you misunderstood", "let me correct you", or steers Claude's model of the system after a snapshot or an answer.
---

# Reconcile understanding

A correction that only fixes the conversation evaporates at session end. This skill makes
it stick by finding every place the wrong belief is written down.

1. **Say it back.** Restate the correction in your own words — not the owner's — and
   check it matches their intent. Fixing the wrong belief everywhere is wasted if the
   new one is also slightly off, so this step is worth the round trip.
2. **Hunt the belief's homes.** Search everywhere the wrong version may live: the memory
   directory (`MEMORY.md` + memory files), `docs/` (learnings, decisions, architecture),
   `docs/plan/` (plan, glossary), skill and agent texts, the architecture snapshot
   artifact, and open tickets. List every occurrence found.
3. **List the fixes**: place / current wrong text / corrected text, one table, so the
   owner can strike lines.
4. **Apply them**: memories directly; docs through the normal PR path; plan and glossary
   edits in a PR the owner reviews (they are where agreed intent lives, so the change
   should be visible, not folded into something unrelated); the snapshot artifact
   republished via `/snapshot-architecture` when it carried the error; skill/agent text
   in its own operating-model PR (`flow.md`).
5. **Write the correction as a memory** (type: feedback — the fact, why the old belief
   was wrong, how to apply the right one) so it survives into every future session.
