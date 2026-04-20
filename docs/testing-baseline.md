# Workspace test baseline

Canonical command, exclusion list, and expected per-crate test counts for
streamlib's unit + integration test suite. Use this when reporting test
results for a PR so reviewers can spot silent test-coverage loss.

**Do not use `cargo test -p streamlib` as the workspace baseline.** It
covers only the top-level `streamlib` crate (~207 tests) and misses the
bulk of the suite — `vulkan-video` alone contributes more tests than
every other crate combined.

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

Expected result on a clean main (Linux, libssl-dev installed):

```
passed=848  failed=0  ignored=21
```

Split across binary tests (844 / 8) and doc tests (4 / 13) — the exact
split depends on test scheduling, but the **total should not drift
downward** between runs on the same commit unless a test was deleted
or gated behind `#[ignore]`.

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
exclusion list and vendor/install the system deps instead.

**Review the exclusion list when a new workspace member lands.** A new
example that silently drags `openssl-sys` will break this command on
fresh machines.

---

## Expected per-crate test counts

Measured against `main` on Linux. Counts are **upper bounds under normal
conditions** — drivers, race conditions, and `#[ignore]` gates can shift
a handful of tests. A drop of more than ~5 tests in any crate without an
obvious explanation in the PR is a red flag.

| Crate                      | passed | ignored | notes                                              |
|----------------------------|-------:|--------:|----------------------------------------------------|
| `vulkan-video`             |    617 |      11 | RHI, session/DPB, rate control, NV12, validator    |
| `streamlib`                |    207 |       7 | lib + integration + binary targets                 |
| `streamlib-codegen-shared` |     12 |       0 |                                                    |
| `streamlib-macros`         |      7 |       1 | derive macros + compile-tests                      |
| `streamlib-broker`         |      4 |       0 |                                                    |
| `streamlib-plugin-abi`     |      0 |       2 | only doctests, currently all ignored               |
| All other crates           |      0 |       0 | binaries / CLIs with no test targets               |

**How to measure a single crate** (useful when bisecting a drop):

```bash
cargo test -p <crate> --no-fail-fast 2>&1 \
  | grep -E "^test result:" \
  | awk '{ for (i=1;i<=NF;i++) { \
        if ($i=="passed;") p+=$(i-1); \
        if ($i=="failed;") f+=$(i-1); \
        if ($i=="ignored;") ign+=$(i-1); } } \
      END { print "passed="p" failed="f" ignored="ign }'
```

---

## Using this in a PR

Run the canonical command and quote the totals in the PR description:

```
cargo test --workspace --exclude ... → passed=XXX failed=0 ignored=XX
```

If totals changed vs. this doc:

- **Passed count went up** — point at the added tests in the diff.
- **Passed count went down** — explain why (test deleted, `#[ignore]`d,
  moved to another crate). If a test was removed silently, the PR
  should be blocked until it's restored or the removal is justified.
- **Failed count > 0** — hard block.

Update the counts in this doc when the expected baseline genuinely
shifts (e.g., a feature-branch adds a crate full of new tests that
merged to `main`).

---

## Known flakes

- `streamlib::core::utils::loop_control::tests::test_shutdown_event_exits_loop`
  — occasionally times out under `cargo test -p streamlib` when the
  iceoryx2 node is contended by other tests in parallel. Passes
  reliably in isolation and in `cargo test --workspace` ordering.
  Tracked in #361's follow-up space.

If you hit a failure on `main`, re-run the single affected test in
isolation with `cargo test -p <crate> <test_name>` before assuming the
suite is broken.

---

## CI gate (pending)

This command is intended to become a CI gate once the dependencies in
[#343](https://github.com/tato123/streamlib/issues/343) land (GPU runner,
validation-layer wiring, hermetic harness). The gate will:

1. Run the canonical command on every PR.
2. Parse the totals.
3. Block merge on any `failed > 0` or unexplained drop in `passed`.

Until then, treat this doc as the humans-enforced version of the same
gate.
