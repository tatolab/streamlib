// implement-ticket workflow — take one ticket from a re-derived plan to a
// self-reviewed branch. The build lead is chosen by the issue's zones; it works
// in its own worktree with contractual checkpoint commits.

export const meta = {
  name: 'implement-ticket',
  description:
    'Rederive the issue against current code and post a plan-of-record, implement in the build lead’s own worktree, run the local gate battery, then self-review. Shape modules (tests / abi / polyglot / bug-reproduce-first) are enforced by the output schema.',
  phases: [
    {
      title: 'Rederive',
      detail:
        'Verify the issue-body claims against current code, confirm the work shape, and post the plan-of-record as an issue comment. Cross-compile verification is required for any Apple/macOS-path change.',
    },
    {
      title: 'Implement',
      detail:
        'The zone-matched build lead implements in its own git worktree with checkpoint commits at logical boundaries.',
    },
    {
      title: 'Test',
      detail: 'local-ci-runner runs the gate battery in the worktree and returns a pass/fail table.',
    },
    {
      title: 'SelfReview',
      detail:
        'The lead reviews its own diff and emits the shape-module report (tests, needs_bench, abi, polyglot, bug-reproduce-first, deviations, follow-ups).',
    },
  ],
};

// args arrives as { issue, zones?, shape?, ... } — object or JSON string.
const input = typeof args === 'string' ? JSON.parse(args) : args || {};
const issue = input.issue;
const zones = Array.isArray(input.zones) ? input.zones : [];
const shape = input.shape || 'implement'; // 'implement' | 'bug-reproduce-first'

// Pick the single build lead by zone; null means generic reasoning.
// KEEP-IN-SYNC(zone-router): implement-ticket.js, verify-change.js, draft-design.js, run-research.js, fix-ticket.js
function leadForZones(zoneList) {
  const z = zoneList.map((s) => String(s).toLowerCase());
  const has = (...keys) => keys.some((k) => z.some((zone) => zone.includes(k)));
  if (has('abi', 'plugin')) return 'plugin-abi-expert';
  if (has('python', 'deno', 'polyglot', 'ipc', 'escalate', 'iceoryx')) return 'polyglot-ipc-expert';
  if (has('package', 'registry', 'schema', 'slpkg', 'module-loader')) return 'package-registry-expert';
  if (has('vulkan', 'rhi', 'video', 'gpu', 'codec', 'kernel', 'texture')) return 'gpu-vulkan-expert';
  if (has('camera', 'v4l2', 'media', 'audio', 'display', 'modifier')) return 'linux-media-expert';
  return null;
}

const lead = leadForZones(zones);

function leadOpts(extra) {
  const o = Object.assign({}, extra);
  if (lead) o.agentType = lead;
  else o.model = 'opus';
  return o;
}

const rederiveSchema = {
  type: 'object',
  properties: {
    plan_of_record: { type: 'string' },
    stale_claims: { type: 'array', items: { type: 'string' } },
    shape_confirmed: { type: 'string' },
    macos_cross_compile_required: { type: 'boolean' },
    posted: { type: 'boolean' },
  },
  required: ['plan_of_record', 'shape_confirmed', 'posted'],
};

const implementSchema = {
  type: 'object',
  properties: {
    // K1: the branch/worktree/diff facts the Test + SelfReview phases run against.
    worktree_path: { type: 'string' },
    branch: { type: 'string' },
    commits: { type: 'array', items: { type: 'string' } },
    diff_stat: { type: 'string' },
    // K3: explicit test-report shape instead of a free-form object.
    tests: {
      type: 'object',
      properties: {
        added: { type: 'array', items: { type: 'string' } },
        reverted_fail_confirmed: { type: 'boolean' },
        command: { type: 'string' },
        notes: { type: 'string' },
      },
    },
    needs_bench: { type: 'boolean' },
    abi: { type: ['object', 'null'] },
    polyglot: { type: ['object', 'null'] },
    bug_reproduce_first: { type: ['object', 'null'] },
    deviations: { type: 'array', items: { type: 'string' } },
    followup_candidates: { type: 'array', items: { type: 'string' } },
  },
  required: ['worktree_path', 'branch', 'commits', 'diff_stat', 'tests', 'needs_bench', 'deviations', 'followup_candidates'],
};

