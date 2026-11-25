// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib_runtime_client_contract::local_api_wire_contract::{
    CloseViewerToolResult, CreateNodeFileToolResult, CreateNodeUiElementToolResult,
    ListProjectFilesToolResult, ListProjectNodeTypesToolResult,
    ListProjectNodeUiElementsToolResult, ListProjectViewsToolResult, OpenViewerToolResult,
    ProjectCodeCheckResult, ProjectFileTransactionToolResult, ReadProjectFileToolResult,
    WriteStudioViewOutputToolResult,
};

use super::worked_examples;
use super::{EVERY_SURFACE, ListNodeTypes, SetNodeInputValue};
use crate::control_operation::{
    ControlOperation, ControlOperationAgentToolListing, ControlOperationEffect,
    ControlOperationExample, ControlOperationExposure, ControlOperationGroup,
    ControlOperationStability,
};
use crate::control_operation_requests::{
    CheckProjectCodeRequest, CloseViewerRequest, CreateNodeFileRequest, CreateNodeUiElementRequest,
    ListProjectFilesRequest, ListProjectNodeTypesRequest, ListProjectNodeUiElementsRequest,
    ListProjectViewsRequest, OpenViewerRequest, ReadProjectFileRequest, WriteProjectFileRequest,
    WriteStudioViewOutputRequest,
};

/// `list_project_files`: list the files of a project that are not assets.
pub struct ListProjectFiles;

impl ControlOperation for ListProjectFiles {
    type Request = ListProjectFilesRequest;
    type Result = ListProjectFilesToolResult;
    const METHOD_NAME: &'static str = "list_project_files";
    const ONE_LINE_DOC: &'static str = "List the files of a project that are not assets.";
    const AGENT_USAGE_NOTE: &'static str = "List the project's files, or those under `under`, each by its path and length in bytes, sorted by path. Hidden folders such as `.tatolab/`, Python virtual environments, `node_modules/`, `__pycache__/` and symbolic links are not listed.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::File;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::LIST_PROJECT_FILES);
}

/// `read_project_file`: read one file of a project.
pub struct ReadProjectFile;

impl ControlOperation for ReadProjectFile {
    type Request = ReadProjectFileRequest;
    type Result = ReadProjectFileToolResult;
    const METHOD_NAME: &'static str = "read_project_file";
    const ONE_LINE_DOC: &'static str = "Read one file of a project.";
    const AGENT_USAGE_NOTE: &'static str = "Read one of the project's files whole, answering its bytes as `content_base64`, standard base64 with padding, its `byte_len`, and its `content_sha256`, which a `write_project_file` sends back as `expected_sha256`. Refused for a path that is not a file of the project.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::File;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ListedAsATool;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::READ_PROJECT_FILE);
}

/// `create_node_file`: create a node file in a project from the template, as one transaction.
pub struct CreateNodeFile;

impl ControlOperation for CreateNodeFile {
    type Request = CreateNodeFileRequest;
    type Result = CreateNodeFileToolResult;
    const METHOD_NAME: &'static str = "create_node_file";
    const ONE_LINE_DOC: &'static str =
        "Create a node file in a project from the template, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Create a new node file of the project from the runtime's node template, as one transaction an undo reverses: a class under bare `@node` in the current `tatolab.stream` idiom, named after the file, with a type parameter bound to `Texture`, one input pin `picture_in` of that parameter, one field `every_nth` ranged 1 to 60, and one output pin `picture_out` of the same parameter, which carries the type linked into `picture_in` and hands on one picture in every `every_nth` unchanged, each with its description. It loads as a node type of the project with no edits, passes `check_project_code` with no finding, and `list_project_node_types` lists it at once. Answers the `path`, the `node_type` it declares — `<module>:<class>`, what `add_node` takes — and the `transaction` and `commit`. Refused for a path that already holds a file, that is not a `.py` file importable as a Python module, or whose class name the template cannot declare, and for every path `write_project_file` refuses.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::File;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ListedAsATool;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::CREATE_NODE_FILE);
}

/// `create_node_ui_element`: create a control or a node body in a project from the kit's
/// template and register it, as one transaction.
pub struct CreateNodeUiElement;

