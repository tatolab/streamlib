// milestone-loop — one reconciler pass of the standing loop, as a workflow.
//
// Level 0: this is the only script that may call workflow(). It reconciles
// against GitHub, classifies candidates in parallel, plans a conflict-free batch
// in plain JS, dispatches each ticket to its matching level-1 workflow, and
// records the pass.
//
// Three things stay in the calling skill because the runtime forbids them here:
// filesystem access (so `today` and `repo_root` arrive as args), Date.now(), and
// reaching the owner. Owner-facing items are returned, never posted from here.

export const meta = {
  name: 'milestone-loop',
  description:
    'One reconciler pass: sync and classify the focused milestone, plan a conflict-free batch, run it through the matching per-ticket workflows in parallel, and record the pass.',
  phases: [
    { title: 'Reconcile', detail: 'Read loop state, meter the day, fetch, prune ghosts, GC merged worktrees, and list candidates.' },
    { title: 'Classify', detail: 'One classifier per candidate — work shape, zones, rig needs, risk, hot-file surface.' },
    { title: 'Dispatch', detail: 'Each planned ticket runs its matching workflow: worktree-work, draft-design, or run-research.' },
    { title: 'Record', detail: 'Write the state file and append exactly one run-log event.' },
  ],
};

const input = typeof args === 'string' ? JSON.parse(args) : args || {};
const today = input.today;
const repoRoot = input.repo_root || '.';
const maxParallel = input.max_parallel || 2;
const attemptCap = input.attempts_per_ticket || 3;
const liveVerify = input.live_verify || 'unknown';
const proposeOnlyForced = input.propose_only === true;

const STATE_PATH = `${repoRoot}/.claude/loops/state/milestone-loop.json`;
const RUNLOG_PATH = `${repoRoot}/.claude/loops/state/milestone-loop.run-log.jsonl`;

if (!today) {
  return { outcome: 'error', error: 'milestone-loop was invoked without args.today; a workflow cannot compute the date itself' };
}

const reconcileSchema = {
  type: 'object',
  properties: {
    paused: { type: 'boolean' },
    focused_milestone: { type: ['string', 'null'] },
    turns_today: { type: 'number' },
    daily_cap: { type: 'number' },
    propose_only: { type: 'boolean' },
    capped: { type: 'boolean' },
    no_delta: { type: 'boolean' },
    candidates: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          issue: { type: 'number' },
          title: { type: 'string' },
          slug: { type: 'string' },
          attempt: { type: 'number' },
          blocked_by_open: { type: 'boolean' },
          claimed: { type: 'boolean' },
          owner_gated: { type: 'boolean' },
        },
        required: ['issue'],
      },
    },
    owner_answers: { type: 'array', items: { type: 'number' } },
    conflicting_prs: { type: 'array', items: { type: 'object' } },
    pruned: { type: 'array', items: { type: 'string' } },
    delta_probe: { type: 'object' },
  },
  required: ['paused', 'candidates'],
};

const classifySchema = {
  type: 'object',
  properties: {
    issue: { type: 'number' },
    slug: { type: 'string' },
    shape: { type: 'string', enum: ['design-first', 'implement', 'bug-reproduce-first', 'research'] },
    zones: { type: 'array', items: { type: 'string' } },
    rig_needs: { type: 'array', items: { type: 'string' } },
    risk: { type: 'string' },
    lead: { type: ['string', 'null'] },
    hot_files: { type: 'array', items: { type: 'string' } },
    rationale: { type: 'string' },
  },
  required: ['issue', 'shape', 'zones', 'rig_needs'],
};