// K3: degrade-not-crash. A schema-forced agent that exhausts the harness
// StructuredOutput retry-cap would otherwise crash the whole workflow. Try the
// schema'd call; on a null/failed result retry ONCE schema-free, asking for a
// single JSON object matching the shape, JSON.parse the reply, and only then
// give up — returning { degraded: true } + a log line so the phase continues
// degraded instead of the run dying.
async function resilientAgent(prompt, opts) {
  const first = await agent(prompt, opts);
  if (first) return first;
  const options = opts || {};
  const { schema, ...schemaFree } = options;
  const shape = schema ? JSON.stringify(schema) : '{}';
  const retry = await agent(
    `${prompt}\n\nReturn ONLY a single JSON object matching this shape — no prose, no code fence: ${shape}`,
    schemaFree,
  );
  if (retry && typeof retry === 'object') return retry;
  if (typeof retry === 'string') {
    try {
      return JSON.parse(retry);
    } catch (parseError) {
      log(`resilientAgent: schema-free retry did not parse (${options.label || 'unlabeled'}); continuing degraded`);
      return { degraded: true };
    }
  }
  log(`resilientAgent: schema-free retry returned no usable output (${options.label || 'unlabeled'}); continuing degraded`);
  return { degraded: true };
}

phase('Rederive');
const rederive =
  (await resilientAgent(
    `Rederive issue #${issue} (zones: ${zones.join(', ') || 'unspecified'}, shape: ${shape}) against CURRENT code. ` +
      `The issue body is the goal, not a spec — verify every specific claim (file paths, referenced code, listed defects) ` +
      `against the tree and flag what has gone stale. Confirm the work shape. If the change touches any Apple/macOS path, ` +
      `note that cross-compile verification (cargo check --target aarch64-apple-darwin, on Linux) is required and set ` +
      `macos_cross_compile_required. ` +
      `If the zones include package/registry work: crate deps in packages declare version = "0.6.0" but resolve to the ` +
      `local checkout via \`streamlib link --engine\` / [patch.crates-io] — there is NO crates registry and NO publish ` +
      `step. Treat any "blocked on republish" conclusion as a misdiagnosis to re-verify against ` +
      `docs/architecture/static-registry.md. ` +
      `Post the plan-of-record as an issue comment via gh. Do NOT start implementing yet.`,
    leadOpts({ phase: 'Rederive', label: `rederive:${lead || 'generic'}`, schema: rederiveSchema }),
  )) || {};
log(`rederive posted=${rederive.posted === true} shape=${rederive.shape_confirmed || shape}`);

phase('Implement');
const bugFirst =
  shape === 'bug-reproduce-first'
    ? `This is a bug-reproduce-first ticket: commit a FAILING test that reproduces the bug BEFORE the fix, then make it pass. `
    : ``;
