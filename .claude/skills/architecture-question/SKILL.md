---
name: architecture-question
description: Ask how something in the system works and get Claude's current understanding back, with evidence, labeled by source. Use when the owner asks "how does X work", "why is Y shaped this way", "what happens when Z" — any question about the system's behavior or structure rather than a request to change it.
disable-model-invocation: true
---

# Architecture question

The deliverable is shared understanding, not just an answer. No edits from this skill.

1. **Verify before asserting.** Read the actual code paths before answering — an
   architecture answer from memory alone is how wrong beliefs calcify. Route domain
   depth through the matching expert (`.claude/agent-knowledge/`).
2. **Answer in labeled layers**, so the owner can see where each piece of the
   understanding comes from:
   - **[CODE]** — what the tree actually does today, with `file:line`. Code is the
     authority on current behavior.
   - **[PLAN]** — what `docs/plan/` says is agreed or targeted, when it bears on the
     question.
   - **[INFERRED]** — Claude's judgment or reconstruction, with stated confidence.
3. **Surface disagreements found on the way.** If the code contradicts a plan entry or a
   doc while answering, say so in the answer — that drift is often worth more than the
   answer itself.
4. **Close the say-back loop.** End with one paragraph: "So my understanding is … —
   does that match yours?" A correction goes through `/reconcile-understanding`; a
   confirmed answer worth keeping may become a memory or a glossary term.
