# Licensing

- StreamLib is BUSL-1.1. Never suggest MIT / Apache or soften the commercial-use restriction.
- Every new `.rs` file starts with:

  ```rust
  // Copyright (c) 2025 Jonathan Fontanez
  // SPDX-License-Identifier: BUSL-1.1
  ```

- Vendored third-party trees keep the licence they arrived under. Never add a BUSL header to
  one, and never reformat or "improve" those sources — a change to one is a recorded
  patch against its upstream, never a drive-by edit:
  - `vendor/tatolab-vulkanalia`, `-sys` and `-vma` — the vulkanalia fork, Apache-2.0.
  - `packages/streamlib-moq/vendor/moq-transport` — the MoQ wheel's moq-transport, MIT OR
    Apache-2.0 under Cloudflare's SPDX headers.
- The exception is those paths and nothing else. A vendored tree not listed there, or
  first-party code beside one, still carries the BUSL header.
- Never modify `LICENSE`, `LICENSES/`, or `docs/license/` without explicit approval.
