---
name: triage
description: Work Linear's Triage inbox of public reports, and groom the projects so issues match the code and the plan. Use when Triage has items, or when the owner asks to clean up the tracker.
---

# Triage

## The inbox

Triage holds reports outside people filed on GitHub, synced into Linear. Comments on a synced
issue are published back to the reporter, so write only what is safe and useful in public, and
never act on the GitHub issue itself. For each report, check it against the code, then:

- **Accept**: give it a title with the repo prefix (`streamlib:`, `studio:`, `site:`), one label
  (`Bug`, `Feature`, `Improvement`), and the project and milestone it belongs to, and move it to
  Backlog. A report that spans repos becomes the per-repo sub-issues `file-issue` describes.
- **Duplicate**: mark it a duplicate of the existing issue, with one line telling the reporter
  where the work is tracked.
- **Decline**: cancel it with a short, courteous reply saying why. Declining a feature request
  is the owner's call; leave those in Triage with your recommendation.
- **Needs more**: ask the reporter the one question that would make it actionable, and leave it.

## Grooming

Read every open issue with its comments, and every milestone, against the current code and the
agreed design. Look for: issues the code already satisfies or has made meaningless, descriptions
that no longer match the code, issues with no project or milestone, missing prefixes or labels,
blockers that are Done or Canceled, milestones with no issues, and milestones whose issues are
all Done.

Present **one batch**: item, action, one-line reason. Execute the clear-cut lines — prefixes,
labels, placement, removing Done blockers, rewriting a drifted description to what is wanted
now while keeping everything its comments established. Leave for the owner anything that
cancels work, removes a Canceled blocker, changes what a milestone promises, or reorders a
project. Report what you executed and what is waiting.
