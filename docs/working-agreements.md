# Working agreements

Owner rulings on how sessions work, and practical lessons from the repo, the rig and CI, that help
agents across many issues and no other file records. `CLAUDE.md`, `.claude/rules/` and `docs/plan/`
outrank this file. If an entry contradicts one of them, or the tree shows it is stale, fix or delete
the entry in the same PR.

Where a fact goes: a bug is a bug issue; a ruling or fact that only matters to one issue is a
comment on that issue; a fact that helps agents across issues goes here or in the doc that owns its
topic; `CLAUDE.md` holds only what every session must load.

## Deciding and asking

- **A DECIDED entry is today's direction, not a wall.** When the owner floats an architecture idea,
  judge it on its merits first, then name the plan entry or ADR it would amend and point at
  `/align`. Never answer "the ADR says X, so no" (owner, 2026-10-14).
- **An option that breaks a DECIDED entry is not an option.** Present it as a sequencing gap that
  names the step that must come first. Check every noun against `docs/plan/GLOSSARY.md` before
  presenting, because loose wording makes a design look like a reversal (owner, 2026-10-01).
- **Choosing a pattern is not architecture.** Picking the implementation pattern that meets a
  ticket's goal is `/implement` work, even when it is substantial. Bring it to the owner only if it
  changes what the product does or promises (owner, 2026-08-13).
- **Desk research before spikes.** Behaviour an OS, language, spec or shipped prior art already
  defines is answered in-session by research subagents, never by a spike or research ticket.
  Spikes are for what only streamlib's own combination can reveal (owner, 2026-10-01).
- **Delete residue before asking.** Before an align round, drop any undecided item that later
  DECIDED entries make impossible or redundant. Ask only forks that still exist (owner, 2026-10-02).

## Design posture

- **Never redesign an approved API mid-implementation.** If the approved shape looks wrong while
  building, stop and bring the divergence to the owner before writing it. A note in the PR is not
  a substitute. A parity ticket inventories the old surface first (owner, 2026-08-03).
- **A ticket's convenience spelling loses to the naming rule.** When a ticket's API bullet is
  shorter than its own naming bullet or the layer below, the explicit spelling wins. Raise it once,
  then record the ruling in the ticket body (owner, 2026-09-03).
- **Question the shape before adding an opt-in.** When a design shows a gap, first ask whether the
  shape that made the gap is right. Never cite the audio-window contract as precedent for another
  payload-reading carve-out (owner, 2026-10-16).
- **No transitional scaffolding.** Never recommend a bridge, shim or throwaway launcher when the end
  shape is reachable, because agents build on whatever the tree holds. Each slice deletes what it
  replaces in the same PR (owner, 2026-10-02).
- **Recommend only the production-final design.** Never offer a defer-while-nobody-uses-it option.
  "No users yet" licenses a clean break without shims, never shipping a contract known to be wrong
  (owner, 2026-10-02).
- **A capability serves any processor.** Never scope a served shape, schema or catalog entry to the
  built-ins. Design how a processor an agent wrote at runtime gets it, then have the built-ins use
  that path (owner, 2026-09-10).
- **Python can build everything Rust can.** Codecs, capture and kernels run natively and Python
  drives them. A capability missing from Python gets an owner-visible disposition the day it is
  found: built, ticketed, or named native-only (owner, 2026-08-03).
- **Product features are the commercial product's.** AR, web control panels, dashboards,
  marketplaces, agent-to-agent coordination and simulation do not belong in this repo. When the
  line is unclear, ask; never assert it (owner, 2026-09-01).
- **Protocols use their official library.** Implement a protocol with its maintained library at the
  latest version, never hand-rolled from the spec, and delete leftover hand-rolled code in the same
  change (owner, 2026-10-06).
- **Upgrades take the new version's way.** Adopt upstream's changed mechanism and retire our old
  workaround. A test the upgrade breaks is fixed upstream's way in the same PR (owner, 2026-09-29).
- **The wheel's abi3-py310 floor is held partly for NVIDIA Isaac Sim** (owner, 2026-08-03). Weigh
  that before proposing a higher Python floor.
- **The control plane serving config verbatim is intentional.** `graph` returns every processor's
  config, secret-bearing keys included, inside a trusted boundary. Do not file it or redact a key to
  work around it (owner, 2026-09-05).

## Tickets and PRs

- **A ready PR is a merge contract.** It never carries open questions or `[NEEDS DECISION]`.
  Settle each fork in chat first, then open it (owner, 2026-10-02).