impl ControlOperation for CreateNodeUiElement {
    type Request = CreateNodeUiElementRequest;
    type Result = CreateNodeUiElementToolResult;
    const METHOD_NAME: &'static str = "create_node_ui_element";
    const ONE_LINE_DOC: &'static str = "Create a control or a node body in a project from the kit's template and register it, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Create a new control for one pin type, or a node body for one node type, in the project's own `node_ui/` folder, as one transaction an undo reverses: the script at `node_ui/<script>`, written from the extension SDK kit's template for its kind (`templates/control.ts` or `templates/node_body.ts`) with the pin type or node type written in, and its entry in `node_ui/tatolab_node_ui.json`, which is created when missing. The script default-exports its element class and never defines a custom element itself. What it writes passes `check_project_code` unchanged, so start from it: edit the script with `write_project_file`, then run `check_project_code` again and read each finding's code and place. The kit is found as `check_project_code` finds it: the runtime unit's, the one its starter names, then the project's installed SDK; the runtime holds no template of its own. Answers the `script_path` and `manifest_path` it wrote, `template_from`, the kit with its version and where it was found (null when sent again under the key of a call already applied, which answers that call's transaction without reading the kit), and the `transaction` and `commit`. Refused when no kit is found or the kit holds no template for the kind, naming why; for a control of a built-in type with no `control` name, as a built-in type's default control is the Studio's own to register (a named type's default, a control of its import path with no name, is written, and `check_project_code` refuses it unless the project's own code declares the type); where a file already is at the script's path, or for the manifest's own name; when the manifest does not read against the view contract; and when it already registers a control of the same pin type and name, or a node body for the same node type.";
    const SINCE_CONTRACT_VERSION: u32 = 3;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
    const GROUP: ControlOperationGroup = ControlOperationGroup::View;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ListedAsATool;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::CREATE_NODE_UI_ELEMENT);
}

/// `write_project_file`: write one file of a project, as one transaction.
pub struct WriteProjectFile;

impl ControlOperation for WriteProjectFile {
    type Request = WriteProjectFileRequest;
    type Result = ProjectFileTransactionToolResult;
    const METHOD_NAME: &'static str = "write_project_file";
    const ONE_LINE_DOC: &'static str = "Write one file of a project, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Write one of the project's files whole, as one transaction: a new file is created with its folders, an existing one overwritten. An undo puts back what the file held, or takes a new file out. Answers the `path`, the `content_sha256` of the bytes written, and the `transaction` and `commit`. With `expected_sha256`, refused when the file changed or was taken out since it was read, so a save never overwrites another change. Content the file already holds is refused, as it changes nothing. Refused for what another tool changes — a stream's graph document, an asset's sidecar or bytes (`replace_asset`), `tatolab-project.json`, a file under `imports/` — and for a file inside a hidden folder or a Python virtual environment, which history does not record.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::File;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Destroys;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ListedAsATool;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::WRITE_PROJECT_FILE);
}

/// `check_project_code`: check a project's code on disk without running a stream, answering each
/// finding as data.
pub struct CheckProjectCode;

