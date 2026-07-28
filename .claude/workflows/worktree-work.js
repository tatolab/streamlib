// worktree-work — one ticket's entire worktree lifecycle: create, build, gate,
// verify, bounded fix rounds, PR, sweep. The stage list is composed from the
// ticket's classification rather than fixed, so a bug ticket lands a failing
// test first, an ABI ticket runs the contract stage, and a leaf docs ticket runs
// neither.
//
// Level 1 in the workflow tree: the orchestrator calls this via workflow(), so
// this script may only use agent() — workflow() nesting is one level deep.

export const meta = {
  name: 'worktree-work',
  description:
    "One ticket from a fresh worktree to an opened PR. Composes its stage list from the ticket's shape and zones, walks it, then verifies with bounded fix rounds.",
  phases: [
    { title: 'Claim', detail: 'Create the worktree and canonical branch from a fresh origin/main, and post the claim comment.' },
    { title: 'Rederive', detail: 'Verify the issue-body claims against current code and post the plan-of-record.' },
    { title: 'FailingTest', detail: 'bug-reproduce-first only: commit a failing test that reproduces the bug.' },
    { title: 'Implement', detail: 'The zone-matched build lead implements in the worktree with checkpoint commits.' },
    { title: 'AbiContract', detail: 'ABI zones only: version bump, layout regression tests, slot reservation.' },
    { title: 'PolyglotParity', detail: 'Python/Deno zones only: both SDKs ship, or an explicit schema-only rationale.' },
    { title: 'CrossCompile', detail: 'Apple-path changes only: cargo check --target aarch64-apple-darwin.' },
    { title: 'Gates', detail: 'local-ci-runner runs the gate battery in the worktree.' },
    { title: 'Bench', detail: 'Hot-path changes only: a microbenchmark over the changed path.' },
    { title: 'SelfReview', detail: 'The lead reviews its own diff and re-emits the shape-module report.' },
    { title: 'Evidence', detail: 'Rig-touching tickets only: run the live pipeline and capture evidence.' },
    { title: 'Verify', detail: 'change-verifier plus path-routed read-only domain lenses, then adjudicate.' },
    { title: 'Fix', detail: 'Respond to the reviewers in the same worktree, bounded at six rounds.' },
  ],
};

const input = typeof args === 'string' ? JSON.parse(args) : args || {};
const issue = input.issue;
const repoRoot = input.repo_root || '.';
const zones = Array.isArray(input.zones) ? input.zones : [];
const shape = input.shape || 'implement';
const rigNeeds = Array.isArray(input.rig_needs) ? input.rig_needs : [];
const liveVerify = input.live_verify || 'unavailable';
const mode = input.mode === 'rebase' ? 'rebase' : 'work';
const slug = input.slug || `${issue}-ticket`;
const branchName = input.branch || `feat/${issue}-${slug}`;

// The team self-reviews until the branch is ready for a human, and nits force rounds
// too, so the bound has to be generous enough that escalation means "genuinely stuck"
// rather than "ran out of turns". Escalating a branch whose findings were merely
// unfinished spends the owner's attention on work the team could have completed.
const MAX_FIX_ROUNDS = 6;

function has(zoneList, ...keys) {
  const z = (zoneList || []).map((s) => String(s).toLowerCase());
  return keys.some((k) => z.some((zone) => zone.includes(k)));
}

// Fallback only — the orchestrator classifies once and passes `lead` down.
// KEEP-IN-SYNC(zone-router): worktree-work.js, draft-design.js, run-research.js
function leadForZones(zoneList) {
  if (has(zoneList, 'abi', 'plugin')) return 'plugin-abi-expert';
  if (has(zoneList, 'python', 'deno', 'polyglot', 'ipc', 'escalate', 'iceoryx')) return 'polyglot-ipc-expert';
  if (has(zoneList, 'package', 'package-source', 'registry', 'schema', 'slpkg', 'module-loader')) return 'package-source-expert';
  if (has(zoneList, 'vulkan', 'rhi', 'video', 'gpu', 'codec', 'kernel', 'texture')) return 'gpu-vulkan-expert';
  if (has(zoneList, 'camera', 'v4l2', 'media', 'audio', 'display', 'modifier')) return 'linux-media-expert';
  return null;
}

const lead = input.lead || leadForZones(zones);

function leadOpts(extra) {
  const o = Object.assign({}, extra);
  if (lead) o.agentType = lead;
  else o.model = 'opus';
  return o;
}

