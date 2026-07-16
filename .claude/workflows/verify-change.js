// verify-change workflow — the independent gate a branch clears before a draft
// PR opens. Stage A is the always-on change-verifier; Stage B adds path-routed
// read-only expert lenses (and evidence re-validation when E2E is claimed);
// adjudication opens a DRAFT PR on PASS, never a merge.

export const meta = {
  name: 'verify-change',
  description:
    'Adjudicate a branch before opening a PR: the change-verifier (Stage A) plus parallel path-routed domain lenses and, when the branch claims E2E evidence, the evidence-verifier (Stage B). Any blocker → FIX; an unresolved owner question → DISCUSS; else PASS opens a draft PR.',
  phases: [
    { title: 'Verify', detail: 'Stage A: the always-on change-verifier reviews the diff against the ticket and returns its verdict JSON.' },
    { title: 'Lenses', detail: 'Stage B: parallel read-only domain lenses over the diff, plus evidence-verifier if E2E evidence is claimed.' },
    { title: 'Adjudicate', detail: 'Merge findings → FIX / DISCUSS / PASS; PASS opens a draft PR via gh (never a merge).' },
  ],
};

// args: { issue, branch, zones?, claims_e2e?, ... } — object or JSON string.
const input = typeof args === 'string' ? JSON.parse(args) : args || {};
const issue = input.issue;
const branch = input.branch; // K4: verify addresses a branch, not just an issue number.
const zones = Array.isArray(input.zones) ? input.zones : [];
const claimsE2e = input.claims_e2e === true;

// Zones → the domain experts to run as read-only lenses over the diff.
// KEEP-IN-SYNC(zone-router): implement-ticket.js, verify-change.js, draft-design.js, run-research.js, fix-ticket.js
function expertsForZones(zoneList) {
  const z = zoneList.map((s) => String(s).toLowerCase());
  const has = (...keys) => keys.some((k) => z.some((zone) => zone.includes(k)));
  const experts = [];
  if (has('abi', 'plugin')) experts.push('plugin-abi-expert');
  if (has('python', 'deno', 'polyglot', 'ipc', 'escalate', 'iceoryx')) experts.push('polyglot-ipc-expert');
  if (has('package', 'registry', 'schema', 'slpkg', 'module-loader')) experts.push('package-registry-expert');
  if (has('vulkan', 'rhi', 'video', 'gpu', 'codec', 'kernel', 'texture')) experts.push('gpu-vulkan-expert');
  if (has('camera', 'v4l2', 'media', 'audio', 'display', 'modifier')) experts.push('linux-media-expert');
  return experts;
}

// N2: severity is a closed taxonomy. Only `owner-question` (or an ESCALATE
// verdict) demotes PASS → DISCUSS; `should-fix`/`low`/`info` ride the PR body as
// documented review items; `blocker` forces FIX.
const SEVERITY_ENUM = ['blocker', 'should-fix', 'low', 'owner-question', 'info'];

const verdictSchema = {
  type: 'object',
  properties: {
    verdict: { type: 'string' },
    findings: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          severity: { type: 'string', enum: SEVERITY_ENUM },
          file: { type: 'string' },
          line: { type: 'number' },
          claim: { type: 'string' },
          evidence: { type: 'string' },
          suggested_next_step: { type: 'string' },
        },
      },
    },
    lens: { type: 'string' },
    coverage_notes: { type: 'string' },
  },
  required: ['verdict', 'findings'],
};

// N2 taxonomy, appended to every reviewer prompt so severities are calibrated.
const severityTaxonomy =
  `Severity taxonomy (use EXACTLY one of these per finding): ` +
  `blocker (the change is wrong / a gate is red — forces a FIX); ` +
  `should-fix (a real defect the owner would want fixed, but it can ship as a documented review item on the PR body); ` +
  `low (a nit — naming, a doc line); ` +
  `owner-question (RESERVED for a call only the repo owner can make — scope, product direction, or a merge decision — this is the ONLY finding severity that parks the PR for the owner); ` +
  `info (an observation, no action). ` +
  `Do NOT mark rig-gated deferrals, doc nits, or "confirm you meant X" as owner-question.`;

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

const experts = expertsForZones(zones);

// K4: cheap mechanical pre-flight BEFORE the expensive verifier + parallel lenses
// spawn. The runtime exposes no direct shell, so a single sonnet guard runs the
// three ground-truth checks (issue open? branch exists? diff vs origin/main
// non-empty?). If any fails, return an error verdict immediately and spawn
// nothing else — turning a wasted full verify (wrong/empty/merged branch) into
// one cheap call.
if (!branch) {
  log('verify-change: no branch supplied — cannot verify a change without a branch');
  return {
    verdict: 'ERROR',
    findings: [{ severity: 'blocker', claim: 'verify-change was invoked without args.branch', evidence: 'input.branch is empty', suggested_next_step: 'invoke verify-change with the canonical feat/<issue>-<slug> branch' }],
    pr_number: null,
  };
}
phase('Guard');
const guard =
  (await agent(
    `Pre-flight ground-truth check for verifying issue #${issue} on branch \`${branch}\` — read-only, do NOT edit. ` +
      `Confirm all three: (1) issue #${issue} is OPEN (\`gh issue view ${issue} --json state\`); (2) the branch ` +
      `\`${branch}\` exists (\`git rev-parse --verify ${branch}\` or \`git ls-remote --exit-code --heads origin ${branch}\`); ` +
      `(3) \`git diff origin/main..${branch} --stat\` is NON-EMPTY. Return { ok: true } only if all three hold; ` +
      `otherwise { ok: false, reason: "<which check failed>" }.`,
    { phase: 'Guard', label: 'branch-guard', model: 'sonnet', schema: { type: 'object', properties: { ok: { type: 'boolean' }, reason: { type: 'string' } }, required: ['ok'] } },
  )) || {};