impl ControlOperation for CheckProjectCode {
    type Request = CheckProjectCodeRequest;
    type Result = ProjectCodeCheckResult;
    const METHOD_NAME: &'static str = "check_project_code";
    const ONE_LINE_DOC: &'static str =
        "Check a project's code on disk without running a stream, answering each finding as data.";
    const AGENT_USAGE_NOTE: &'static str = "Check the project's code as it is on disk, for any project and with no stream loaded, changing nothing. Four checks run. `python_node_declarations`: each module the node type listing names, or each of `paths`, is imported in the project's interpreter by the reader that loads the node, so a declaration it refuses here is refused at load. `extension_manifest`: each `views/<view>/tatolab_view.json` of the project's own views, or each among `paths`, is read as the view opens: one that does not open is an `error` (`view_manifest_refused` or `view_manifest_missing`), as is one with a port still declared by its older field, its words naming the port and the `pin_type` to write, or the pin types it may become; the project's own `node_ui/tatolab_node_ui.json`, when it has one and `paths` names it or is omitted, is read as `list_project_node_ui_elements` reads it: one that does not read is an `error` coded `node_ui_manifest_refused`, a `controls` entry with no `control` name registers its type's default control, which only the module that declares the type may register: for a built-in type, as `float`, the Studio's own module, so it is always an `error` coded `default_control_not_the_modules_to_register`; for a named type, written as its import path `module:QualifiedClassName`, the module whose code declares it as `list_project_node_types` lists its `named_pin_types`, so an entry for a type an import declares, or no module declares, is that same `error`, its words naming the module that may register it. A `node_bodies` entry for a node type the project's own code does not declare, as `list_project_node_types` lists it, is an `error` coded `node_body_not_the_modules_to_register`. An entry for a type, or a node type, that no module declares while a module that may declare it does not parse, as the listing's `unreadable_modules` names it, is instead a `warning` coded `default_control_not_judged` or `node_body_not_judged`, its words naming that module's file. When that listing is refused, the node bodies and named types' defaults are not judged and the check answers `not_checked` with the listing's refusal. `extension_types` and `extension_lint`: each of the project's own view folders and its `node_ui` folder, or each a path among `paths` lies in, is copied into a scratch folder beside the extension SDK kit and checked as the Studio runs it: `extension_types` type-checks its `.ts` files with the kit's strict options, each `controls` entry tied to the class type it declares, an `error` coded by the compiler as `TS2322`, a tie that does not hold placed at the manifest and its words naming the entry; `extension_lint` reads them with the kit's strict rules, `vendor/` left out, coded by the rule as `typescript(no-explicit-any)`. A `.tsx` file is an `error` coded `tsx_not_checked`, and a `.js`, `.mjs` or `.map` file outside `vendor/` an `error` coded `not_written_in_ts`, its words naming the file and `.ts` as what to write. The kit is the runtime unit's, else the one `TATOLAB_EXTENSION_SDK_KIT` named as the runtime started, else the project's installed `node_modules/@tatolab/tato-app-view-sdk`; with none, both answer `not_checked` with `extension_sdk_not_found` and words naming each place looked; `checked_with` names each program and the kit, with versions. A project with no view or control passes both. Each finding has its `finding_code`, its `severity`, its `words` with the fix, and its `path`, `line` and `column`, with the `node_type` and `port` it is about when it is about one: a refused declaration answers the reader's code, such as `value_input_has_no_default`; a module that does not import answers `module_does_not_import` with its traceback; a node declared in a retired form, such as `@node.function`, or a retired name a module imports, is an `error` coded `declaration_retired` whose words name its replacement. `verdict` is `failed` on any `error`, else `not_fully_checked` when a check did not run to its end, else `passed`; warnings alone pass. A check still running at its bound, 60 seconds for the declarations and 120 for each program over each extension folder, is killed and answers `not_checked` with `check_ran_out_of_time` and the file it was importing, or the folder it was checking, as `refused_file`, keeping the findings of the files it finished; `files_checked` lists only those, each with its `content_sha256`.";
    const SINCE_CONTRACT_VERSION: u32 = 3;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
    const GROUP: ControlOperationGroup = ControlOperationGroup::File;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ListedAsATool;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::CHECK_PROJECT_CODE);
}

/// `list_project_views`: list the Tato Studio views a project brings.
pub struct ListProjectViews;

impl ControlOperation for ListProjectViews {
    type Request = ListProjectViewsRequest;
    type Result = ListProjectViewsToolResult;
    const METHOD_NAME: &'static str = "list_project_views";
    const ONE_LINE_DOC: &'static str = "List the Tato Studio views a project brings.";
    const AGENT_USAGE_NOTE: &'static str = "List every Tato Studio view a project brings — each folder of its own `views/`, then of each import's `imports/<folder>/views/` — read from the folder's `tatolab_view.json` without running anything in it. Each view under `views` has its `view_type`, `<project name>:<view folder>`, which is a node type of each stream the project runs when the view opens and its `placement` is `node`; the `project`, `project_name` and `import_folder` (null for the project's own) of the project bringing it; its `view_folder_in_the_project`, the folder relative to the project, as `views/<folder>` or `imports/<import>/views/<folder>`, which names its files to a project file read; its `title`, `description`, `icon`, `preview` and `placement` (`area` or `node`); its declared `inputs` and `outputs`, each a `name`, its `pin_type`, a `description` and the `range` its manifest states; and its `manifest` as written. A folder whose manifest is missing, is not JSON, or does not read against view contract 1.0 is listed with `opens` false and why in `not_opened_because`; the rest still lists.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::View;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::LIST_PROJECT_VIEWS);
}

/// `list_project_node_ui_elements`: list the controls and node bodies a project and its imports
/// register.
pub struct ListProjectNodeUiElements;

