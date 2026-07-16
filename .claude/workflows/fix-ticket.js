// fix-ticket workflow — the missing primitive for touching an EXISTING branch.
// Unlike implement-ticket (which always starts fresh from Rederive in a new
// worktree), fix-ticket works IN the branch already under review: it applies a
// set of enumerated findings (mode: 'fix') or rebases the branch onto origin/main
// and resolves (mode: 'rebase'), re-runs the in-worktree gate battery, and
// returns the K1-shaped report. It replaces the hand-rolled fix-<issue>-attemptN
// scratchpad scripts and the main-context hand-rebases.

export const meta = {
  name: 'fix-ticket',
  description:
    'Apply enumerated verify findings to an existing branch (mode: fix), or rebase that branch onto origin/main and resolve (mode: rebase), re-run the in-worktree gate battery, and return the branch/worktree/commits/diff/gates report. Works in the existing branch — never a fresh worktree from Rederive.',
  phases: [
    {
      title: 'Apply',
      detail:
        'The zone-matched lead checks out the existing branch in a worktree and either applies the enumerated findings (fix) or rebases onto origin/main and resolves conflicts (rebase). Checkpoint commits; force-push on rebase.',
    },
    {
      title: 'Test',
      detail: 'local-ci-runner runs the gate battery IN that worktree and returns a pass/fail table.',
    },
    {
      title: 'Report',
      detail: 'The lead re-emits the branch/worktree/commits/diff_stat report over the post-fix tree.',
    },
  ],
};

// args: { issue, branch, findings: [...], mode: 'fix' | 'rebase' } — object or JSON string.
const input = typeof args === 'string' ? JSON.parse(args) : args || {};
const issue = input.issue;
const branch = input.branch;
const mode = input.mode === 'rebase' ? 'rebase' : 'fix';
const findings = Array.isArray(input.findings) ? input.findings : [];
const zones = Array.isArray(input.zones) ? input.zones : [];

// Pick the single lead by zone; null means generic reasoning.
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

// K3: degrade-not-crash — see implement-ticket.js. On a null/failed structured
// result, retry ONCE schema-free and parse; then continue degraded, not dead.
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

// The K1-shaped report the lead returns over the post-fix tree.
const fixReportSchema = {
  type: 'object',
  properties: {
    worktree_path: { type: 'string' },
    branch: { type: 'string' },
    commits: { type: 'array', items: { type: 'string' } },
    diff_stat: { type: 'string' },
    applied: { type: 'array', items: { type: 'string' } },
    unresolved: { type: 'array', items: { type: 'string' } },
    rebased: { type: 'boolean' },
    force_pushed: { type: 'boolean' },
  },
  required: ['worktree_path', 'branch', 'commits', 'diff_stat'],
};

// A branch is required — fix-ticket never operates without one.
if (!branch) {
  log('fix-ticket: no branch supplied — nothing to fix');
  return {
    branch: null,
    worktree_path: null,
    commits: [],
    diff_stat: '',
    gates: {},
    error: 'fix-ticket was invoked without args.branch',
  };
}

phase('Apply');
const applyPrompt =
  mode === 'rebase'
    ? `Rebase branch \`${branch}\` (issue #${issue}) onto origin/main and resolve. Work IN the existing branch: check ` +
      `it out in a git worktree (\`git worktree add <path> ${branch}\` if one is not already live) — do NOT create a ` +
      `fresh feature branch and do NOT restart from Rederive. Then: \`git fetch origin\`; rebase \`${branch}\` onto ` +
      `origin/main; resolve every conflict preserving the branch's intent; make checkpoint commits as needed; and ` +
      `force-push with lease (\`git push --force-with-lease\`). Set rebased and force_pushed. Return the report over ` +
      `the rebased tree — absolute \`worktree_path\`, \`branch\`, the resulting \`commits\`, and ` +
      `\`git diff origin/main --stat\` in \`diff_stat\`.`
    : `Apply the enumerated verify findings to branch \`${branch}\` (issue #${issue}). Work IN the existing branch: ` +
      `check it out in a git worktree (\`git worktree add <path> ${branch}\` if one is not already live) — do NOT ` +
      `create a fresh feature branch and do NOT restart from Rederive. Apply ONLY the findings listed below (no ` +
      `scope creep, no unrelated auto-fixes); hold the engine doctrine and licensing/logging conventions. Make ` +
      `checkpoint commits at logical boundaries. List which findings you addressed in \`applied\` and any you could ` +
      `not in \`unresolved\`. Return the report over the fixed tree — absolute \`worktree_path\`, \`branch\`, the ` +
      `resulting \`commits\`, and \`git diff origin/main --stat\` in \`diff_stat\`.\n\n` +
      `Findings to apply (JSON): ${JSON.stringify(findings)}`;

const applied =
  (await resilientAgent(applyPrompt, leadOpts({ phase: 'Apply', label: `fix:${lead || 'generic'}:${mode}` }))) || {};
log(`fix applied: mode=${mode} branch=${applied.branch || branch} commits=${(applied.commits || []).length} unresolved=${(applied.unresolved || []).length}`);

phase('Test');
const worktreePath = applied.worktree_path || '';
const ciTable =
  (await agent(
    `Run the local gate battery for issue #${issue}'s branch (${applied.branch || branch}) in the change worktree at: ` +
      `${worktreePath || '(MISSING — the fix phase returned no worktree_path)'}. FIRST cd into that worktree. ` +
      `HARD GUARD: if the worktree path is missing/empty OR \`git -C '${worktreePath}' diff origin/main --stat\` is ` +
      `EMPTY, FAIL immediately and report a no-diff failure — do NOT run gates against an empty or wrong tree. ` +
      `Otherwise derive the gates from .github/workflows/*.yml and the xtask lint suite at run time and return the ` +
      `pass/fail table. Do not edit anything.`,
    { agentType: 'local-ci-runner', phase: 'Test', label: 'local-ci' },
  )) || {};
log('local gate battery complete');

phase('Report');
const finalReport =
  (await resilientAgent(
    `Re-emit the branch/worktree/commits/diff report for issue #${issue}'s branch \`${applied.branch || branch}\` over ` +
      `the post-${mode} tree in the worktree at ${worktreePath || '(MISSING)'} — cd there first. Confirm the ` +
      `\`diff_stat\` is non-empty and the \`commits\` list is accurate, and correct any field the Apply phase got wrong. ` +
      `Local gate results (for context, do not re-run here): ${JSON.stringify(ciTable)}`,
    leadOpts({ phase: 'Report', label: `fixreport:${lead || 'generic'}`, schema: fixReportSchema }),
  )) || {};

return {
  mode,
  branch: finalReport.branch || applied.branch || branch,
  worktree_path: finalReport.worktree_path || applied.worktree_path || null,
  commits: finalReport.commits || applied.commits || [],
  diff_stat: finalReport.diff_stat || applied.diff_stat || '',
  applied: applied.applied || [],
  unresolved: applied.unresolved || [],
  rebased: applied.rebased === true,
  force_pushed: applied.force_pushed === true,
  gates: ciTable,
  lead: lead || 'generic',
};
