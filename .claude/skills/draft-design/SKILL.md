---
name: draft-design
description: Architecture-first entry point — recon the current code with the relevant domain experts, then produce a design brief FOR JONATHAN (mermaid diagram, trade-offs, decisions taken, and an explicit decisions-for-owner list) posted to the issue. Use before building anything engine-zone, when a feature needs a shape decided, or when he asks to "design X first" or "draft the approach for this issue". Nothing builds until he approves in a comment.
---

# draft-design — shape it before you build it

Engine-zone features (RHI, IPC wire, processor model, public ABI, escalate ops, registry/schema) get their shape decided and approved **before** any code lands (`.claude/rules/engine-doctrine.md`, `.claude/rules/flow.md`). This skill produces the brief that gets that approval.

## Procedure

### 1. Scope the design question
Read the issue (or the intent, if there's no issue yet — see step 4). State in one line what shape is being decided and which zones it touches. That zone read picks the experts.

### 2. Recon current code — in parallel, read-only
Spawn the domain experts whose zones the design touches (gpu-vulkan-expert, linux-media-expert, plugin-abi-expert, polyglot-ipc-expert, package-registry-expert) **in parallel**, read-only, each on a specific recon question: what core system already covers this concern, what would have to change, what the contract invariants and known failure modes are. The experts re-derive from the tree and cite `file:line` — the brief stands on evidence, not memory. This is the `draft-design.js` Recon phase when run under the loop; invoked directly, spawn the experts yourself.

The engine-model rule is the spine of the recon: **prove no existing core system already covers the concern before proposing a new abstraction.** A brief that adds a parallel shape where the RHI / GpuContext / pubsub / processor model already solves it is the default-wrong answer.

### 3. Draft the brief FOR JONATHAN
Merge the recon into a design brief written for the owner to decide from. It contains:
- **What & why** — the goal in plain language.
- **Mermaid diagram** — the proposed shape (data flow, ownership, or the state machine — whichever the design turns on).
- **Alternatives considered** — each option with its one-line why-or-why-not, including "extend the existing system" vs "new abstraction."
- **Decisions taken** — the calls the recon settled, with the evidence.
- **Risk class** — how far the change reaches and what it commits us to.
- **DECISIONS FOR OWNER** — an explicit, numbered list of the calls only Jonathan can make (a trade-off with no clear winner, a scope boundary, a milestone question). This is the point of the brief.

Keep implementation mechanics OUT — no file-by-file plan, no test function names. The brief decides the shape; the picker re-derives the mechanics at build time.

### 4. Post it
Post the brief to the issue as a comment. If there's no issue yet, create one first via the `file-issue` shape (feature form, `Design` section carries the brief), then post.

### 5. Hand the decision back
Nothing builds until Jonathan approves in a comment. Under the loop, this is a park (the ticket waits on his answer). Interactively, surface the DECISIONS-FOR-OWNER list to him directly. Either way, the brief is the deliverable — do not start implementing off an unapproved design.