- **No draft PRs for review.** Before asking for review, run an independent adversarial review,
  fold its findings, run the gates, wait for CI and CodeRabbit and answer them — a record is
  fixed, a design finding is routed with `Owner decision needed:` and never folded — then mark
  it ready (owner, 2026-10-02 and 2026-10-17).
- **When CodeRabbit skips a large PR**, two read-only reviewers with different lenses rank the
  issues, each finding gets a PR comment with its action, high and major ones are fixed unless
  they are design, which route to the owner, and the PR squash-merges unless one needs the owner
  (owner, 2026-10-05 and 2026-10-17).
- **An `/implement` PR closes its ticket.** Do the work that stands between the fix and the exit
  criteria in the session, say what cannot be proven, and still close. Leave it open only when the
  owner scopes the PR as partial (owner, 2026-09-13).
- **A scope note does not shield a bug.** "Do not touch X" forbids redesigning X, not fixing a
  defect the ticket's own fix also cures. Fix it and say so in the PR body (owner, 2026-09-06).
- **Big tickets carry the ultracode marker.** At `/derive-tickets`, split where a clean seam exists
  and open every ticket still past one session with `**Run with ultracode**`. When the owner talks
  about ultracode rather than invoking it, answer instead of launching a workflow (owner, 2026-10-02).
- **A redirected ticket is rewritten, not struck.** State what we want, what we do today, why we
  are moving, and the constraints. Keep everything an issue comment established (owner, 2026-08-25).
- **No follow-on ticket without a plan.** A deferred follow-on is filed only with its design, cost,
  vehicle and exit criteria (owner, 2026-09-05).
- **Read the comments.** Owner rulings land in issue comments after filing, so every tracker sweep
  reads `gh issue view <n> --comments`. A ticket too large for one session hands off through a
  pushed branch and a structured comment, never a handoff file.

## Testing, CI and verification

- **PR CI is moving to tests only, on one cached build per platform** (owner, 2026-10-15). Never
  add a new PR job that compiles the engine itself; put a new test in an existing test job.
- **A flaky test is a design defect.** Rerunning to unblock is fine; then reproduce it, find the
  nondeterminism and fix it in its own PR with before and after counts (owner, 2026-10-16).
- **A user-facing Done-means needs a real-stream proof.** Rust tests with in-process test nodes do
  not exercise venvs, helper processes or shutdown. Prove it with real scaffolded streams through a
  temporary harness, with literal output in PR comments (owner, 2026-10-09).
- **Delete a test whose claim is already proven elsewhere.** When a change orphans a test's
  subject, relocate it only if its claim would otherwise go unproven (owner, 2026-08-30).
- **Verify the machine, don't guess.** Check devices, loaded modules and what a wheel exposes
  before saying what something needs, and label anything untested (owner, 2026-09-06).
- **No acceptance depends on hardware the rig lacks.** Propose a substitute from what is attached
  and say what it does not prove (owner, 2026-10-05).
- **Round-tripping is not conformance.** A format writer is tested against an independent parser:
  a writer whose tests parse with the writing crate can omit a required box for its whole life.
  A per-item self-consistency check cannot see one item replayed forever.
- **Never `--all-features`.** It turns on the vendored VMA's `bind` feature and bindgen overwrites
  `vendor/tatolab-vulkanalia-vma/src/vma.rs`. Fix with `git checkout --` on that file, and tell any
  spawned agent that runs cargo.
- **Run the engine lib suite under `timeout`.** An aborted run prints no summary and a wedged run
  never returns. A/B against main's binary, and copy the binary to `/tmp` before a long loop.
- **A per-frame proof runs under `ulimit -n 1024`.** Sessions run with a huge fd limit while the
  owner's terminals run at 1024, so a per-frame fd leak is invisible in a session. Run at least
  60 s and sample `/proc/<pid>/fd`.
- **Throughput comparisons need a quiet machine.** A concurrent cargo build or gate battery invalidates
  frame counts and makes camera fixtures lose every claim race. Re-run quiet and A/B against main.
- **`check-no-in-process-placement` keys exempt lines by path and exact text.** Archiving a change
  file or rewrapping a plan paragraph breaks it. Re-point or rewrap in the same PR; never weaken it.
- **macOS CI logs truncate.** Fetch the full zip with
  `gh api repos/tatolab/streamlib/actions/runs/<id>/logs` and grep the macOS job's file.
- **A red only on the runner can be the toolchain.** Ubuntu's apt `glslc` corrupts non-ASCII bytes
  in `-g` debug strings, which the rig's newer shaderc does not.

## Working in the repo

