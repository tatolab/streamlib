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

// args: { issue, zones?, claims_e2e?, ... } — object or JSON string.
const input = typeof args === 'string' ? JSON.parse(args) : args || {};
const issue = input.issue;
const zones = Array.isArray(input.zones) ? input.zones : [];
const claimsE2e = input.claims_e2e === true;

// Zones → the domain experts to run as read-only lenses over the diff.
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

const verdictSchema = {
  type: 'object',
  properties: {
    verdict: { type: 'string' },
    findings: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          severity: { type: 'string' },
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

const experts = expertsForZones(zones);

phase('Verify');
// A null stage-A result (agent skipped or died) is treated as blocked, not a pass.
const stageA =
  (await agent(
    `Independently review the diff on issue #${issue}'s branch against the ticket. You are read-only; run the tests ` +
      `yourself and trust no claim. Emit exactly your verdict JSON.`,
    { agentType: 'change-verifier', phase: 'Verify', label: 'change-verifier', schema: verdictSchema },
  )) || { verdict: 'REJECT', findings: [{ severity: 'blocker', claim: 'change-verifier produced no result', evidence: 'agent returned null', suggested_next_step: 're-run the verifier' }] };
log(`change-verifier verdict=${stageA.verdict} findings=${(stageA.findings || []).length}`);

phase('Lenses');
const lensThunks = experts.map((expert) => () =>
  agent(
    `Read-only lens over the diff on issue #${issue}'s branch, from your domain's angle (zones: ${zones.join(', ')}). ` +
      `Do NOT edit. Find domain-specific correctness / invariant violations the mechanical gates can't catch; cite file:line. ` +
      `Emit findings in the verdict JSON shape (verdict APPROVE/REJECT/ESCALATE, findings[], lens, coverage_notes).`,
    { agentType: expert, phase: 'Lenses', label: `lens:${expert}`, schema: verdictSchema },
  ),
);
if (claimsE2e) {
  lensThunks.push(() =>
    agent(
      `The branch on issue #${issue} claims E2E evidence. Locate the referenced output artifacts and run the Phase-B ` +
        `audit against them (log gates all zero, read + describe every sampled PNG, PSNR vs thresholds). If the artifacts ` +
        `are absent the evidence is unverified — say so. Emit findings in the verdict JSON shape.`,
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
const hasOpenQuestion = findings.some((f) => f.severity === 'question');

let verdict;
let prNumber = null;
if (hasBlocker || hasReject) {
  verdict = 'FIX'; // caller bounces once within the attempt cap
} else if (hasEscalate || hasOpenQuestion) {
  verdict = 'DISCUSS';
} else {
  verdict = 'PASS';
  const opened =
    (await agent(
      `All lenses cleared the branch on issue #${issue}. Open a DRAFT pull request via gh (gh pr create --draft). ` +
        `NEVER merge — merging is the owner's call. Fill the PR body with the ticket link, the change summary, the test ` +
        `evidence, and any E2E report. Return the PR number.`,
      { phase: 'Adjudicate', label: 'open-draft-pr', model: 'opus', schema: { type: 'object', properties: { pr_number: { type: 'number' } }, required: ['pr_number'] } },
    )) || {};
  prNumber = opened.pr_number || null;
}
log(`adjudicated verdict=${verdict} pr=${prNumber}`);

return { verdict, findings, pr_number: prNumber };
