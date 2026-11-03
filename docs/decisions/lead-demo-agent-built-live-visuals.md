# Lead demo: agent-built live visuals

Rationale for the `[lead-demo]` entry in `docs/plan/ARCHITECTURE.md` §Product, decided
2026-11-03.

## Trigger

Read this before choosing what a release, a sample or a docs page leads with, and before
writing camera or microphone capture back in as the headline.

## Decision

The product leads with an agent building a live visual system while the person watches. The
first form is a stream overlay built from the browser source, Rive, the compositor and the MCP
tools; the second is data-driven motion graphics, scrubbable, once stream clocks land. Camera
and microphone stay modules, not the story.

## Why

- **It works on the tree as it is.** The browser source, Rive, the compositor and MCP exist;
  the three engine milestones make it faster and richer rather than gating it.
- **It is what the stack does that others do not.** Real programs in their own processes, an
  agent editing the graph live, output reaching any browser by URL.
- **It grows with the roadmap.** The strict families make it fast, subgraphs give agents
  reusable pieces, and stream clocks add scrubbing and export.

## Rejected alternatives

- **A Kerbal-style parts garage** — the better long game, but it needs all three milestones
  and its own app before anyone can touch it.
- **Motion graphics first** — waits on stream clocks.
- **"TouchDesigner, but headless"** — a category, not a demo.
