# Loop budget

Daily caps per loop. When a cap trips, the loop stops landing new work and records why.

| Loop | Max main-turns / day | Max subagent spawns / turn | Token guidance |
|---|---|---|---|
| milestone-loop | 40 | 12 | Keep a single turn's spawns lean — one research/impl agent per open ticket in the pass, not one per file. Budget roughly one deep reasoning agent (opus) per acting-on ticket; prefer reusing an agent's context via follow-up over re-spawning fresh. |

## On exceed
When a loop reaches a daily cap (or the 80% propose-only threshold in `loops/constraints.md`):

1. **Pause** — stop landing new code for the rest of the day (propose-only: planning, drafting,
   and posting questions are still allowed).
2. **Append a run-log event** to `loops/run-log.md` recording the cap hit (`"outcome":"budget-cap"`).
3. **Post an owner comment** on the active milestone (or the ticket in flight) so the owner sees the
   loop capped and can raise the cap or let it resume tomorrow.

## Model policy
All reasoning and implementation spawns are `opus`; mechanical spawns are `sonnet`. **No `fable`.**
Token guidance above is about spawn discipline, not a downgrade — never trade the model tier for
budget.

## Kill switch
Stop the `/loop` and clear the `/goal`, set `paused: true` in the loop's state file, or let the
budget cap here halt new work. Any one of the three stops the loop.
