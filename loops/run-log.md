# Loop run-log

Append-only. One JSON object per line (JSON-lines), one per turn. Newest entries go at the bottom.
This is the loop's durable memory of what it did — every turn appends exactly one event before it
yields.

## Event format

Each line is a single JSON object with these keys:

```json
{"ts":"<ISO-8601 UTC>","loop":"<loop name>","turn":<int>,"items":["<ticket refs the pass touched>"],"actions":["<what the turn did>"],"attempts":{"<ticket>":<count>},"verdicts":{"<ticket>":"<pass|fail|parked|escalated>"},"escalations":["<ticket refs escalated to the owner>"],"est_tokens":<int>,"outcome":"<progressed|blocked|budget-cap|idle>"}
```

- `ts` — turn end time, ISO-8601 UTC.
- `items` — ticket references the reconciler pass looked at this turn.
- `actions` — terse phrases for what happened (`"opened PR"`, `"posted question"`, `"swept worktree"`).
- `attempts` — running attempt count per ticket (cap 3, then escalate).
- `verdicts` — per-ticket result this turn.
- `escalations` — tickets handed to the owner (parked question / attempt cap).
- `est_tokens` — rough tokens spent this turn, for the daily budget.
- `outcome` — the turn's net result.

## Rotation
The daily meter (`grep -c "^{\"ts\":\"$(date -u +%F)" loops/run-log.md`) never needs the whole
file, but this log is append-only and grows forever. On the first turn of a **new UTC month**, or
once the body exceeds **200 lines**, rotate: move the accumulated event lines (everything below the
append marker) into `loops/run-log-YYYY-MM.md` (the month they cover) and start fresh with an empty
body under this header. The header and the append marker stay; only the events move.

<!-- loop appends below this line -->
