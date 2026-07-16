# Loop budget

Daily caps per loop. When a cap trips, the loop stops landing new work and records why.

| Loop | Max main-turns / day | Max subagent spawns / turn | Token guidance |
|---|---|---|---|
| milestone-loop | 40 | 12 | Keep a single turn's spawns lean — one research/impl agent per open ticket in the pass, not one per file. Budget roughly one deep reasoning agent (opus) per acting-on ticket; prefer reusing an agent's context via follow-up over re-spawning fresh. |

<!-- OWNER: cap value / exemption breadth / extension protocol -->

## Metering the day
The **day** is the **UTC date of a run-log line's `ts`** — never the cumulative turn number and
never a wall-clock local date. Today's spend is measured by one mechanical meter, not by reading the
run-log:

```
grep -c "^{\"ts\":\"$(date -u +%F)" loops/run-log.md
```

That counts the run-log lines stamped with today's UTC date. It cannot be conflated with the
cumulative turn counter, and it rolls over correctly at UTC midnight. Never eyeball a long run-log to
estimate the day's count — that is what produced the miscount this meter replaces.

## Exemptions
- **Owner-directed corrections** (work the owner explicitly asked for in a comment) and **red-CI
  fixes** (restoring a broken baseline) are **exempt** from propose-only and from the daily cap —
  they are not "new work," they unblock. The exempt turns are still logged; they just do not throttle.

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
