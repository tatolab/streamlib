---
name: research
description: Answer a question from primary sources, in chat. Use when a decision needs facts the repo cannot provide — platform or driver behaviour, spec details, protocol semantics, a dependency's real behaviour, ecosystem comparisons.
---

# Research

1. **Go to primary sources**: official documentation, specifications, source code, first-party
   APIs. Follow each claim back to the source that owns it; a secondary write-up is a lead, not
   evidence.
2. **A claim about how a machine or tool behaves is settled by running the read-only query**, not
   by reading about it. When only a write would show it, use the primary source or a throwaway
   test against something disposable.
3. **Answer in chat**: the answer, the evidence with URLs, and what remains unknown. Write no file
   in the repo and no code.
4. **A finding that should outlive the session** goes where it is used: a comment on the Linear
   issue it informs, or a document on the Linear project when it informs several. A finding that
   calls for a decision is put to the owner as a stated question.