// A schema-forced agent that exhausts the harness StructuredOutput retry cap
// would otherwise kill the run. Retry once schema-free and parse, then continue
// degraded. No opts here ever carry `isolation` — the worktree is created once
// by the Claim stage, so a retry reuses it instead of forking a second tree.
async function resilientAgent(prompt, opts) {
  const options = opts || {};
  const { schema, ...schemaFree } = options;
  let first;
  try {
    first = await agent(prompt, opts);
  } catch (structuredThrow) {
    log(`resilientAgent: structured attempt threw (${options.label || 'unlabeled'}); falling back to schema-free retry`);
    first = null;
  }
  if (first) return first;
  const wanted = schema ? JSON.stringify(schema) : '{}';
  let retry;
  try {
    retry = await agent(
      `${prompt}\n\nReturn ONLY a single JSON object matching this shape — no prose, no code fence: ${wanted}`,
      schemaFree,
    );
  } catch (retryThrow) {
    log(`resilientAgent: schema-free retry also threw (${options.label || 'unlabeled'}); continuing degraded`);
    return { degraded: true };
  }
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

const claimSchema = {
  type: 'object',
  properties: {
    worktree_path: { type: 'string' },
    branch: { type: 'string' },
    claim_comment_id: { type: ['number', 'null'] },
    created: { type: 'boolean' },
  },
  required: ['worktree_path', 'branch', 'created'],
};

const rederiveSchema = {
  type: 'object',
  properties: {
    plan_of_record: { type: 'string' },
    stale_claims: { type: 'array', items: { type: 'string' } },
    shape_confirmed: { type: 'string' },
    touches_apple: { type: 'boolean' },
    needs_bench: { type: 'boolean' },
    posted: { type: 'boolean' },
    plan_comment_id: { type: ['number', 'null'] },
  },
  required: ['plan_of_record', 'shape_confirmed', 'posted'],
};

const implementSchema = {
  type: 'object',
  properties: {
    worktree_path: { type: 'string' },
    branch: { type: 'string' },
    commits: { type: 'array', items: { type: 'string' } },
    diff_stat: { type: 'string' },
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
  required: ['worktree_path', 'branch', 'commits', 'diff_stat', 'tests', 'deviations', 'followup_candidates'],
};

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

const severityTaxonomy =
  `Severity taxonomy (use EXACTLY one of these per finding): ` +
  `blocker (the change is wrong / a gate is red — forces a FIX); ` +
  `should-fix (a real defect — forces a FIX exactly like a blocker; it is a change request to the implementer, NOT something ` +
  `that ships as a note on the PR body); ` +
  `low (a nit — naming, a doc line; still handed to the implementer to clean up before a human reads the PR, because ` +
  `self-review exists so a human never spends attention on a nit); ` +
  `owner-question (RESERVED for a call only the repo owner can make — scope, product direction, or a merge decision — this is the ONLY finding severity that parks the PR for the owner); ` +
  `info (an observation, no action). ` +
  `Do NOT mark rig-gated deferrals, doc nits, or "confirm you meant X" as owner-question.`;

const STAGE = {
  CLAIM: 'claim',
  REDERIVE: 'rederive',
  FAILING_TEST: 'failing_test',
  IMPLEMENT: 'implement',
  ABI_CONTRACT: 'abi_contract',
  POLYGLOT_PARITY: 'polyglot_parity',
  CROSS_COMPILE: 'cross_compile',
  GATES: 'gates',
  BENCH: 'bench',
  SELF_REVIEW: 'self_review',
  EVIDENCE: 'evidence',
  VERIFY: 'verify',
};

// The composition rules. `touches_apple` and `needs_bench` are not knowable
// until Rederive has read the tree, so this runs twice — once from the
// orchestrator's classification and again after Rederive. Recomputation is
// additive: a stage already walked is never dropped.
function composeStages(t) {
  const stages = [STAGE.CLAIM, STAGE.REDERIVE];
  if (t.shape === 'bug-reproduce-first') stages.push(STAGE.FAILING_TEST);
  stages.push(STAGE.IMPLEMENT);
  if (has(t.zones, 'abi', 'plugin')) stages.push(STAGE.ABI_CONTRACT);
  if (has(t.zones, 'python', 'deno', 'polyglot')) stages.push(STAGE.POLYGLOT_PARITY);
  if (t.touches_apple) stages.push(STAGE.CROSS_COMPILE);
  stages.push(STAGE.GATES);
  if (t.needs_bench) stages.push(STAGE.BENCH);
  stages.push(STAGE.SELF_REVIEW);
  // Evidence precedes Verify so the evidence-verifier lens has artifacts to
  // audit, and follows Gates so a red build never burns a rig run.
  if (rigNeeds.length > 0 && liveVerify === 'available') stages.push(STAGE.EVIDENCE);
  stages.push(STAGE.VERIFY);
  return stages;
}

function expertsForZones(zoneList) {
  const experts = [];
  if (has(zoneList, 'abi', 'plugin')) experts.push('plugin-abi-expert');
  if (has(zoneList, 'python', 'deno', 'polyglot', 'ipc', 'escalate', 'iceoryx')) experts.push('polyglot-ipc-expert');
  if (has(zoneList, 'package', 'package-source', 'registry', 'schema', 'slpkg', 'module-loader')) experts.push('package-source-expert');
  if (has(zoneList, 'vulkan', 'rhi', 'video', 'gpu', 'codec', 'kernel', 'texture')) experts.push('gpu-vulkan-expert');
  if (has(zoneList, 'camera', 'v4l2', 'media', 'audio', 'display', 'modifier')) experts.push('linux-media-expert');
  return experts;
}

const ctx = {
  worktree_path: input.worktree_path || '',
  branch: branchName,
  claim_comment_id: null,
  plan_of_record: '',
  touches_apple: false,
  needs_bench: false,
  claims_e2e: false,
  report: {},
  gates: {},
  evidence: null,
};

function inWorktree() {
  return (
    `Work in the existing worktree at ${ctx.worktree_path}. cd there FIRST. Do NOT create a worktree, do NOT create a ` +
    `new branch, and never run gates or edits against the primary checkout — its build target is shared and its HEAD is ` +
    `someone else's. `
  );
}

async function runClaim() {
  phase('Claim');
  const path = `${repoRoot}/.claude/worktrees/${slug}`;
  const claimed =
    (await resilientAgent(
      `Claim issue #${issue} and prepare its worktree.\n` +
        `1. Post an owner-visible comment on issue #${issue}: "▶ claimed — <one sentence: what and why>". Use ` +
        `\`gh api\` so you get the comment id back, and return it as claim_comment_id.\n` +
        `2. Run: git -C ${repoRoot} fetch origin\n` +
        `3. Run: git -C ${repoRoot} worktree add ${path} -b ${branchName} origin/main\n` +
        `   This creates the branch AND the worktree from a fresh origin/main in one step, so the base cannot be stale. ` +
        `If the branch already exists, add the worktree against it instead of creating it.\n` +
        `Return the absolute worktree_path, the branch name, and created: true once the worktree exists.`,
      { phase: 'Claim', label: `claim:${issue}`, model: 'sonnet', schema: claimSchema },
    )) || {};
  if (claimed.worktree_path) ctx.worktree_path = claimed.worktree_path;
  if (claimed.branch) ctx.branch = claimed.branch;
  ctx.claim_comment_id = claimed.claim_comment_id || null;
  return claimed;
}

async function runRederive() {
  phase('Rederive');
  const r =
    (await resilientAgent(
      `Rederive issue #${issue} (zones: ${zones.join(', ') || 'unspecified'}, shape: ${shape}) against CURRENT code. ` +
        inWorktree() +
        `The issue body is the goal, not a spec — verify every specific claim (file paths, referenced code, listed defects) ` +
        `against the tree and flag what has gone stale. Confirm the work shape. Set touches_apple if the change reaches any ` +
        `Apple/macOS path (it will add a cross-compile stage). Set needs_bench if a hot path changes such that a ` +
        `microbenchmark is warranted. ` +
        `If the zones include package/registry work: crate deps in packages declare version = "0.6.0" but resolve to the ` +
        `local checkout via \`streamlib link --engine\` / [patch.crates-io] — resolve by version from a package source or ` +
        `\`streamlib link\` a checkout; there is no central package registry and no publish step. Treat any "blocked on ` +
        `republish" conclusion as a misdiagnosis to re-verify against docs/architecture/package-source.md. ` +
        `Post the plan-of-record as an issue comment via gh and return its id. Do NOT start implementing yet.`,
      leadOpts({ phase: 'Rederive', label: `rederive:${lead || 'generic'}`, schema: rederiveSchema }),
    )) || {};
  ctx.plan_of_record = r.plan_of_record || '';
  ctx.touches_apple = r.touches_apple === true;
  ctx.needs_bench = r.needs_bench === true;
  log(`rederive posted=${r.posted === true} shape=${r.shape_confirmed || shape} apple=${ctx.touches_apple} bench=${ctx.needs_bench}`);
  return r;
}

async function runFailingTest() {
  phase('FailingTest');
  return await resilientAgent(
    `Issue #${issue} is bug-reproduce-first. ` +
      inWorktree() +
      `Commit a FAILING test that reproduces the bug BEFORE any fix. Run it and confirm it fails for the right reason ` +
      `(not a compile error, not a missing fixture). Commit it on its own. Report the test path and the observed failure.`,
    leadOpts({ phase: 'FailingTest', label: `failing-test:${lead || 'generic'}` }),
  );
}

async function runImplement() {
  phase('Implement');
  const r =
    (await resilientAgent(
      `Implement issue #${issue} per the posted plan-of-record. ` +
        inWorktree() +
        `Make checkpoint commits at logical boundaries (commits are contractual, not optional). ` +
        `BEFORE returning your report, PUSH the branch to origin (\`git push -u origin ${ctx.branch}\`) so the verify ` +
        `stage's branch pre-flight can find it on origin — a report whose branch is not on origin is incomplete. ` +
        `Hold the engine doctrine: extend the existing core system, never spin up a parallel abstraction; production-grade ` +
        `error taxonomy + tracing on engine work; new .rs files carry the BUSL header; tracing not println!/eprintln!. ` +
        `Emit the shape-module report as your structured output — including \`worktree_path\` (${ctx.worktree_path}), the ` +
        `\`branch\`, your checkpoint commit shas in \`commits\`, and the output of \`git diff origin/main --stat\` in ` +
        `\`diff_stat\`.\n\nPlan-of-record: ${ctx.plan_of_record || '(none posted)'}`,
      leadOpts({ phase: 'Implement', label: `implement:${lead || 'generic'}`, schema: implementSchema }),
    )) || {};
  ctx.report = r;
  if (r.needs_bench === true) ctx.needs_bench = true;
  log(`implement done: branch=${r.branch || '(none)'} deviations=${(r.deviations || []).length}`);
  return r;
}

async function runAbiContract() {
  phase('AbiContract');
  return await resilientAgent(
    `Issue #${issue} crosses the plugin ABI. ` +
      inWorktree() +
      `Confirm and complete the ABI contract work: the abi_version bump, updated layout regression tests for every ` +
      `#[repr(C)] type in every language that mirrors it, and slot reservation. Report what you changed and what was ` +
      `already correct. If any part is missing, add it.`,
    { agentType: 'plugin-abi-expert', phase: 'AbiContract', label: 'abi-contract' },
  );
}

async function runPolyglotParity() {
  phase('PolyglotParity');
  return await resilientAgent(
    `Issue #${issue} is pipeline-level polyglot work. ` +
      inWorktree() +
      `Confirm Python AND Deno both ship the change, or record an explicit schema_only_rationale for why one does not. ` +
      `Check layout regression tests exist in each language that mirrors a #[repr(C)] type. Add what is missing.`,
    { agentType: 'polyglot-ipc-expert', phase: 'PolyglotParity', label: 'polyglot-parity' },
  );
}

async function runCrossCompile() {
  phase('CrossCompile');
  return await resilientAgent(
    `Issue #${issue} touches an Apple/macOS path. ` +
      inWorktree() +
      `Run \`cargo check --target aarch64-apple-darwin\` and report the result. No Apple file merges from Linux without ` +
      `this passing. If a real-device runtime check is also needed and cannot run here, say so — it becomes a follow-up ` +
      `noted on the PR, not a blocker.`,
    leadOpts({ phase: 'CrossCompile', label: 'cross-compile' }),
  );
}

async function runGates() {
  phase('Gates');
  const g =
    (await agent(
      `Run the local gate battery for issue #${issue}'s branch (${ctx.branch}) in the change worktree at ` +
        `${ctx.worktree_path || '(MISSING — the claim stage returned no worktree_path)'}. FIRST cd into that worktree. ` +
        `HARD GUARD: if the worktree path is missing/empty OR \`git -C '${ctx.worktree_path}' diff origin/main --stat\` is ` +
        `EMPTY, FAIL immediately and report a no-diff failure — do NOT run the gates against an empty or wrong tree (a ` +
        `fabricated no-diff "success" must not pass to self-review). Otherwise derive the gates from ` +
        `.github/workflows/*.yml and the xtask lint suite at run time and return the pass/fail table. Do not edit anything.`,
      { agentType: 'local-ci-runner', phase: 'Gates', label: 'local-ci' },
    )) || {};
  ctx.gates = g;
  log('local gate battery complete');
  return g;
}

async function runBench() {
  phase('Bench');
  return await resilientAgent(
    `Issue #${issue} changed a hot path. ` +
      inWorktree() +
      `Write and run a microbenchmark over the changed path, and report before/after numbers with the command used. If ` +
      `the change is not measurable, say so plainly rather than inventing a number.`,
    leadOpts({ phase: 'Bench', label: 'bench' }),
  );
}

async function runSelfReview() {
  phase('SelfReview');
  const r =
    (await resilientAgent(
      `Self-review the diff for issue #${issue} against its plan-of-record and the local gate results below. ` +
        inWorktree() +
        `Re-run any gate you re-check contractually in that worktree. Confirm scope discipline (nothing outside the ` +
        `ticket), that every claimed test would FAIL if the fix were reverted, naming passes the zero-context test, and ` +
        `docs/headers conventions hold. Re-emit the shape-module report — keeping the correct \`worktree_path\`, ` +
        `\`branch\`, \`commits\`, and \`diff_stat\` — correcting any field the implement stage got wrong, and list ` +
        `follow-up candidates (do not file them).\n\n` +
        `Implement report: ${JSON.stringify(ctx.report)}\nLocal gates: ${JSON.stringify(ctx.gates)}`,
      leadOpts({ phase: 'SelfReview', label: `selfreview:${lead || 'generic'}`, schema: implementSchema }),
    )) || {};
  ctx.report = Object.assign({}, ctx.report, r);
  return r;
}

async function runEvidence() {
  phase('Evidence');
  const e = await resilientAgent(
    `Issue #${issue} touches the rig (${rigNeeds.join(', ')}) and live verification is available. Run ` +
      `/verify-live in LOOP-RUN mode for this branch: ` +
      inWorktree() +
      `build in the sandbox, run the built binary with the Bash dangerouslyDisableSandbox bypass and DISPLAY=:1, capture ` +
      `the window, Read the captured image, compute PSNR against the expected scenario, and check the log gates. ` +
      `Attach the artifacts (R2 via the attach-artifact skill) and return the artifact URLs plus the measured numbers. ` +
      `SAFETY: read-only observation evals only. Anything with a real-world safety gate (actuators, motors, drone ` +
      `control) must NOT be run — return an owner-approval-required result instead.`,
    leadOpts({ phase: 'Evidence', label: 'verify-live' }),
  );
  ctx.evidence = e;
  ctx.claims_e2e = true;
  return e;
}

async function runVerify(isFixRound, gatingLenses = new Set(), implementerResponse = null) {
  // What the implementer said they fixed and what they declined, so a re-checking
  // reviewer adjudicates a stated position instead of re-deriving it from the diff.
  const responseBlock = implementerResponse
    ? `\n\nThe implementer's response to the review (JSON): ${JSON.stringify(implementerResponse)}\n` +
      `Adjudicate it: confirm each claimed fix actually holds, and for each DECLINED item decide whether the reason is ` +
      `sound. Accept a sound decline and drop the finding. Re-raise it at the same severity if the reason is not sound ` +
      `or the fix is cosmetic.`
    : ``;
  phase('Verify');
  const guard =
    (await agent(
      `Pre-flight ground-truth check for verifying issue #${issue} on branch \`${ctx.branch}\` — read-only, do NOT edit. ` +
        `Confirm all three: (1) issue #${issue} is OPEN; (2) the branch \`${ctx.branch}\` exists on origin; ` +
        `(3) \`git -C ${ctx.worktree_path} diff origin/main --stat\` is NON-EMPTY. Return { ok: true } only if all three ` +
        `hold; otherwise { ok: false, reason: "<which check failed>" }. Also set touches_rust: true if the diff lists any ` +
        `path ending in \`.rs\`, else false.`,
      {
        phase: 'Verify',
        label: 'branch-guard',
        model: 'sonnet',
        schema: { type: 'object', properties: { ok: { type: 'boolean' }, reason: { type: 'string' }, touches_rust: { type: 'boolean' } }, required: ['ok'] },
      },
    )) || {};
  if (guard.ok !== true) {
    const reason = guard.reason || 'branch-guard produced no result';
    log(`verify: branch-guard failed (${reason}); no reviewers spawned`);
    return {
      verdict: 'ERROR',
      findings: [{ severity: 'blocker', claim: 'branch-guard pre-flight failed', evidence: reason, suggested_next_step: 'resolve the branch/issue/diff problem before re-verifying' }],
      pr_number: null,
    };
  }

  const stageARaw = await resilientAgent(
    `Independently review the diff on issue #${issue}'s branch \`${ctx.branch}\` against the ticket. ` +
      inWorktree() +
      `You are read-only; run the tests yourself in that worktree and trust no claim. Emit exactly your verdict JSON. ` +
      severityTaxonomy +
      (isFixRound
        ? ` This is a fix-round DELTA re-verify: the branch already cleared a full verify and has since had verify ` +
          `findings applied. Concentrate on the fix delta and confirm the applied findings are correctly resolved and ` +
          `introduced no regression. Still run the FULL gate battery yourself (a fix can break an untouched file); the ` +
          `domain-lens fan-out is limited to the lenses that gated, so cover everything else yourself.`
        : ``) +
      responseBlock,
    { agentType: 'change-verifier', phase: 'Verify', label: 'change-verifier', schema: verdictSchema },
  );
  const stageA =
    stageARaw && stageARaw.verdict
      ? stageARaw
      : { verdict: 'REJECT', findings: [{ severity: 'blocker', claim: 'change-verifier produced no usable result', evidence: 'agent returned null or degraded past its schema', suggested_next_step: 're-run the verifier' }] };
  log(`change-verifier verdict=${stageA.verdict} findings=${(stageA.findings || []).length}`);

  // On a fix round only the lenses that actually raised a gating finding re-run. A
  // generic verifier cannot judge whether a domain or craftsmanship finding was truly
  // resolved — the lens that raised it has to say so, or "fixed" means "the implementer
  // said so". Lenses that cleared the branch stay skipped; re-running them would just
  // re-review an unchanged diff.
  const experts = isFixRound ? expertsForZones(zones).filter((e) => gatingLenses.has(`lens:${e}`)) : expertsForZones(zones);
  const lensThunks = experts.map((expert) => () =>
    resilientAgent(
      `Read-only lens over the diff on issue #${issue}'s branch \`${ctx.branch}\`, from your domain's angle ` +
        `(zones: ${zones.join(', ')}). ` +
        inWorktree() +
        (isFixRound
          ? `You raised gating findings on this branch and they have since had fixes applied. Re-check YOUR findings ` +
            `specifically: is each one genuinely resolved, or was it papered over? A cosmetic edit that does not address ` +
            `the substance is NOT resolved — say so and keep the severity. Trust no claim; read the current code. `
          : ``) +
        `Do NOT edit. Find domain-specific correctness / invariant violations the mechanical gates cannot catch; cite ` +
        `file:line. Emit the verdict JSON shape. ${severityTaxonomy}` +
        responseBlock,
      { agentType: expert, phase: 'Verify', label: `lens:${expert}`, schema: verdictSchema },
    ).then((r) => (r ? Object.assign({ __lens: `lens:${expert}` }, r) : r)),
  );
  if (guard.touches_rust && (!isFixRound || gatingLenses.has('lens:rust-craftsmanship'))) {
    lensThunks.push(() =>
      resilientAgent(
        `Read-only senior-Rust craftsmanship review of the added/changed Rust on issue #${issue}'s branch ` +
          `\`${ctx.branch}\`. ` +
          inWorktree() +
          `Grade duplication (DRY), code smell, idiomatic Rust, ownership/allocation ergonomics, and API/type shape — the ` +
          `clean-code qualities the mechanical gates and the correctness verifier do not judge. Do NOT edit. Cite ` +
          `file:line and name the concrete fix. Emit the verdict JSON (lens "rust-craftsmanship"); put an overall grade ` +
          `in coverage_notes. ${severityTaxonomy}` +
          (isFixRound
            ? ` This is a re-check: you raised gating findings on this branch and fixes have since been applied. Verify ` +
              `each of YOUR findings is genuinely resolved rather than papered over — a cosmetic edit that leaves the ` +
              `substance intact is NOT resolved, so keep its severity and say why. Trust no claim; read the current code.`
            : ``) +
          responseBlock,
        { agentType: 'rust-craftsmanship-reviewer', phase: 'Verify', label: 'lens:rust-craftsmanship', schema: verdictSchema },
      ).then((r) => (r ? Object.assign({ __lens: 'lens:rust-craftsmanship' }, r) : r)),
    );
  }
  if (ctx.claims_e2e) {
    lensThunks.push(() =>
      resilientAgent(
        `The branch \`${ctx.branch}\` on issue #${issue} claims E2E evidence. Locate the referenced output artifacts and ` +
          `run the Phase-B audit against them (log gates all zero, read + describe every sampled PNG, PSNR vs ` +
          `thresholds). If the artifacts are absent the evidence is unverified — say so. Emit the verdict JSON shape. ` +
          `${severityTaxonomy}\n\nEvidence reported by the run: ${JSON.stringify(ctx.evidence)}`,
        { agentType: 'evidence-verifier', phase: 'Verify', label: 'evidence-verifier', schema: verdictSchema },
      ),
    );
  }
  const lensResults = lensThunks.length > 0 ? (await parallel(lensThunks)).filter(Boolean) : [];
  log(`lenses complete: ${lensResults.length} of ${lensThunks.length} returned`);

  const all = [stageA].concat(lensResults);
  const findings = [];
  for (const r of all) for (const f of (r && r.findings) || []) findings.push(f);

  const hasBlocker = findings.some((f) => f.severity === 'blocker');
  // A REJECT is FIX-worthy on its own — never trust that a rejecting reviewer
  // also remembered to tag a finding `blocker`, or emitted findings at all.
  const hasReject = all.some((r) => r && r.verdict === 'REJECT');
  const hasEscalate = all.some((r) => r && r.verdict === 'ESCALATE');
  const hasOwnerQuestion = findings.some((f) => f.severity === 'owner-question');
  // `should-fix` is a reviewer change request, so it gates the PR the same way a
  // blocker does. Only nits and observations survive as PR-body notes — a severity
  // that asserts "should be fixed" must never be satisfied by writing it down.
  // Everything actionable goes back to the implementer, nits included. The reviewers
  // and the implementer are one team self-reviewing until the branch is ready for a
  // human; a severity threshold that quietly routes findings to the PR body instead
  // spends the reviewer's work on the reader rather than on the code. `info` is the
  // only severity that does not force a round — the taxonomy defines it as no-action —
  // but it still travels in the review the implementer reads.
  const hasShouldFix = findings.some((f) => f.severity === 'should-fix');
  const hasLow = findings.some((f) => f.severity === 'low');
  const reviewItems = findings.filter((f) => f.severity === 'info');

  // Which lenses actually gated, so the next fix round re-runs exactly those. A lens
  // that cleared the branch has nothing to re-check; one that gated must confirm its
  // own finding was resolved rather than leaving that to a generic verifier.
  const gatingLensLabels = [];
  for (const r of all) {
    if (!r || !r.__lens) continue;
    const gated =
      r.verdict === 'REJECT' ||
      ((r.findings || []).some((f) => f.severity === 'blocker' || f.severity === 'should-fix'));
    if (gated) gatingLensLabels.push(r.__lens);
  }

  // The implementer reads the reviews themselves, not a severity-filtered digest —
  // `coverage_notes` carries the reasoning that makes a finding actionable.
  const reviews = all
    .filter(Boolean)
    .map((r) => ({ lens: r.__lens || r.lens || 'change-verifier', verdict: r.verdict, coverage_notes: r.coverage_notes || '', findings: r.findings || [] }));

  const report = { findings, reviews, pr_number: null, review_items: reviewItems, gating_lenses: gatingLensLabels };
  if (hasBlocker || hasReject || hasShouldFix || hasLow) return Object.assign({ verdict: 'FIX' }, report);
  if (hasEscalate || hasOwnerQuestion) return Object.assign({ verdict: 'DISCUSS' }, report);
  return Object.assign({ verdict: 'PASS' }, report);
}

async function runOpenPr(reviewItems) {
  const opened =
    (await resilientAgent(
      `All lenses cleared the branch \`${ctx.branch}\` on issue #${issue}. Open a pull request READY FOR REVIEW via gh ` +
        `(gh pr create --head ${ctx.branch}) — NOT a draft. A PASS means the branch is verified and ready for the owner ` +
        `to merge, so it must not sit in draft. NEVER merge, though — merging is the owner's call. Title the PR as a ` +
        `conventional commit (\`type(scope): summary\`); the repo squash-merges and release-please parses the title, so ` +
        `a mistitled PR silently skips the version bump. Fill the body with the ticket link, the change summary, the ` +
        `test evidence, and any E2E report. The team already self-reviewed this branch to completion, so the body must ` +
        `NOT carry unresolved defects for the owner to triage — every actionable finding was either fixed or declined ` +
        `with a reason a reviewer accepted. Add a "Review notes" section ONLY if this list is non-empty, listing these ` +
        `\`info\`-severity observations verbatim: ${JSON.stringify(reviewItems)}. ` +
        `Write that body to a file and pass \`gh pr create --body-file <path>\`. NEVER ` +
        `pass \`--body "@<path>"\`: gh does NOT expand \`@file\` for \`--body\` (that is a curl / \`gh api\` idiom), so ` +
        `it would be posted as literal text. Return the PR number.`,
      { phase: 'Verify', label: 'open-pr', model: 'sonnet', schema: { type: 'object', properties: { pr_number: { type: 'number' } }, required: ['pr_number'] } },
    )) || {};
  return opened.pr_number || null;
}

async function runFix(reviews, round) {
  phase('Fix');
  return await resilientAgent(
    `Your teammates have code-reviewed your branch \`${ctx.branch}\` (issue #${issue}). This is round ${round} of ` +
      `${MAX_FIX_ROUNDS} of the team's self-review before any human reads this PR. ` +
      inWorktree() +
      `Read each reviewer's FULL analysis below — the coverage notes carry the reasoning, not just the finding list. ` +
      `Respond to EVERY item, nits included: either fix it, or decline it with a concrete reason a reviewer would accept. ` +
      `A human's attention is the scarce resource here — anything the team can settle among itself should never reach ` +
      `them. Declining is legitimate when a reviewer is factually wrong or the change would be out of scope; "it's minor" ` +
      `is not a reason.\n\n` +
      `Stay inside the ticket: no scope creep, no unrelated auto-fixes. Hold the engine doctrine and the licensing / ` +
      `logging / naming conventions. Make checkpoint commits at logical boundaries, then PUSH with a normal ` +
      `fast-forward push (\`git push origin ${ctx.branch}\`) — this is a fix on top of the existing branch, NOT a ` +
      `rebase, so do NOT force-push.\n\n` +
      `Report every item you addressed in \`applied\`, and every item you declined in \`unresolved\` WITH its reason — ` +
      `the reviewers who raised them re-check your work next round and will re-raise anything papered over.\n\n` +
      `Reviews (JSON): ${JSON.stringify(reviews)}`,
    leadOpts({
      phase: 'Fix',
      label: `fix:${lead || 'generic'}`,
      schema: {
        type: 'object',
        properties: {
          applied: { type: 'array', items: { type: 'string' } },
          unresolved: { type: 'array', items: { type: 'string' } },
          commits: { type: 'array', items: { type: 'string' } },
          pushed: { type: 'boolean' },
        },
        required: ['applied', 'unresolved'],
      },
    }),
  );
}

// Rebase mode short-circuits composition entirely: a conflicting PR needs its
// branch replayed onto origin/main, not a fresh build.
if (mode === 'rebase') {
  phase('Fix');
  const rebased =
    (await resilientAgent(
      `Rebase branch \`${ctx.branch}\` (issue #${issue}) onto origin/main and resolve. ` +
        (ctx.worktree_path
          ? inWorktree()
          : `Check the branch out in a git worktree under ${repoRoot}/.claude/worktrees/ if one is not already live. `) +
        `Do NOT create a fresh feature branch and do NOT restart from Rederive. Run \`git fetch origin\`; rebase onto ` +
        `origin/main; resolve every conflict preserving the branch's intent; commit as needed; then force-push with ` +
        `lease (\`git push --force-with-lease\`). Return the absolute worktree_path, branch, resulting commits, and ` +
        `\`git diff origin/main --stat\` in diff_stat.`,
      leadOpts({ phase: 'Fix', label: `rebase:${lead || 'generic'}`, schema: implementSchema }),
    )) || {};
  return {
    outcome: 'rebased',
    stage: 'reverify',
    branch: rebased.branch || ctx.branch,
    worktree_path: rebased.worktree_path || ctx.worktree_path,
    commits: rebased.commits || [],
    diff_stat: rebased.diff_stat || '',
    owner_items: [],
  };
}

const RUNNERS = {
  [STAGE.CLAIM]: runClaim,
  [STAGE.REDERIVE]: runRederive,
  [STAGE.FAILING_TEST]: runFailingTest,
  [STAGE.IMPLEMENT]: runImplement,
  [STAGE.ABI_CONTRACT]: runAbiContract,
  [STAGE.POLYGLOT_PARITY]: runPolyglotParity,
  [STAGE.CROSS_COMPILE]: runCrossCompile,
  [STAGE.GATES]: runGates,
  [STAGE.BENCH]: runBench,
  [STAGE.SELF_REVIEW]: runSelfReview,
  [STAGE.EVIDENCE]: runEvidence,
};

let plannedStages = composeStages({ shape, zones, touches_apple: false, needs_bench: false });
log(`composed stages: ${plannedStages.join(' → ')}`);

const walked = [];
let verifyReport = null;

for (let i = 0; i < plannedStages.length; i += 1) {
  const stageId = plannedStages[i];
  if (stageId === STAGE.VERIFY) break;

  const result = await RUNNERS[stageId]();
  walked.push(stageId);

  if (stageId === STAGE.CLAIM && !ctx.worktree_path) {
    log('claim stage produced no worktree — aborting before any build work');
    return { outcome: 'claim-failed', stage: 'claimed', stages_run: walked, branch: ctx.branch, worktree_path: null, owner_items: [] };
  }

  // Rederive is the first stage that has actually read the tree, so recompute
  // the plan now that touches_apple / needs_bench are known. Additive only —
  // anything already walked stays walked.
  if (stageId === STAGE.REDERIVE) {
    const recomposed = composeStages({ shape, zones, touches_apple: ctx.touches_apple, needs_bench: ctx.needs_bench });
    const merged = walked.slice();
    for (const s of recomposed) if (merged.indexOf(s) === -1) merged.push(s);
    plannedStages = merged;
    log(`recomposed after rederive: ${plannedStages.join(' → ')}`);
  }

  if (stageId === STAGE.IMPLEMENT && !(result && result.branch)) {
    log('implement stage returned no branch — aborting before verify');
    return { outcome: 'implement-failed', stage: 'implement', stages_run: walked, branch: ctx.branch, worktree_path: ctx.worktree_path, owner_items: [] };
  }
}

verifyReport = await runVerify(false);
walked.push(STAGE.VERIFY);

let fixRounds = 0;
while (verifyReport && verifyReport.verdict === 'FIX' && fixRounds < MAX_FIX_ROUNDS) {
  fixRounds += 1;
  log(`#${issue}: verify FIX — applying findings, round ${fixRounds}/${MAX_FIX_ROUNDS}`);
  const fixed = await runFix(verifyReport.reviews || [], fixRounds);
  if (!fixed || fixed.degraded) {
    return {
      outcome: 'fix-failed',
      stage: 'fixing',
      stages_run: walked,
      fix_rounds: fixRounds,
      branch: ctx.branch,
      worktree_path: ctx.worktree_path,
      owner_items: [{ kind: 'escalation', issue, reason: `fix round ${fixRounds} produced no usable result` }],
    };
  }
  verifyReport = await runVerify(true, new Set(verifyReport.gating_lenses || []), fixed);
}

const verdict = (verifyReport && verifyReport.verdict) || 'ERROR';
let prNumber = null;
if (verdict === 'PASS') prNumber = await runOpenPr((verifyReport && verifyReport.review_items) || []);

const ownerItems = [];
if (verdict === 'DISCUSS') {
  ownerItems.push({
    kind: 'owner-question',
    issue,
    branch: ctx.branch,
    findings: (verifyReport.findings || []).filter((f) => f.severity === 'owner-question'),
  });
} else if (verdict === 'FIX') {
  ownerItems.push({ kind: 'escalation', issue, branch: ctx.branch, reason: `still FIX after ${MAX_FIX_ROUNDS} fix rounds` });
} else if (verdict === 'ERROR') {
  ownerItems.push({ kind: 'escalation', issue, branch: ctx.branch, reason: 'verify could not run — see findings' });
}

log(`#${issue} complete: verdict=${verdict} pr=${prNumber} fix_rounds=${fixRounds}`);

return {
  outcome: verdict === 'PASS' ? 'pr-open' : verdict === 'DISCUSS' ? 'owner-question' : 'escalate',
  stage: verdict === 'PASS' ? 'pr-open' : verdict === 'DISCUSS' ? 'parked' : 'escalated',
  verdict,
  pr_number: prNumber,
  branch: ctx.branch,
  worktree_path: ctx.worktree_path,
  stages_run: walked,
  stages_planned: plannedStages,
  fix_rounds: fixRounds,
  claim_comment_id: ctx.claim_comment_id,
  findings: (verifyReport && verifyReport.findings) || [],
  report: ctx.report,
  owner_items: ownerItems,
};
