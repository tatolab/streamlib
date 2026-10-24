---
name: attach-artifact
description: Put evidence files — decoded frames, spectrograms, graph snapshots, logs, screen recordings — at a public URL on gh-artifact.tatolab.com (the R2 bucket github-artifacts) and get the markdown that shows each one in a PR, an issue or a chat, without committing binaries. Use when verify-live, verify-audio or capture-stream-evidence produce a file to attach, when someone on a phone or remote session needs to open a file that lives on this machine, or to delete an attachment once it has served. Wraps `.claude/scripts/attach-artifact` (Infisical + wrangler).
---

# attach-artifact

Evidence belongs where reviewers read: a PR body, an issue comment. GitHub renders an image only from a URL it can fetch, and a binary committed to the repository stays in its history for good. So an artifact goes to the R2 bucket `github-artifacts`, served publicly at `https://gh-artifact.tatolab.com/<key>`, and the PR carries the link.

## Before you attach

- **It is public.** Anyone with the URL can open it, and a CDN may keep it after a delete. Never attach a secret, a token, a signing identity, a `.infisical.json`, or a home path that names someone other than the owner. Read every image you attach first.
- **The checkout is linked to Infisical.** The script reads the Cloudflare token from the Infisical project `Tatolab` (env `prod`) through `infisical run`, so no value ever reaches the shell or the output. A checkout without an untracked `.infisical.json` is refused with the fix: `infisical init` and choose `Tatolab`.

## Attach

```bash
.claude/scripts/attach-artifact <repo>/<pr-N|issue-N>/<topic> <file>...
```

- The key prefix says where the evidence belongs: `streamlib/pr-2920/verify-live`, `streamlib/issue-2919/spectrogram`, `tatolab-app/pr-57/decision-videos`. Lowercase letters, digits, `.`, `_` and `-`.
- Each file lands at `<prefix>/<its file name>`, typed by its extension (PNG, JPEG, GIF, WebP, MP4, WebM, MOV, WAV, MP3, PDF, JSON, and logs/text as UTF-8 text). Name files with letters, digits, `.`, `_` and `-` so the URL needs no escaping.
- HTML, SVG and JavaScript are refused: they would run as a page on tatolab.com. Attach a screenshot, or rename the source `.txt`.
- The script reports a file only once its public URL serves every byte. For each one it prints the URL, then the markdown: an image inline (`![frame.png](…)`), anything else as a link.

Paste the markdown into the PR body or the issue comment beside the description of what it shows (`verify-live`'s report, `verify-audio`'s outcome). A video is a link: GitHub plays inline only its own uploads, but the link plays in any browser, a phone included.

## Remove

```bash
.claude/scripts/attach-artifact --remove <key>...
```

Deletes each object and checks its URL answers 404. Remove attachments that showed anything you would not leave public, and scratch uploads once they have served. `--url <key>` prints a key's public URL without touching the bucket.

## When the script refuses

- **`not linked to Infisical`**: run `infisical init` in the checkout and choose `Tatolab`. The `.infisical.json` it writes stays untracked.
- **`wrangler refused`**: rerun the printed step by hand to see wrangler's words. An expired or rotated `CLOUDFLARE_DEPLOY_API_TOKEN` is fixed in Infisical, never in the shell.
- **`serves N bytes, not the M`**: the upload did not land whole, or the CDN answered early. Run it again; a second failure is worth reporting with both numbers.

The script's own test: `bash .claude/scripts/tests/attach-artifact.test.sh` (stubs on PATH; no network, no token).
