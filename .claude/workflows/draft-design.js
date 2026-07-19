// draft-design workflow — architecture-first recon + a design brief for the
// owner. Read-only: it lands no code, only a brief posted to the issue.

export const meta = {
  name: 'draft-design',
  description:
    'Recon the current code with the domain experts whose zones the issue touches, then merge into a design brief (mermaid + alternatives + risk + decisions-for-owner) and post it to the issue. No code changes.',
  phases: [
    {
      title: 'Recon',
      detail:
        'Parallel read-only recon by each domain expert whose zone the issue touches — what core system already covers the concern, what would change, contract invariants, known failure modes; cite file:line.',
    },
    {
      title: 'Brief',
      detail: 'One opus synthesizer merges the recon into a design brief and posts it to the issue via gh.',
    },
  ],
};

// args arrives as { issue, zones?, ... } — object or JSON string.
const input = typeof args === 'string' ? JSON.parse(args) : args || {};
const issue = input.issue;
const zones = Array.isArray(input.zones) ? input.zones : [];

// Map the issue's zones to the domain experts to consult. Returns the expert
// agentType names; empty means "generic reasoning only".
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

const reconSchema = {
  type: 'object',
  properties: {
    existing_system: { type: 'string' },
    would_change: { type: 'array', items: { type: 'string' } },
    invariants: { type: 'array', items: { type: 'string' } },
    failure_modes: { type: 'array', items: { type: 'string' } },
    evidence: { type: 'array', items: { type: 'string' } },
  },
  required: ['existing_system', 'would_change', 'invariants'],
};

const briefSchema = {
  type: 'object',
  properties: {
    decisions_for_owner: { type: 'array', items: { type: 'string' } },
    diagram_included: { type: 'boolean' },
    risk_class: { type: 'string' },
    posted: { type: 'boolean' },
  },
  required: ['decisions_for_owner', 'diagram_included', 'risk_class'],
};

// K3: degrade-not-crash — see implement-ticket.js. On a null/failed structured
// result, retry ONCE schema-free and parse; then continue degraded, not dead.
async function resilientAgent(prompt, opts) {
  const options = opts || {};
  const { schema, ...schemaFree } = options;
  let first;
  try {
    first = await agent(prompt, opts);
  } catch (structuredThrow) {
    // agent({schema}) throws when it exhausts the StructuredOutput retry cap;
    // degrade to the schema-free retry below instead of killing the whole run.
    log(`resilientAgent: structured attempt threw (${options.label || 'unlabeled'}); falling back to schema-free retry`);
    first = null;
  }
  if (first) return first;
  const shape = schema ? JSON.stringify(schema) : '{}';
  let retry;
  try {
    retry = await agent(
      `${prompt}\n\nReturn ONLY a single JSON object matching this shape — no prose, no code fence: ${shape}`,
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

const experts = expertsForZones(zones);

phase('Recon');
let reconResults;
if (experts.length === 0) {
  log(`no zone-matched experts for issue #${issue}; generic recon only`);
  const generic = await resilientAgent(
    `Read-only architecture recon for issue #${issue}. Do NOT edit any file. ` +
      `Prove whether a core system already covers this concern before any new abstraction is proposed. ` +
      `Re-derive from the current tree and cite file:line for every claim.`,
    { phase: 'Recon', label: 'recon:generic', model: 'opus', schema: reconSchema },
  );
  reconResults = [generic].filter(Boolean);
} else {
  reconResults = (
    await parallel(
      experts.map((expert) => () =>
        resilientAgent(
          `Read-only recon for issue #${issue}, zone(s): ${zones.join(', ') || 'unspecified'}. ` +
            `Do NOT edit any file. Answer for the design: which existing core system already covers this concern, ` +
            `what would have to change, the contract invariants that bound it, and the known failure modes. ` +
            `Read your symptom index first, re-derive from the tree, and cite file:line for every claim.`,
          { agentType: expert, phase: 'Recon', label: `recon:${expert}`, schema: reconSchema },
        ),
      ),
    )
  ).filter(Boolean);
}
log(`recon complete: ${reconResults.length} of ${experts.length || 1} expert(s) returned`);

phase('Brief');
const brief =
  (await resilientAgent(
    `Merge the recon below into a DESIGN BRIEF for the repo owner on issue #${issue}, then post it as an ` +
      `issue comment via gh. The brief must contain: What & why; a mermaid diagram of the proposed shape; ` +
      `Alternatives considered (each with a one-line why/why-not, including "extend the existing system" vs ` +
      `"new abstraction"); Decisions taken (with evidence); a Risk class; and an explicit numbered ` +
      `DECISIONS FOR OWNER list (only the calls the owner must make). Keep implementation mechanics OUT — no ` +
      `file-by-file plan, no test names. Nothing builds until he approves in a comment.\n\n` +
      `Recon findings (JSON): ${JSON.stringify(reconResults)}`,
    { phase: 'Brief', label: 'brief', model: 'opus', schema: briefSchema },
  )) || {};
log(`brief posted=${brief.posted === true} decisions=${(brief.decisions_for_owner || []).length}`);

return {
  decisions_for_owner: brief.decisions_for_owner || [],
  diagram_included: brief.diagram_included === true,
  risk_class: brief.risk_class || '',
  experts_consulted: experts,
};