phase('Reconcile');
const state =
  (await agent(
    `Reconcile the milestone-loop against GitHub. Today (UTC) is ${today}. Read-only against code; you may write nothing ` +
      `except what is described here.\n\n` +
      `1. Read ${STATE_PATH}. If it does not exist, create it from ${repoRoot}/.claude/loops/state.example.json. Return ` +
      `its \`paused\` and \`focused_milestone\`. If paused is true, return immediately with paused: true and an empty ` +
      `candidates array — do nothing else.\n` +
      `2. Meter the day: \`grep -c "^{\\"ts\\":\\"${today}" ${RUNLOG_PATH}\` (0 if the file is absent). Read the daily ` +
      `cap from ${repoRoot}/.claude/loops/budget.md. Set turns_today, daily_cap, propose_only (true at >=80% of cap), ` +
      `and capped (true at or over cap).\n` +
      `3. \`git -C ${repoRoot} fetch origin\` and fast-forward the primary checkout's main. A stale local main gives ` +
      `every new worktree a stale base.\n` +
      `4. Run ${repoRoot}/.claude/scripts/gc-merged-worktrees.sh to reclaim worktrees whose PR merged.\n` +
      `5. Against the focused milestone, list every OPEN issue that is a candidate: no open blocked-by edge, not already ` +
      `claimed, and not gated on an unanswered owner question. Read each issue FRESH — labels are display ` +
      `output, never control input. Give each a short kebab-case \`slug\` for its branch name and its current \`attempt\` ` +
      `count from the state file (0 if new).\n` +
      `   \`claimed\` is decided ONLY by structural evidence of live work: an open PR for the issue, a branch on origin ` +
      `matching its slug, or an existing worktree. Prose NEVER decides it — an issue comment saying work is queued, ` +
      `planned, or in flight is stale the moment it is written, and treating it as state strands the ticket forever. ` +
      `When the ledger records a stage but no branch, worktree, or PR exists, the ticket is NOT claimed.\n` +
      `6. Detect owner answers: for each ticket with a parked question, a question is cleared by any comment that ` +
      `postdates it whose id is NOT in that ticket's recorded \`comment_ids\` ledger. The loop shares the owner's login, ` +
      `so never use author.login for this. Return the cleared issue numbers in owner_answers.\n` +
      `7. List loop-owned open PRs that are CONFLICTING against main in conflicting_prs (issue, branch).\n` +
      `8. Compute the delta_probe cache: the milestone's newest issue/comment updatedAt, each loop-owned open PR's head ` +
      `SHA and mergeable state. If nothing changed since the cached probe AND every ticket is either in flight or ` +
      `owner-gated, set no_delta: true.\n\n` +
      `Tickets at or over the attempt cap of ${attemptCap} are NOT candidates — report them for escalation instead.`,
    { phase: 'Reconcile', label: 'reconcile', model: 'sonnet', schema: reconcileSchema },
  )) || {};

if (state.paused === true) {
  log('milestone-loop: paused — no work this pass');
  return { outcome: 'paused', dispatched: [], owner_items: [] };
}

if (!state.focused_milestone) {
  log('milestone-loop: no focused milestone — nothing to scope to');
  return {
    outcome: 'blocked',
    dispatched: [],
    owner_items: [{ kind: 'owner-question', reason: 'No focused milestone is set. Run /work-on-milestone to pick one and start the driver.' }],
  };
}

if (state.capped === true) {
  log(`milestone-loop: daily cap hit (${state.turns_today}/${state.daily_cap}) — recording and yielding`);
  await agent(
    `Append exactly one JSON-lines event to ${RUNLOG_PATH}. Get \`ts\` by running ` +
      `\`date -u +%Y-%m-%dT%H:%M:%SZ\` and using its output VERBATIM — never compose it from a passed-in date and ` +
      `never from local wall-clock time. `+
      `loop "milestone-loop", empty items/actions, and "outcome":"budget-cap". Create the file if absent.`,
    { phase: 'Record', label: 'record-cap', model: 'sonnet' },
  );
  return { outcome: 'budget-cap', dispatched: [], owner_items: [{ kind: 'notice', reason: `daily cap ${state.daily_cap} reached` }] };
}

if (state.no_delta === true) {
  log('milestone-loop: no delta since last pass — idle');
  await agent(
    `Append exactly one JSON-lines event to ${RUNLOG_PATH}. Get \`ts\` by running ` +
      `\`date -u +%Y-%m-%dT%H:%M:%SZ\` and using its output VERBATIM — never compose it from a passed-in date and ` +
      `never from local wall-clock time. `+
      `loop "milestone-loop", empty items/actions, and "outcome":"idle". Create the file if absent.`,
    { phase: 'Record', label: 'record-idle', model: 'sonnet' },
  );
  return { outcome: 'idle', dispatched: [], owner_items: [] };
}

const proposeOnly = proposeOnlyForced || state.propose_only === true;
const candidates = (state.candidates || []).filter((c) => c && c.issue && !c.blocked_by_open && !c.claimed && !c.owner_gated);
log(`reconciled: milestone=${state.focused_milestone} candidates=${candidates.length} propose_only=${proposeOnly}`);