- **Branch work goes in a worktree.** Several sessions share the main checkout, so never run
  `git checkout`, `switch`, `stash` or `reset` there. Every repo file a session writes goes in a
  worktree on its own branch (owner, 2026-09-27).
- **Subagents share the parent's checkout.** After each one returns, check the branch and
  spot-check integrated files. A worktree-isolated agent must be told to write only inside its
  worktree.
- **Diff reviews against `origin/main`.** Local `main` goes stale after squash-merges, so reviewers
  flag other people's merged work.
- **Guard `--body-file` edits.** An empty fetch once blanked an issue. Fetch from inside the repo
  and gate every edit on `test -s` in the same `&&` chain.
- **Auto mode refuses opaque bulk scripts.** Have a script emit payload files, then run visible
  `gh` loops with the literal `repos/tatolab/streamlib` path.
- **uv venv files are hardlinks into the uv cache.** Break the link before probing a venv file, or
  the edit reappears in every later venv.
- **Rig fixtures exec `$REPO_ROOT/target`.** In a worktree, symlink `target/`, unset
  `CARGO_TARGET_DIR`, and remove the symlink afterwards.
- **An example's venv runs the published wheel its lockfile pins**, so a branch fix is invisible
  there until a release. Print the module's `__file__` before blaming a branch.
- **Reasoning effort defaults to medium** for sessions, agents and workflow agents. Use high only
  for a hard problem; never set xhigh or max in an automated path (owner, 2026-09-28).
- **`.claude` skill and agent edits are small tweaks**, each reviewable alone, never a broad
  rewrite (owner, 2026-09-28).

## The rig

- **Torch must be the CUDA build.** With `+cpu` torch the GPU tests skip and still look green.
  Check `torch.__version__` first; fix with `uv pip install --reinstall-package torch torch`
  (owner, 2026-09-27). The `tests/stream-on-runtime` venv gets it from `uv sync --group rig`.
- **vivid runs at 30 fps.** It is not loaded after a reboot (`sudo -n /usr/sbin/modprobe vivid`),
  and its frame interval is device-wide state other apps leave at 1 fps, which ends capture. Check
  with `v4l2-ctl -d /dev/video0 --get-parm` and set `--set-parm=30` (owner, 2026-10-05).
- **The X display is `:1`.** `:0` vanishes when the owner locks the session.
- **Lavapipe first needs both selectors off.** Set `VK_LOADER_DISABLE_SELECT=1 NODEVICE_SELECT=1`
  and list the ICD files in reverse, or a "lavapipe first" test passes for the wrong reason. To
  simulate the CI lane, also stub `systemctl`, because `tatolabd` copies the display from the user
  manager.
- **Check `nvidia-smi` before a GPU run.** An unattended NVIDIA upgrade without a reboot leaves
  llvmpipe as the only device.
- **A camera stream without a camera** runs `TestPatternSource` into `VirtualCameraSink` on
  v4l2loopback, with `STREAMLIB_CAMERA_DEVICE` pointing at it.
- **`pkill -f` matches the calling shell's own command line.** Stop processes by pid.

## Talking with the owner

- **One topic at a time, in plain language.** Say what it is, why it matters, and what must be
  decided, with the question first and mechanics only if asked (owner, 2026-09-02).
- **Reports go in a Claude doc**, never files on disk the owner cannot see remotely (owner,
  2026-09-28).
- **Research asked in passing stays in chat or a Claude doc.** A `docs/research/` memo and PR are
  for research feeding a live `/align` or `/propose-change` (owner, 2026-10-02).
- **No cost caveats in idea or dogfooding discussions.** Describe a dependency by what it needs
  (owner, 2026-09-06).
- **Never propose upstream patches to cloudflare/moq-rs** (owner, 2026-09-06). Ask before offering
  one to the moq-dev crates; it is outward-facing work under the owner's name.

## Raised by the owner, not decided

Bring these to the align they touch. None is a decision.

- **Built-ins should follow the users' path** (2026-10-06). Native Rust built-ins while users write
  Python nodes is a smell; the sketch is Python built-ins reaching their Rust over FFI, as extension
  wheels do. The vsync and audio-callback deadlines are the tension.
- **Windows is a third floor** (2026-09-26): Windows 10 and 11, Vulkan only, floor build 1809, after
  Linux and macOS are finished. Shape new platform seams three-way.

## External references

- **Secrets** live in the Infisical project `Tatolab` (`dev` and `prod`; `prod` syncs to GitHub).
  Run with `infisical run --env prod -- <cmd>` and never echo a value.
- **Tatolab.app** is the private repo `tatolab/tatolab-app`, a client of the runtime's local API.
