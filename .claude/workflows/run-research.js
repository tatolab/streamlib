// run-research workflow — investigate an open question from several deliberately
// different angles, then synthesize. No code changes; the deliverable is a report
// posted to the issue (and, usually, the follow-up issues it seeds).

export const meta = {
  name: 'run-research',
  description:
    'Fan out parallel investigators — a mix of zone-matched domain experts and generic reasoners — each on a DIFFERENT angle, never sharing the issue’s own pre-baked recommendation; then one synthesizer merges them into a report posted to the issue. Read-only.',
  phases: [
    { title: 'Investigate', detail: 'Parallel read-only investigators, each assigned a distinct angle so their conclusions are independent.' },
    { title: 'Synthesize', detail: 'One synthesizer merges the angles into a recommendation + open-questions report and posts it to the issue.' },
  ],
};

// args: { issue, zones?, ... } — object or JSON string.
const input = typeof args === 'string' ? JSON.parse(args) : args || {};
const issue = input.issue;
const zones = Array.isArray(input.zones) ? input.zones : [];

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

// Four distinct angles. Zone-matched experts take the two streamlib-internal
// angles; generic opus reasoners take the two outward-looking angles. Assigning
// one angle per investigator is what keeps their conclusions independent.
const angles = [
  { angle: 'prior-art: how comparable production systems solve this, with sources', agentType: null },
  { angle: 'constraints: what in the current streamlib architecture bounds or blocks each option (cite file:line)', agentType: experts[0] || null },
  { angle: 'cost: the simplest viable shape and what it would take to build', agentType: null },
  { angle: 'risk: the failure modes of each option and what a wrong choice commits us to', agentType: experts[1] || null },
];

const investigateSchema = {
  type: 'object',
  properties: {
    angle: { type: 'string' },
    findings: { type: 'array', items: { type: 'string' } },
    leaning: { type: 'string' },
    evidence: { type: 'array', items: { type: 'string' } },
  },
  required: ['angle', 'findings'],
};

const synthesisSchema = {
  type: 'object',
  properties: {
    report_posted: { type: 'boolean' },
    recommendation: { type: 'string' },
    open_questions: { type: 'array', items: { type: 'string' } },
    followup_candidates: { type: 'array', items: { type: 'string' } },
  },
  required: ['report_posted', 'recommendation', 'open_questions'],
};

phase('Investigate');
const investigations = (
  await parallel(
    angles.map((a) => () => {
      const opts = { phase: 'Investigate', label: `angle:${a.angle.split(':')[0]}`, schema: investigateSchema };
      if (a.agentType) opts.agentType = a.agentType;
      else opts.model = 'opus';
      return resilientAgent(
        `Investigate issue #${issue} from ONE angle only — ${a.angle}. Do NOT read or repeat any recommendation the ` +
          `issue body may already contain; form your own view from this angle alone, so your conclusion is independent. ` +
          `Read-only, no code changes. Cite sources / file:line. Report your findings and which way this angle leans.`,
        opts,
      );
    }),
  )
).filter(Boolean);
log(`investigation complete: ${investigations.length} of ${angles.length} angles returned`);

phase('Synthesize');
const synthesis =
  (await resilientAgent(
    `Synthesize the independent angle investigations below into a research report for issue #${issue}, and post it as an ` +
      `issue comment via gh. The report states each option with its pros/cons/evidence, then an unambiguous recommendation ` +
      `(or, if there's no clear winner, says so and lists the question that would break the tie), and an open-questions list ` +
      `for the owner. If the research seeds concrete follow-up work, list follow-up candidates (do not file them). No code.\n\n` +
      `Angle findings (JSON): ${JSON.stringify(investigations)}`,
    { phase: 'Synthesize', label: 'synthesize', model: 'opus', schema: synthesisSchema },
  )) || {};
log(`synthesis posted=${synthesis.report_posted === true}`);

return {
  report_posted: synthesis.report_posted === true,
  recommendation: synthesis.recommendation || '',
  open_questions: synthesis.open_questions || [],
};
