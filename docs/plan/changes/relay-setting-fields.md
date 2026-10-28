# relay-setting-fields

> **Approved by the owner, 2026-10-27, in session:** shape B (a flat field list, not JSON Schema
> plus a widget hint), and a secret flagged explicitly instead of marked `writeOnly`. The owner
> also said "I'm auto approving everything", so this proposal and its ticket go ahead without a
> further stop.

Built from #2939. The relay-adapter catalog (`GET /api/relay-adapters`,
`streamlib://relay-adapter-catalog`) describes each adapter's settings as a JSON Schema
(`settings_schema`). That says what the data is, not how a person enters it. A secret is marked
`writeOnly`, which means "never sent back in a read", not "secret". Tatolab.app knows no adapter
by name, so it draws every setting as a text field.

**After this change:**
- **Each adapter lists its settings as fields.** Each field carries:
  - its `name`;
  - its `type`, from a fixed set: `string`, `url`, `number`, `boolean`, `select` (with its
    `options`), `string_list`, `file` (pick a file or paste its text; either way the runtime
    receives the file's text);
  - `secret`, a flag independent of the type;
  - `required`;
  - its `doc`;
  - its `default`, when it has one.
- **The fields come from the adapter's one Rust settings type.** The catalog, what
  `add_relay`/`configure_relay` read, and what the runtime stores can't drift.
- **What a stream author sees:** nothing. No node, port, config key or Python surface changes.

**Scale gate — this skill, no ADR.**
- **Contract changes:** the relay-adapter catalog's per-adapter document, served at
  `/api/relay-adapters` and `streamlib://relay-adapter-catalog`.
- **Not touched:** the RHI, the IPC wire format, the processor model, the Python API, and the
  node catalog's config schemas. Node configs are a different schema and stay JSON Schema.

**Precondition.** The relay adapters entry (§Networking, `ARCHITECTURE.md:3086-3112`) and the
catalog sentence (§Control plane & observability, `:3449-3452`) are DECIDED and SHIPPED.

## Current state (recon, 2026-10-27, `main` `45db0bd3f`)

- **The trait:** `RelayAdapter::settings_schema()` returns a JSON Schema
  (`runtime/streamlib-relay-adapter-sdk/src/lib.rs:288-292`). Each adapter implements it as
  `<Settings>::processor_config_schema_document()`. That derive is the node-config one:
  `RelaySettingsJsonSchema` re-exports `ProcessorConfigJsonSchema` (`lib.rs:23`).
- **Secrecy:** `RelaySecretSetting` writes a `writeOnly` string schema
  (`relay_secret_setting.rs:50-69`). `secret_setting_names_in_the_schema` reads it back to choose
  what goes to the OS secret store (`:72-84`).
- **Settings are read by serde, not by the schema:** `relay_instance_settings_from_json`
  (`lib.rs:251-258`). The schema only describes the settings, and is how the runtime finds the
  secret ones.
- **What's served:** `RelayAdapterCatalogEntry.settings_schema`
  (`relay_instances_of_this_machine.rs:28-30`), mirrored by
  `RelayAdapterCatalogResponseAdapter.settings_schema` (`streamlib-api-server/src/state.rs:37-40`).
  The resource's description (`mcp_resources.rs:49`), the route's (`handlers.rs:328`) and the
  tool descriptions (`mcp.rs:84`, `:485`, `:886`) all name the settings schema.
- **The settings declared today:**
  - moq.pro: `project_id` (a string) and `signing_key` (a secret JWK, as JSON or base64url JSON);
  - Cloudflare: `relay_url` (a secret URL with its token in the path);
  - local: `port` (`Option<u16>`).

  moq.pro's `relay_url` is already gone (#2938, #2942).

## Decided 1 — a flat field list replaces the settings schema (owner, 2026-10-27)

The catalog serves `settings`, a list of fields, in place of `settings_schema`. A relay's settings
are a handful of flat fields with no nesting or conditions, and the document exists to draw a
form. Nothing validates against a schema: settings are read into the adapter's Rust type.
Rejected: JSON Schema plus a per-field widget hint — the hint half has no single standard, and
the app would map keywords to controls itself (owner, 2026-10-27).

## Decided 2 — secrecy is an explicit flag, independent of the type (owner, 2026-10-27)

Each field carries `secret: true | false`. Nothing else marks a secret. A secret file and a secret
URL are both possible.
Rejected: marking a secret `writeOnly` — it says the value is never read back, not that it is a
secret (owner, 2026-10-27).

## Decided 3 — how the Rust type declares a field (session, 2026-10-27)

- A field's `doc`, `required` and `default` come from the settings type's derive: its doc
  comment, whether it is `Option` / `#[serde(default)]`, and its default.
- Its `type` comes from its Rust type:
  - an integer → `number`;
  - `bool` → `boolean`;
  - a unit-variant enum → `select`, with the variants as its `options`;
  - `Vec<String>` → `string_list`;
  - `String` → `string`.

  `url` and `file` are strings a person enters differently, so a setting type declares them.
- A secret is `RelaySecretSetting<F>`, generic over how the secret is entered: as a string, a
  URL or a file. It keeps today's redaction and secret-store behaviour.
- A settings type with a field none of these covers is a programming error, which each adapter's
  catalog test catches.

## MODIFIED: §Control plane & observability — the relay-adapter catalog (`:3449-3452`)

`streamlib://relay-adapter-catalog` serves each relay adapter's kind, the name and icon people see
it by, its doc and its settings. Each setting gives its `name`, its `type` (`string`, `url`,
`number`, `boolean`, `select` with its `options`, `string_list`, or `file`), whether it is
`secret`, whether it is `required`, its `doc` and its `default`. All of it comes from the
adapter's one Rust settings type. A secret is kept in the OS secret store and listed only as set
or unset.

## Not modified

- The node catalog's config schemas (`streamlib://node-catalog`, `/api/registry`).
- `relays.json` and the secret store's layout: which settings are stored where is unchanged.
- `list_relays`' answer.

## Tests

- **SDK:** each Rust shape maps to its field type; a `RelaySecretSetting` of each entry kind is a
  secret field of that type; `required` and `doc` come through; a field of an undeclarable type
  is refused.
- **Each adapter's catalog test:**
  - moq.pro: `project_id` is `string`; `signing_key` is `file` + secret; both required;
  - Cloudflare: `relay_url` is `url` + secret, required;
  - local: `port` is `number`, not required.
- **`tatolabd`:** the secret store keeps exactly the fields flagged `secret`.
- **API server:** `/api/relay-adapters` and the resource serve `settings` with the `secret` flag,
  and no `settings_schema`.

## Tickets

- #2939 — the whole change, one tracer bullet.

## REMOVED

- REMOVED: settings_schema
- REMOVED: secret_setting_names_in_the_schema
