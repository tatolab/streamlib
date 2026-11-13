# edit-attribution

> **Direction from the owner, 2026-11-13 (project thread).** A recorded edit says three things,
> kept apart: the **adapter** it arrived on, the **client** that sent it, like a browser's
> User-Agent, and the **author**, who performed it. The client is a full accounting of the
> program: "whatever MCP gives us so we know exactly the program, version, etc. … I know I
> can't enforce it but that's the level of detail I'd want". The author is "an attributable
> person", never an agent and never a string like "Jonathan using Claude Code". "Author we may
> not get, so it could be unknown." The client name stays optional: OBS and other clients may
> send none. Multiplayer will tie the author back to a real user in an account.

This changes what every recorded transaction holds, the `author` argument of every change
operation, and what history and the change feed answer. It touches the local API's public
contract, so it carries an ADR once approved.

## Current state (recon, `main` `e50f32b`)

- **One field, two meanings.** `ProjectTransactionAuthor { kind, name }` with
  `ProjectTransactionAuthorKind::{Person, Agent, Runtime}`
  (`runtime/streamlib-project-repository/src/project_transaction.rs:18-58`) is the only
  attribution a transaction carries (`:725-731`).
- **The client name becomes the author.** `the_author_of_a_change`
  (`runtime/streamlib-api-server/src/control_operation_dispatcher.rs:1523-1546`) records a
  caller-named author as given. With none, it records `Agent` named by the request's client
  name, or `AUTHOR_NAME_OF_AN_UNNAMED_CLIENT = "an MCP client"` (`:72`), whatever the adapter.
- **What the runtime keeps of the client is only a name.**
  - Over MCP: `clientInfo.name` only (`runtime/streamlib-api-server/src/mcp.rs:219-230`).
  - Over `/control` and `/control/ws`: the `tatolab-control-client-name` header
    (`runtime/streamlib-runtime-client-contract/src/local_api_wire_contract.rs:35`, read at
    `control_json_rpc_adapter.rs:183-188`).
  - The `ControlRequestOrigin` carries it (`control_operation_dispatcher.rs:74-84`) and nothing
    names the adapter.
- **History is Git.** Each commit's signature is the author's name with
  `<kind>@tatolab.invalid` (`project_history_repository.rs:100-128`, `project_transaction.rs:48-57`).
  The transaction's JSON rides the `Tatolab-Transaction` trailer. Records are at
  `PROJECT_TRANSACTION_FORMAT_VERSION = 2` (`project_transaction.rs:665`) and every older one
  reads back.
- **Undo walks per author.** `undo_by` matches `applied.transaction.author == *author`
  (`project_repository.rs:1555-1640`). The `undo`/`redo` `author` argument defaults to the
  client's agent (`control_operation_requests.rs:1473`).
- **Readers see a name only.** The change feed and `list_transactions` answer `author_name`
  (`mcp_project_change_feed.rs:79`, `:361`). Tato Studio shows "Last edit by <author_name>".

## ADDED

- ADDED: `ProjectTransactionAdapter`, which the runtime records and no caller sends.
  - `mcp`: `/mcp` and `/mcp/stdio`.
  - `control_web_socket`: `/control/ws`.
  - `control_post`: one `POST /control`.
  - `runtime`: a change the runtime makes on its own, such as a first-party package move or a
    reload at start.
  - The adapter is set where each surface builds its `ControlRequestOrigin`.
- ADDED: `ProjectTransactionClient`, the program as it declared itself, each field optional and
  recorded as missing when absent, never filled in.
  - Fields: `name`, `version`, `title`, `platform`, and `protocol_version` (MCP only).
  - Over MCP it is the whole `clientInfo` the `initialize` sent (`name`, `version`, `title`)
    with the negotiated protocol version.
  - Over `/control` and `/control/ws` it comes from request headers beside today's name header:
    `tatolab-control-client-version` and `tatolab-control-client-platform`.
  - The runtime's own changes carry `tatolabd` and its version.
  - Nothing is enforced: a client that declares nothing is recorded as the empty client.
- ADDED: `ProjectTransactionAuthor` as an attributable person.
  - Fields: `account`, an account id, `null` until accounts exist; and `display_name`.
  - It is optional on every transaction: `None` is the unknown author.
  - Only a caller names it, through the `author` argument; it is never derived from the client.
