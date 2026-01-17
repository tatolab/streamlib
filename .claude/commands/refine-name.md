# Naming Refinement Skill

You are helping refine a name to be MORE EXPLICIT, not shorter.

## Context

This codebase uses intentionally verbose, explicit names because:
1. AI agents lose context constantly - every session starts fresh
2. Short "idiomatic" names cause AI coding errors (empirically validated)
3. The name IS the documentation - no need to read code to understand purpose
4. Names should encode INTENT and RELATIONSHIPS, not implementation

## The Naming Philosophy

**WRONG (implementation-focused):**
- `Writer`, `Reader`, `Buffer`, `Channel`, `Manager`, `Handler`, `Context`
- `Sender`, `Receiver`, `Producer`, `Consumer`
- Abbreviations: `ctx`, `mgr`, `conn`, `buf`, `cfg`

**RIGHT (intent-focused):**
- `LinkOutputDataWriter` - writes data from a link output (not just "Writer")
- `LinkInputFromUpstreamProcessor` - connection FROM upstream TO this input
- `LinkOutputToDownstreamProcessor` - connection FROM this output TO downstream
- `LinkOutputToProcessorMessage` - message sent from link output to processor

## Rules

1. **Encode the relationship**: Where does it come from? Where does it go?
2. **Encode the role**: What does it DO in the system?
3. **Never use generic words alone**: `Inner`, `State`, `Manager`, `Handler`, `Context`
4. **Qualify everything**: `LinkState` → what state? → `LinkLifecycleState` or `LinkWiringState`
5. **Direction is explicit**: Use `From`/`To`, `Upstream`/`Downstream`, `Input`/`Output`

## Your Task

Given: $ARGUMENTS

Suggest 2-3 MORE EXPLICIT names. Do NOT suggest shorter names.

For each suggestion:
1. State the name
2. Explain what each part communicates
3. Rate how unambiguous it is (can someone with ZERO context understand it?)

Remember: Verbosity is a FEATURE. A 40-character method name that's crystal clear beats a 10-character name that requires reading code.
