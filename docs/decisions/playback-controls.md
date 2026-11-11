# Playback controls

Rationale for the `[playback-controls]` change to `docs/plan/ARCHITECTURE.md` §Product, §Processor
model and §Control plane, directed by the owner on 2026-11-11 (#3542).

## Trigger

Read this before adding a verb that changes whether a project is held, before letting a playback
verb unload a stream, and before making a read or an edit depend on a project playing.

## Decision

The runtime holds a project, readable and editable, from the moment it is created, started or
opened until it is removed. Start, Stop, Pause and Resume change only its playback: Start builds
fresh processors from the held graph on a new clock, Stop tears every processor, device claim and
the clock down and keeps the graph, Pause holds the flow and Resume carries it on.

## Why

- **The tools a creation app is modelled on work this way.** Unreal's Stop on play-in-editor puts
  the person back in the editor with the level open; DaVinci Resolve's transport never closes the
  timeline. Tato Studio is a creation tool in that mould, and every panel it builds with (the
  graph, the outliner, Properties, Add node, History, previews) reads a held project.
- **Stop as the Docker container's verb took the editor down with the flow.** A stopped project
  answered "is not loaded" to every read and edit, so one press of Stop left the person unable to
  build until they pressed Play.
- **The engine already separates the two.** A loaded stream at `RuntimeStatus::Initial` has its
  graph model and no runtime context, and edits made in it compile at its start. Only the stop
  path discarded the graph.
- **Stop still frees the machine.** Processors, helper node instances, GPU surfaces and device
  claims go at Stop, so a camera or a GPU is released exactly as before; only the graph model and
  its described types stay.

## Rejected

- Stop as the unload of the project, the Docker container's verb — a stopped project could not
  be read or edited.
- A separate editing model beside the loaded stream — a parallel graph per concern; the loaded
  stream is the one model, playing or not.
