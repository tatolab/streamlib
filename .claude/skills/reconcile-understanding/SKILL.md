---
name: reconcile-understanding
description: The correction loop — the owner fixes something Claude got wrong, and the wrong belief is hunted down in every place it is written. Use when the owner says "that's wrong", "you misunderstood", "let me correct you", or steers Claude's model of the system.
---

# Reconcile understanding

A correction that only fixes the conversation is gone when the session ends. Find every place the
wrong belief is written down and fix it there.

1. **Say the correction back in your own words** and check it matches the owner's intent before
   fixing anything; a slightly wrong replacement spreads just as far.
2. **Hunt the belief's homes**: CLAUDE.md, rules, skills, agents, docs, code comments, memory
   files, and Linear issues and project documents. Search for the idea, not only the wording. If
   it also lives in another repo, name those places.
3. **List the fixes** — place, current text, corrected text — so the owner can strike lines.
4. **Apply them.** Repo text goes in one PR. A Linear issue or document is edited in place. A
   place in another repo becomes an issue there through `file-issue`. Where the wrong text
   records an agreed decision, show the owner the new wording before changing it.

If the belief came from something a check could have caught, propose that check with
`propose-rule`.
