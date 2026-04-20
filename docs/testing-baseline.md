# Workspace test baseline

Canonical command and exclusion list for streamlib's unit + integration
test suite. Use this when reporting test results for a PR so reviewers
can spot silent test-coverage loss.

**Do not use `cargo test -p streamlib` as the workspace baseline.** It
covers only the top-level `streamlib` crate and misses the bulk of the
suite — `vulkan-video` alone contributes more tests than every other
crate combined.

---

## Canonical command

```bash
cargo test --workspace \
    --exclude api-server-demo \
    --exclude camera-deno-subprocess \
    --exclude camera-python-subprocess \
    --exclude camera-rust-plugin \
    --exclude webrtc-cloudflare-stream
```

The command should print `test result: ok.` from every binary and from
every `Doc-tests` block, with **zero failures**. That — not any
particular total — is the pass bar.

---

## Why the exclusion list

Every excluded crate is an **example binary with zero tests**. The
exclusions exist so `cargo test --workspace` builds on systems without
`libssl-dev` / OpenSSL development headers installed — without them, the
build step fails and no tests run. No test coverage is lost by
excluding them.

| Excluded crate              | Reason                                            |
|-----------------------------|---------------------------------------------------|
| `api-server-demo`           | Pulls `openssl-sys` via `native-tls` / `reqwest`  |
| `camera-deno-subprocess`    | Example binary, no tests; TLS-adjacent deps       |
| `camera-python-subprocess`  | Example binary, no tests; TLS-adjacent deps       |
| `camera-rust-plugin`        | Example binary, no tests; TLS-adjacent deps       |
| `webrtc-cloudflare-stream`  | Pulls WebRTC + TLS deps; no tests                 |

If one of these crates later gains real tests, move it off the
exclusion list and install the system deps on the builder instead.

**Review the exclusion list when a new workspace member lands.** A new
example that silently drags `openssl-sys` will break this command on
fresh machines.

---

## Measuring totals

The totals drift as tests are added or removed, so there's no fixed
number to validate against. Capture the output summary for a PR with:

```bash
cargo test --workspace --exclude ... 2>&1 \
  | grep -E "^test result:" \
  | awk '{ for (i=1;i<=NF;i++) {
        if ($i=="passed;") p+=$(i-1);
        if ($i=="failed;") f+=$(i-1);
        if ($i=="ignored;") ign+=$(i-1); } }
      END { print "passed="p" failed="f" ignored="ign }'
```

To isolate a single crate while bisecting a regression:

```bash
cargo test -p <crate> --no-fail-fast
```

---

## Using this in a PR

Run the canonical command and quote the totals in the PR description:

```
cargo test --workspace --exclude ... → passed=N failed=0 ignored=M
```

Compare **against the last PR that ran this command on main**, not
against any number hardcoded here:

- **Passed went up** — point at the added tests in the diff.
- **Passed went down** — explain why (test deleted, `#[ignore]`d,
  moved). A silent drop is a blocker until justified.
- **Failed > 0** — hard block.

Reviewers: if the PR body doesn't include a totals line, ask for one
before signing off on "tests pass."

---

## CI gate (pending)

This command is intended to become a CI gate once the dependencies in
[#343](https://github.com/tato123/streamlib/issues/343) land (GPU
runner, validation-layer wiring, hermetic harness). The gate will:

1. Run the canonical command on every PR.
2. Parse the totals.
3. Block merge on any `failed > 0` or unexplained drop in `passed`.

Until then, treat this doc as the humans-enforced version of the same
gate.