- ADDED: a transaction records `adapter`, `client` and `author: Option<…>` side by side, at
  `PROJECT_TRANSACTION_FORMAT_VERSION = 3`.
  - The Git commit signature is the author's `display_name`, or `unknown` when there is none.
  - Its e-mail is `<adapter>@tatolab.invalid`, which Git requires.
  - The trailer's JSON stays the record of all three.
- ADDED: the change feed and `list_transactions` answer `author` (`{account, display_name}` or
  `null`), `client` and `adapter` per transaction, in place of `author_name`.
- ADDED: Tato Studio and `tatolab-cli` declare themselves fully.
  - Studio: name `tato-studio`, its version, and its platform (`macOS 26.1`, `Linux`).
  - The CLI: name `tatolab-cli` and its version.
  - Studio sends the person as `author` once it has one to send. Until then it sends none and
    its edits read as unknown author via Tato Studio. That is tatolab-app work and waits on this
    change.

## MODIFIED

- MODIFIED: the `author` argument of every change operation, and of `undo` and `redo`, becomes
  `{account?, display_name}`.
  - `kind` goes.
  - `AUTHOR_ARGUMENT_DESCRIPTION` and the `undo`/`redo` description say the author is the person
    the change is attributed to, and that omitting it records an unknown author.
- MODIFIED: `ControlRequestOrigin` carries `adapter` and the whole `client` in place of
  `client_name`.
- MODIFIED: older records read back.
  - Version 1 or 2 `Person` → author `{account: null, display_name: name}`.
  - `Agent` → no author, client `{name}`.
  - `Runtime` → no author, adapter `runtime`, client `tatolabd`.
  - Any other older record's adapter is unknown, read as `null`.
- MODIFIED: ARCHITECTURE.md §Processor model (the project's transactions, around line 2174) and
  §Control plane (around line 4500, "each change is one transaction by its `author` … which
  `undo` and `redo` walk per author") say a transaction records its adapter, its client and
  its optional author, and undo walks per the key below.

## REMOVED

- REMOVED: ProjectTransactionAuthorKind
- REMOVED: ProjectTransactionAuthorKindArgument
- REMOVED: AUTHOR_NAME_OF_AN_UNNAMED_CLIENT
- REMOVED: the_author_of_a_change
- REMOVED: commit_email
  - The kind-named e-mail goes with the kind; the adapter-named one is built where the commit is.

## [NEEDS DECISION] What undo walks, now that the author is optional

Today undo reverses the latest edit of the same author, and every edit has one. With an
optional author, an edit with none needs a key too, or no one can undo it.

- **A. Author and client together (recommended).** Undo walks the edits of the same author
  using the same program: Ctrl+Z in Studio undoes your Studio edits, not what Claude Code just
  did in your name. An edit with no author walks by its client alone, so an unnamed OBS plugin
  undoes its own edits. This matches how undo works in an editor and keeps a person's two tools
  from undoing each other.
- **B. The author when named, else the client.** Simpler, but a person working in Studio and
  through Claude Code at once undoes across both.
- **C. The author only, and edits with no author cannot be undone by `undo`.** Strict, but it
  strands every unnamed client's edits.

With A, the `undo` and `redo` key is `(author, client name)` matched on the account when there
is one and the display name otherwise, and an older record maps as above. Two cases A leaves
open, to settle with it:

- **No author and no client name.** Two unnamed clients share an empty key and would undo each
  other's edits. Options: add the adapter to the key; or let such edits be undone only by
  naming their transaction.
- **Older `Person` records.** They map with no client, so a person undoing from a named client
  after the upgrade would not reach them. Options: let a key with no client match any client of
  the same author; or leave older edits undoable only from a caller that sends no client name.

## Not in this change

- Who may claim which account. Once accounts exist the runtime will need to check a claimed
  `account`. Until then the author is what the caller says, which suits a local machine.
- Showing the client and adapter in Studio's Transactions view. That is tatolab-app work once
  the feed answers them.

## Proof

- **Repository tests:**
  - a version-3 transaction round-trips its adapter, client and absent author;
  - every version-1 and version-2 shape reads back as mapped above;
  - undo walks per the decided key.
- **Engine test over a real engine:**
  - an edit over each adapter (MCP with a full `clientInfo`, `/control/ws` with the client
    headers, `POST /control` with none) records that adapter and that client, with no author
    unless one was sent;
  - a named author is recorded with no client name in it.
- **Gates:** the contract compatibility gate shows the `author` argument's change, and
  `generate-control-contract` and `generate-runtime-client` regenerate.
