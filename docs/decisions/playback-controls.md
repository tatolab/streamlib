# Playback controls

Rationale for the `[playback-controls]` change to `docs/plan/ARCHITECTURE.md` §Product, §Processor
model and §Control plane, directed by the owner on 2026-11-11 (#3542).

## Trigger

Read this before adding a verb that changes whether a project is held, before letting a playback
verb unload a stream, and before making a read or an edit depend on a project playing.

## Decision

The project's lifecycle and its playback are separate verbs. `open_project` holds a project,
readable and editable, until `close_project` or `remove_project` lets it go; `reload_project`
softly refreshes an open project, running only the operations whose inputs changed. `start_playback`, `stop_playback`, `pause_playback` and `resume_playback`
change only its playback and never load, read or refresh anything: Play instantiates fresh
processors from the held graph on a new clock, Stop removes every instance and all run state and
keeps the graph, Pause holds everything, the clock included, and Resume carries it on. Playback
is recorded, so a restart restores it.

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
- **Play starts fresh, like a level.** Unreal's play-in-editor builds every actor from the level
  each time; nothing of the last session leaks into the next except what the game saved on
  purpose. A node's own persisted files and assets are that save; function-node state is not.
- **A timeline needs a transport.** A later timeline view drives playback with these verbs, so
  Pause is a state of its own, recorded and holding the clock.
- **Stop still frees the machine.** Processors, helper node instances, GPU surfaces and device
  claims go at Stop, so a camera or a GPU is released exactly as before; only the graph model and
  its described types stay.

## Rejected

- Stop as the unload of the project, the Docker container's verb — a stopped project could not
  be read or edited.
- A separate editing model beside the loaded stream — a parallel graph per concern; the loaded
  stream is the one model, playing or not.
