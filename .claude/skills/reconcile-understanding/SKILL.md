---
name: reconcile-understanding
description: The correction loop — the owner fixes something Claude got wrong, and the wrong belief is hunted down in every place it lives (memories, docs, plan, glossary, skills, the snapshot). Use when the owner says "that's wrong", "you misunderstood", "let me correct you", or steers Claude's model of the system after a snapshot or an answer.
disable-model-invocation: true
---

# Reconcile understanding

A correction that only fixes the conversation evaporates at session end. This skill makes
it stick by finding every place the wrong belief is written down.

1. **Say-back gate.** Restate the correction in your own words — not the owner's — until
   they confirm it matches their intent. Unconfirmed means not yet understood; do not
   proceed past this step without the yes.
2. **Hunt the belief's homes.** Search everywhere the wrong version may live: the memory
   directory (`MEMORY.md` + memory files), `docs/` (learnings, decisions, architecture),
   `docs/plan/` (plan, glossary), skill and agent texts, the architecture snapshot
   artifact, and open tickets. List every occurrence found.
3. **Propose the fix batch**: place / current wrong text / corrected text, one table.
   The owner approves as a list and may strike lines.
4. **Apply the approved batch**: memories directly; docs through the normal PR path;
   plan and glossary edits only via a plan-editing skill's session (never sneak them);
   the snapshot artifact republished via `/snapshot-architecture` when it carried the
   error; skill/agent text via a dedicated operating-model PR (`flow.md`).
5. **Write the correction as a memory** (type: feedback — the fact, why the old belief
   was wrong, how to apply the right one) so it survives into every future session.