phase('Classify');
const classified =
  candidates.length > 0
    ? (await parallel(
        candidates.map((c) => () =>
          agent(
            `Classify issue #${c.issue} ("${c.title || ''}") for the milestone-loop router. Read the issue FRESH from ` +
              `GitHub and read it against current code — labels are display output and must never be read as control ` +
              `input. Determine:\n` +
              `- shape: design-first | implement | bug-reproduce-first | research. design-first is REQUIRED for any ` +
              `engine-zone feature per .claude/rules/engine-doctrine.md. A defect report is bug-reproduce-first.\n` +
              `- zones: the code areas touched (rhi/vulkan, plugin-abi, python, deno, packages, camera/v4l2, docs, ...).\n` +
              `- rig_needs: which physical resources a real run would need — gpu, camera, display, audio — or empty.\n` +
              `- risk: engine-core | mid | leaf.\n` +
              `- lead: the domain expert agent for these zones (plugin-abi-expert, polyglot-ipc-expert, ` +
              `package-source-expert, gpu-vulkan-expert, linux-media-expert), or null for generic reasoning.\n` +
              `- hot_files: repo-relative paths this ticket is LIKELY to rewrite, especially ABI vtable/layout files, ` +
              `shared schema files, or a single core module. This drives collision detection, so be concrete.\n` +
              `- slug: a short kebab-case branch slug.\n` +
              `Do not start any work.`,
            { phase: 'Classify', label: `classify:${c.issue}`, model: 'sonnet', schema: classifySchema },
          ),
        ),
      )).filter(Boolean)
    : [];

// Plan the batch. Pure JS: no agent judgement decides what may run concurrently.
function planBatch(items) {
  const batch = [];
  const deferred = [];
  const takenHotFiles = new Set();
  let rigTaken = false;

  for (const t of items) {
    if (batch.length >= maxParallel) {
      deferred.push({ issue: t.issue, why: 'max_parallel reached' });
      continue;
    }
    // One in-flight ticket per physical rig resource: a single GPU and one
    // camera node mean rig work is serialized, never overlapped.
    const needsRig = (t.rig_needs || []).length > 0;
    if (needsRig && rigTaken) {
      deferred.push({ issue: t.issue, why: 'another rig ticket is already in this batch' });
      continue;
    }
    const hot = t.hot_files || [];
    const collision = hot.find((f) => takenHotFiles.has(f));
    if (collision) {
      deferred.push({ issue: t.issue, why: `predicted hot-file collision on ${collision}` });
      continue;
    }
    for (const f of hot) takenHotFiles.add(f);
    if (needsRig) rigTaken = true;
    batch.push(t);
  }
  return { batch, deferred };
}