if (guard.ok !== true) {
  const reason = guard.reason || 'branch-guard produced no result';
  log(`verify-change: branch-guard failed (${reason}); returning error verdict, no reviewers spawned`);
  return {
    verdict: 'ERROR',
    findings: [{ severity: 'blocker', claim: 'branch-guard pre-flight failed', evidence: reason, suggested_next_step: 'resolve the branch/issue/diff issue before re-running verify-change' }],
    pr_number: null,
  };
}

phase('Verify');
// A null/degraded stage-A result (agent skipped, died, or degraded past its
// schema) is treated as blocked, not a pass.
const stageARaw = await resilientAgent(
  `Independently review the diff on issue #${issue}'s branch \`${branch}\` against the ticket. You are read-only; run ` +
    `the tests yourself and trust no claim. Emit exactly your verdict JSON. ${severityTaxonomy}`,
  { agentType: 'change-verifier', phase: 'Verify', label: 'change-verifier', schema: verdictSchema },
);
const stageA =
  stageARaw && stageARaw.verdict
    ? stageARaw
    : { verdict: 'REJECT', findings: [{ severity: 'blocker', claim: 'change-verifier produced no usable result', evidence: 'agent returned null or degraded past its schema', suggested_next_step: 're-run the verifier' }] };
log(`change-verifier verdict=${stageA.verdict} findings=${(stageA.findings || []).length}`);

phase('Lenses');
const lensThunks = experts.map((expert) => () =>
  resilientAgent(
    `Read-only lens over the diff on issue #${issue}'s branch \`${branch}\`, from your domain's angle ` +
      `(zones: ${zones.join(', ')}). Do NOT edit. Find domain-specific correctness / invariant violations the mechanical ` +
      `gates can't catch; cite file:line. Emit findings in the verdict JSON shape (verdict APPROVE/REJECT/ESCALATE, ` +
      `findings[], lens, coverage_notes). ${severityTaxonomy}`,
    { agentType: expert, phase: 'Lenses', label: `lens:${expert}`, schema: verdictSchema },
  ),
);
if (claimsE2e) {
  lensThunks.push(() =>
    resilientAgent(
      `The branch \`${branch}\` on issue #${issue} claims E2E evidence. Locate the referenced output artifacts and run ` +
        `the Phase-B audit against them (log gates all zero, read + describe every sampled PNG, PSNR vs thresholds). If the ` +
        `artifacts are absent the evidence is unverified — say so. Emit findings in the verdict JSON shape. ${severityTaxonomy}`,
      { agentType: 'evidence-verifier', phase: 'Lenses', label: 'evidence-verifier', schema: verdictSchema },
    ),
  );
}
const lensResults = lensThunks.length > 0 ? (await parallel(lensThunks)).filter(Boolean) : [];
log(`lenses complete: ${lensResults.length} of ${lensThunks.length} returned (${experts.length} domain + ${claimsE2e ? 1 : 0} evidence)`);

phase('Adjudicate');
const all = [stageA].concat(lensResults);
const findings = [];
for (const r of all) for (const f of (r && r.findings) || []) findings.push(f);

const hasBlocker = findings.some((f) => f.severity === 'blocker');
// A REJECT verdict is FIX-worthy on its own — never trust that a rejecting
// reviewer also remembered to tag a finding `blocker` (or emitted findings at all).
const hasReject = all.some((r) => r && r.verdict === 'REJECT');
const hasEscalate = all.some((r) => r && r.verdict === 'ESCALATE');
// N2: DISCUSS fires ONLY on a call reserved for the owner. `should-fix`/`low`/
// `info` findings do NOT park — they ride the PR body as documented review items
// while PASS opens the draft PR.
const hasOwnerQuestion = findings.some((f) => f.severity === 'owner-question');
const reviewItems = findings.filter((f) => f.severity === 'should-fix' || f.severity === 'low' || f.severity === 'info');

let verdict;
let prNumber = null;
if (hasBlocker || hasReject) {
  verdict = 'FIX'; // caller bounces once within the attempt cap
} else if (hasEscalate || hasOwnerQuestion) {
  verdict = 'DISCUSS';
} else {
  verdict = 'PASS';
  const opened =
    (await resilientAgent(
      `All lenses cleared the branch \`${branch}\` on issue #${issue}. Open a DRAFT pull request via gh ` +
        `(gh pr create --draft --head ${branch}). NEVER merge — merging is the owner's call. Fill the PR body with the ` +
        `ticket link, the change summary, the test evidence, any E2E report, and a "Review items (non-blocking)" section ` +
        `listing these findings verbatim so the owner sees them in-context: ${JSON.stringify(reviewItems)}. ` +
        `Return the PR number.`,
      { phase: 'Adjudicate', label: 'open-draft-pr', model: 'sonnet', schema: { type: 'object', properties: { pr_number: { type: 'number' } }, required: ['pr_number'] } },
    )) || {};
  prNumber = opened.pr_number || null;
}
log(`adjudicated verdict=${verdict} pr=${prNumber} review_items=${reviewItems.length}`);

return { verdict, findings, pr_number: prNumber, review_items: reviewItems };
