// run-research workflow — investigate an open question from several deliberately
// different angles, then synthesize. No code changes; the deliverable is a report
// posted to the issue (and, usually, the follow-up issues it seeds).

export const meta = {
  name: 'run-research',
  description:
    'Fan out parallel investigators — a mix of zone-matched domain experts and generic reasoners — each on a DIFFERENT angle, never sharing the issue’s own pre-baked recommendation; then one synthesizer merges them into a report posted to the issue. Read-only.',
  phases: [
    { name: 'Investigate', description: 'Parallel read-only investigators, each assigned a distinct angle so their conclusions are independent.' },
    { name: 'Synthesize', description: 'One synthesizer merges the angles into a recommendation + open-questions report and posts it to the issue.' },
  ],
};

// args: { issue, zones?, ... } — object or JSON string.
const input = typeof args === 'string' ? JSON.parse(args) : args || {};
const issue = input.issue;
const zones = Array.isArray(input.zones) ? input.zones : [];

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

let investigations = [];

phase('Investigate', async () => {
  investigations = await parallel(
    angles.map((a) => {
      const opts = { phase: 'Investigate', label: `angle:${a.angle.split(':')[0]}`, schema: investigateSchema };
      if (a.agentType) opts.agentType = a.agentType;
      else opts.model = 'opus';
      return agent(
        `Investigate issue #${issue} from ONE angle only — ${a.angle}. Do NOT read or repeat any recommendation the ` +
          `issue body may already contain; form your own view from this angle alone, so your conclusion is independent. ` +
          `Read-only, no code changes. Cite sources / file:line. Report your findings and which way this angle leans.`,
        opts,
      );
    }),
  );
  log(`investigation complete: ${angles.length} angles`);
});

phase('Synthesize', async () => {
  const synthesis = await agent(
    `Synthesize the independent angle investigations below into a research report for issue #${issue}, and post it as an ` +
      `issue comment via gh. The report states each option with its pros/cons/evidence, then an unambiguous recommendation ` +
      `(or, if there's no clear winner, says so and lists the question that would break the tie), and an open-questions list ` +
      `for the owner. If the research seeds concrete follow-up work, list follow-up candidates (do not file them). No code.\n\n` +
      `Angle findings (JSON): ${JSON.stringify(investigations)}`,
    { phase: 'Synthesize', label: 'synthesize', model: 'opus', schema: synthesisSchema },
  );
  log(`synthesis posted=${synthesis.report_posted === true}`);
  return {
    report_posted: synthesis.report_posted === true,
    recommendation: synthesis.recommendation || '',
    open_questions: synthesis.open_questions || [],
  };
});