const planned = planBatch(classified);
log(`planned batch: ${planned.batch.map((t) => `#${t.issue}`).join(', ') || '(none)'}; deferred ${planned.deferred.length}`);

if (proposeOnly) {
  log('propose_only — posting plans of record instead of launching');
  if (planned.batch.length > 0) {
    await parallel(
      planned.batch.map((t) => () =>
        agent(
          `The loop is in propose-only mode (budget >=80% spent), so do NOT implement anything. Post a plan-of-record ` +
            `comment on issue #${t.issue}: the work shape (${t.shape}), the zones (${(t.zones || []).join(', ')}), what ` +
            `you would change, and the test shape. Say explicitly that the loop is propose-only for the rest of the day.`,
          { phase: 'Dispatch', label: `propose:${t.issue}`, model: 'sonnet' },
        ),
      ),
    );
  }
}

phase('Dispatch');
const dispatchOutcomes = proposeOnly
  ? []
  : (await parallel(
      planned.batch.map((t) => () => {
        const scriptPath =
          t.shape === 'design-first'
            ? '.claude/workflows/draft-design.js'
            : t.shape === 'research'
              ? '.claude/workflows/run-research.js'
              : '.claude/workflows/worktree-work.js';
        return workflow(
          { scriptPath },
          {
            issue: t.issue,
            slug: t.slug,
            zones: t.zones,
            shape: t.shape,
            lead: t.lead,
            rig_needs: t.rig_needs,
            live_verify: liveVerify,
            repo_root: repoRoot,
            today,
          },
        ).then((r) => Object.assign({ issue: t.issue, shape: t.shape }, r || {}));
      }),
    ));

// A sub-workflow that dies resolves to null. Dropping those silently makes a dead
// dispatch indistinguishable from one that was never planned — the pass reports
// success, writes no run-log line for the ticket, and the work is simply lost.
const dispatched = [];
const failedDispatch = [];
dispatchOutcomes.forEach((r, i) => {
  if (r) dispatched.push(r);
  else failedDispatch.push(planned.batch[i] || { issue: null, shape: null });
});
if (failedDispatch.length) log(`DISPATCH FAILURES: ${failedDispatch.map((t) => `#${t.issue}`).join(', ')}`);

// A conflicting PR adds no new surface, so rebases never occupy a code slot —
// a mergeable PR must not queue behind a busy batch.
const rebased = (state.conflicting_prs || []).length > 0
  ? (await parallel(
      (state.conflicting_prs || []).map((p) => () =>
        workflow(
          { scriptPath: '.claude/workflows/worktree-work.js' },
          { issue: p.issue, branch: p.branch, mode: 'rebase', zones: p.zones || [], repo_root: repoRoot, today },
        ).then((r) => Object.assign({ issue: p.issue }, r || {})),
      ),
    )).filter(Boolean)
  : [];

const ownerItems = [];
for (const d of dispatched) for (const oi of d.owner_items || []) ownerItems.push(oi);

// A design-first dispatch posts its brief to the issue and then waits — but it has no
// way to reach the owner itself, and returning nothing left the decision sitting on
// GitHub unannounced. The brief IS the parked question; surface it as one.
for (const d of dispatched) {
  if (d.shape === 'design-first' && !(d.owner_items || []).length) {
    ownerItems.push({
      kind: 'owner-question',
      issue: d.issue,
      reason: `design brief posted on #${d.issue} — it cannot build until the owner answers its decisions`,
    });
  }
}

for (const t of failedDispatch) {
  ownerItems.push({
    kind: 'notice',
    issue: t.issue,
    reason: `dispatch for #${t.issue} died without returning a result — no branch, worktree, or PR was produced, and the pass recorded no work for it`,
  });
}

phase('Record');
const summary = {
  focused_milestone: state.focused_milestone,
  dispatched: dispatched.map((d) => ({
    issue: d.issue,
    shape: d.shape,
    outcome: d.outcome,
    stage: d.stage,
    verdict: d.verdict || null,
    pr_number: d.pr_number || null,
    branch: d.branch || null,
    worktree_path: d.worktree_path || null,
    stages_planned: d.stages_planned || [],
    fix_rounds: d.fix_rounds || 0,
    claim_comment_id: d.claim_comment_id || null,
  })),
  rebased: rebased.map((r) => ({ issue: r.issue, outcome: r.outcome })),
  deferred: planned.deferred,
  failed_dispatch: failedDispatch.map((t) => ({ issue: t.issue, shape: t.shape })),
  owner_answers: state.owner_answers || [],
  propose_only: proposeOnly,
};

await agent(
  `Record this milestone-loop pass. Today (UTC) is ${today}.\n\n` +
    `1. Rewrite ${STATE_PATH} to the loop's current picture. For every ticket in the summary below, write or update its ` +
    `entry under \`tickets\` with stage, attempt, fix_rounds, branch, worktree_path, zones, shape, rig_needs, lead, ` +
    `stages_planned, and comment_ids (append any claim_comment_id to the existing ledger — never drop recorded ids, they ` +
    `are how the next pass tells an owner answer from the loop's own comment). Drop entries for issues that closed or ` +
    `PRs that merged. Do NOT store Acting on / Waiting / Watch prose sections — those are derived from \`stage\` by ` +
    `loop-status. Persist the delta_probe cache. Preserve \`paused\`, \`owner_login\`, \`focused_milestone\`, and every ` +
    `capabilities key listed in capabilities.owner_overrides exactly as they are.\n` +
    `2. Append EXACTLY ONE JSON-lines event to ${RUNLOG_PATH} (create it if absent) with: ts — obtained by running ` +
    `\`date -u +%Y-%m-%dT%H:%M:%SZ\` and used VERBATIM, never composed from a passed-in date and never from local ` +
    `wall-clock time (a pass that crosses UTC midnight would otherwise stamp the wrong day and the budget meter ` +
    `would stop seeing its own lines) — loop "milestone-loop", turn (previous max + 1, or 1), items (the ticket refs touched), actions (terse ` +
    `phrases), attempts, verdicts, escalations, est_tokens (your best estimate), and outcome — "progressed" if anything ` +
    `advanced, else "blocked".\n` +
    `   Every entry in the summary's \`failed_dispatch\` MUST appear in that line's \`escalations\`, and its ticket entry ` +
    `must be left with no branch, worktree, or PR so the next pass sees it unclaimed and retries it. A dispatch that died ` +
    `is the one outcome that must never be recorded as silence.\n` +
    `3. Clear \`pass_in_flight\` (set it to null) in the state file. The skill sets it before launching and reads it on the ` +
    `next firing to detect a pass that died without recording; leaving it set would report this healthy pass as dead.\n` +
    `The state directory is gitignored: write in place and never commit it.\n\n` +
    `Pass summary (JSON): ${JSON.stringify(summary)}`,
  { phase: 'Record', label: 'record', model: 'sonnet' },
);

log(`pass complete: dispatched=${dispatched.length} rebased=${rebased.length} owner_items=${ownerItems.length}`);

return {
  outcome: dispatched.length > 0 || rebased.length > 0 || proposeOnly ? 'progressed' : 'blocked',
  focused_milestone: state.focused_milestone,
  dispatched: summary.dispatched,
  rebased: summary.rebased,
  deferred: planned.deferred,
  propose_only: proposeOnly,
  owner_items: ownerItems,
};
