# package-source-expert — symptom index

Knowledge lives in `docs/`; this file is only routing. Update in the same PR that adds a learning (see `.claude/rules/docs-policy.md`).

Match your symptom or concern, read the doc, then verify its claims against current code — a doc is the best-known state when it was written, not ground truth. Most packaging knowledge lives in the `docs/architecture/` package-source + schema docs (arch docs describe shipped state); the learnings dir carries only the cross-build soundness trap.

| symptom / concern | read |
|---|---|
| A GPU package works in-process / as a workspace plugin but corrupts the driver when shipped as a separately-built source-only `.slpkg` — the load-time build divergence, and why package GPU code must use the cdylib-safe FullAccess primitives, not the raw device | `docs/learnings/slpkg-raw-device-rhi-construction.md` |
| A `-dev.N` publisher's schema JTDs are silently unfetchable — the catalog-keyed-by-FULL-version vs schemas-keyed-by-RELEASE-CORE-version asymmetry, and the version model | `docs/architecture/schema-identity-and-packaging.md`, `docs/architecture/package-development-model.md` |
| A consumer observes a half-written package-source tree, or you're deciding release-write ordering — atomic release (manifest written last) + staged rename/exchange swap; a package source is a static `.slpkg` file tree, no central/hosted registry | `docs/architecture/package-source.md` |
| Deciding by-version vs path/patch resolution, the dev loop (link vs `-dev.N`), or the on-disk staging layout of a built package | `docs/architecture/package-development-model.md`, `docs/architecture/package-staging-layout.md` |