impl ControlOperation for ListProjectNodeUiElements {
    type Request = ListProjectNodeUiElementsRequest;
    type Result = ListProjectNodeUiElementsToolResult;
    const METHOD_NAME: &'static str = "list_project_node_ui_elements";
    const ONE_LINE_DOC: &'static str =
        "List the controls and node bodies a project and its imports register.";
    const AGENT_USAGE_NOTE: &'static str = "List every control and node body a project registers in its own `node_ui/tatolab_node_ui.json`, then those each import registers in `imports/<folder>/node_ui/tatolab_node_ui.json`, read from the manifest without running any of its scripts and without waiting on a compiler. Each row under `elements` names its `module` — the `project`, `project_name` and `import_folder` (null for the project's own) of the project registering it, whose name qualifies a control name it registers, as `acme:knob` — its `script_path`, the script relative to the project, which names it to a project file read, and its `element`, whose `kind` is `control`, with the `pin_type` whose inputs it edits or shows and its `control` name, null for the type's default, or `node_body`, with the `node_type` whose node it draws. `manifests` holds each manifest that reads as written, with its `manifest_path`. A manifest that cannot be read, is not JSON, or does not read against view contract 1.1 is listed under `unreadable_manifests` with its `manifest_path` and why; the other modules still list. A module with no manifest registers nothing. Whether an entry is one its module may register is judged by `check_project_code`'s `extension_manifest` check, not here: a listed entry may still fail it.";
    const SINCE_CONTRACT_VERSION: u32 = 3;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
    const GROUP: ControlOperationGroup = ControlOperationGroup::View;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> =
        Some(worked_examples::LIST_PROJECT_NODE_UI_ELEMENTS);
}

/// `open_viewer`: open a viewer of a node's output port, held open for the connection that opened
/// it.
pub struct OpenViewer;

impl ControlOperation for OpenViewer {
    type Request = OpenViewerRequest;
    type Result = OpenViewerToolResult;
    const METHOD_NAME: &'static str = "open_viewer";
    const NEEDS_A_CONNECTION_THAT_STAYS_OPEN: bool = true;
    const ONE_LINE_DOC: &'static str =
        "Open a viewer of a node's output port, held open for the connection that opened it.";
    const AGENT_USAGE_NOTE: &'static str = "Open a viewer of any output port of a running node, adding no node and changing no graph: the runtime holds the port's channel open and answers the `viewer` id, the `iceoryx2_domain_root`, the `channel` whose reserved subscriber slot the viewer takes from its own process, and the `wake_service` the node notifies on each write, whose one listener the viewer takes so it waits for a message rather than reading on a timer. The answer's `pin_type` is the port's pin type, resolved as `graph` serves it on the port, absent on a port with none, and `viewer_reads` says which messages the viewer reads: `every_message` for a port carrying samples, an Audio port or one with no pin type whose messages are sample blocks, in order as far back as the channel holds them for it, so a viewer can play the sound, and `latest_message` for every other port, a port of a number type included, skipping what it falls behind on; either way the channel overwrites what the viewer has not read, so it never slows the stream; a Texture message names a pooled frame rather than carrying its pixels, and the viewer imports that frame's surface from the `surface_share_address` the way a helper process does. The viewer stays open until `close_viewer` or the end of the connection that opened it, so it is opened only over a connection that stays open: `/mcp/stdio` or `/control/ws`. Refused for a node that is not running, with `structuredContent.refusal.not_running_because` naming why: `playback_stopped`, `playback_paused` (a node added while its project is paused), `preparing` or `node_failed`, the runtime's words beside it in `runtime_words`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Observe;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::OPEN_VIEWER);
}

/// `close_viewer`: close a viewer this connection opened.
pub struct CloseViewer;

impl ControlOperation for CloseViewer {
    type Request = CloseViewerRequest;
    type Result = CloseViewerToolResult;
    const METHOD_NAME: &'static str = "close_viewer";
    const NEEDS_A_CONNECTION_THAT_STAYS_OPEN: bool = true;
    const ONE_LINE_DOC: &'static str = "Close a viewer this connection opened.";
    const AGENT_USAGE_NOTE: &'static str =
        "Close a viewer this connection opened, letting go of its port's channel.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Observe;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::CLOSE_VIEWER);
}