const implemented =
  (await resilientAgent(
    `Implement issue #${issue} per the posted plan-of-record. ${bugFirst}` +
      `Work in this worktree. FIRST, verify your base is fresh: run \`git merge-base --is-ancestor origin/main HEAD\` in ` +
      `your worktree; if HEAD is behind origin/main, rebase onto origin/main BEFORE writing any code so you build on the ` +
      `just-merged surfaces (a stale base is what lets a fabricated no-diff "success" slip through). ` +
      `Create — or rename your worktree branch to — the canonical \`feat/${issue}-<slug>\` branch, and return its exact ` +
      `name in the \`branch\` field. Make checkpoint commits at logical boundaries (commits are contractual, not optional). ` +
      `Hold the engine doctrine: extend the existing core system, never spin up a parallel abstraction; production-grade ` +
      `error taxonomy + tracing on engine work; new .rs files carry the BUSL header; tracing not println!/eprintln!. ` +
      `If the change crosses the plugin ABI, the abi module of your report must reflect the abi_version bump, updated ` +
      `layout tests, and slot reservation. If it is pipeline-level polyglot work, Python AND Deno both ship (or set an ` +
      `explicit schema_only_rationale). If a hot path changed and a microbenchmark is warranted, set needs_bench. ` +
      `Emit the shape-module report as your structured output — including the absolute \`worktree_path\`, the canonical ` +
      `\`branch\`, your checkpoint commit shas in \`commits\`, and the output of \`git diff origin/main --stat\` in ` +
      `\`diff_stat\`.\n\nPlan-of-record: ${rederive.plan_of_record || '(none posted)'}`,
    leadOpts({ phase: 'Implement', label: `implement:${lead || 'generic'}`, isolation: 'worktree', schema: implementSchema }),
  )) || {};
log(`implement done: branch=${implemented.branch || '(none)'} needs_bench=${implemented.needs_bench === true} deviations=${(implemented.deviations || []).length}`);

phase('Test');
const worktreePath = implemented.worktree_path || '';
const ciTable =
  (await agent(
    `Run the local gate battery for issue #${issue}'s branch (${implemented.branch || 'unknown branch'}) in the change ` +
      `worktree at: ${worktreePath || '(MISSING — implement phase returned no worktree_path)'}. ` +
      `FIRST cd into that worktree. HARD GUARD: if the worktree path is missing/empty OR ` +
      `\`git -C '${worktreePath}' diff origin/main --stat\` is EMPTY, FAIL immediately and report a no-diff failure — ` +
      `do NOT run the gates against an empty or wrong tree (a fabricated no-diff "success" must not pass to self-review). ` +
      `Otherwise derive the gates from .github/workflows/*.yml and the xtask lint suite at run time and return the ` +
      `pass/fail table. Do not edit anything.`,
    { agentType: 'local-ci-runner', phase: 'Test', label: 'local-ci' },
  )) || {};
log('local gate battery complete');

phase('SelfReview');
const finalReport =
  (await resilientAgent(
    `Self-review the diff for issue #${issue} against its plan-of-record and the local gate results below. ` +
      `Do the review IN the change worktree at ${worktreePath || '(MISSING)'} — cd there first; re-run any gate you ` +
      `re-check contractually in that worktree, never the primary checkout. ` +
      `Confirm scope discipline (nothing outside the ticket), that every claimed test would FAIL if the fix were reverted, ` +
      `naming passes the zero-context test, and docs/headers conventions hold. Re-emit the shape-module report — keeping ` +
      `the correct \`worktree_path\`, \`branch\`, \`commits\`, and \`diff_stat\` — correcting any field the implement ` +
      `phase got wrong, and list follow-up candidates (do not file them).\n\n` +
      `Implement report: ${JSON.stringify(implemented)}\nLocal gates: ${JSON.stringify(ciTable)}`,
    leadOpts({ phase: 'SelfReview', label: `selfreview:${lead || 'generic'}`, schema: implementSchema }),
  )) || {};

return {
  branch: finalReport.branch || implemented.branch || null,
  worktree_path: finalReport.worktree_path || implemented.worktree_path || null,
  commits: finalReport.commits || implemented.commits || [],
  diff_stat: finalReport.diff_stat || implemented.diff_stat || '',
  tests: finalReport.tests || {},
  needs_bench: finalReport.needs_bench === true,
  abi: finalReport.abi || null,
  polyglot: finalReport.polyglot || null,
  bug_reproduce_first: finalReport.bug_reproduce_first || null,
  deviations: finalReport.deviations || [],
  followup_candidates: finalReport.followup_candidates || [],
  lead: lead || 'generic',
};
