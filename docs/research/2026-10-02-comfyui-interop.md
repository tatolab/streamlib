# Research memo: two-way interop with ComfyUI

2026-10-02, for a future `/align`; nothing here is decided. Question: can a stream run ComfyUI
custom-node packs, ideally by translating a node definition into a Tatolab node
(ComfyUI → streamlib), and can ComfyUI workflows use Tatolab nodes or live ports
(streamlib → ComfyUI)? For each direction: is it feasible, what is the cleanest mechanism,
what breaks, and what is unknown. Read against the plan at worktree HEAD `db74c6659`.

Evidence is marked **[V]** verified in a primary source (file:line at a pinned commit, or an
official page), **[P]** probed in this session (command and result stated), or **[I]**
inferred. No blog or secondary write-up is cited.

**Pinned sources.** Line numbers below are at these commits:

- `CU` = Comfy-Org/ComfyUI master `36c0b0a687e5` (2026-10-02, v0.38.0):
  `https://github.com/Comfy-Org/ComfyUI/blob/36c0b0a687e5e6d7b55e3e61ab24262ffc0f2508/<path>#L<n>`
- `DOCS` = Comfy-Org/docs `37d925732d8c` (the source of docs.comfy.org)
- `CS` = livepeer/comfystream `4e9dace24b19` (2025-12-31)
- `HS` = hiddenswitch/ComfyUI `974c920ec827` (2026-10-02), a third-party fork
- `PI` = Comfy-Org/pyisolate `8ff4733f649f` (2026-08-21)
- `CM` = Comfy-Org/ComfyUI-Manager `2a6cb164309d` (2026-10-01)
- `FE` = Comfy-Org/ComfyUI_frontend `2409252b6459`
- Eight popular packs, cloned for a sample: Impact-Pack `429d015`, KJNodes `d3cfe21`,
  VideoHelperSuite `4d907be`, AnimateDiff-Evolved `9257651`, controlnet_aux `0cd2904`,
  Custom-Scripts `609f3af`, ComfyUI_essentials `9d9f4be`, rgthree-comfy `449c58f`.

## Verdict

**ComfyUI → streamlib: feasible for a bounded set of workflows. Translating arbitrary packs
automatically is not feasible.**

- The cleanest mechanism is **one Tatolab node that hosts a ComfyUI executor and runs a
  workflow subgraph**. The node's setup builds the executor and imports the packs, the first
  run loads the models, and then each frame re-runs the prompt. ComfyUI's output cache skips
  the loader nodes, whose inputs have not changed, and runs only the part that depends on the
  frame. This is exactly how comfystream works. The node runs in a processor interpreter, so it
  fits the placement rule as it stands.
- Translating one ComfyUI node into one Tatolab node fails as a general mechanism.
  - MODEL, CLIP, VAE and CONDITIONING are live Python objects tied to ComfyUI's model manager.
    They cannot cross the process boundary that every Tatolab Python node has. ComfyUI's own
    process-isolation branch had to write a proxy for each of them.
  - A node's interface cannot be read without importing the node.
  - Six of the eight packs sampled reference `PromptServer`, and `PromptServer.instance` only
    exists once a server has been constructed.
- **The biggest blockers:**
  - **Licence.** ComfyUI is GPL-3.0, and so are many packs. A processor interpreter that
    imports both `comfy` and the BUSL `tatolab.stream` puts both in one address space. They
    would also exchange frames over shared memory. That is the pattern the FSF FAQ calls one
    combined program. Counsel has to decide whether that binds us.
  - **Packaging.** Upstream ComfyUI is not pip-installable (probed).
  - **Environments.** Packs assume one environment that carries ComfyUI's torch.
  - **Realtime.** ComfyUI is realtime only for workflows built from realtime-specific packs
    (TensorRT, StreamDiffusion). comfystream's curated list shows this. An ordinary sampler
    workflow is not realtime.

**streamlib → ComfyUI: the read side is feasible and simple. The write side has no streamlib
mechanism today.**

- The cleanest mechanism is **a separate ComfyUI custom-node pack with a "read Tatolab port"
  node**. Each time the prompt runs, it fetches one frame from an exposed port's `png` (or
  `ndjson`) form by URL. It returns `NaN` from `IS_CHANGED`, so every run fetches again. It
  talks to Tatolab over HTTP only.
- The `png` form is the plan's **OPEN** URL-grammar direction, not yet decided. On the same
  machine, today's `tap` + `exchange` verbs would serve meanwhile.
- **Writing from ComfyUI into a stream** has no mechanism yet. An exposed port is an output
  only, so this needs an ingest path. That is a gap for `/align`.
- **Exposing Tatolab's node catalog as ComfyUI nodes** is mechanically possible but
  semantically poor. Tatolab ports carry no types, and ComfyUI sockets need them. Also, no
  Tatolab verb runs a node once on a given input.
- The pack itself would most likely have to be GPL-3.0, because it imports `comfy`. That
  distribution would be separate from Tatolab.
- Neither ComfyUI core nor any Comfy-Org repository has a streaming executor. "Realtime" in
  ComfyUI means re-queuing the prompt, either from the frontend's instant auto-queue or from a
  driver loop like comfystream's.

## Evidence: ComfyUI

### 1. The node contract, and how much of it is machine-readable

- **Two schemas, both live.** A pack's `__init__.py` is executed with `exec_module` into the
  one server process (`CU nodes.py:2254-2274`).
  - **V1:** if the module exports `NODE_CLASS_MAPPINGS`, each class is registered. This is the
    legacy shape: `INPUT_TYPES` classmethod, `RETURN_TYPES`, `FUNCTION`, `OUTPUT_NODE`,
    `IS_CHANGED`, `VALIDATE_INPUTS`, `INPUT_IS_LIST` (`CU nodes.py:2303-2313`).
  - **V3:** otherwise an async `comfy_entrypoint()` returns a `ComfyExtension` whose async
    `get_node_list()` yields `io.ComfyNode` classes. Each one's `define_schema()` returns a
    `Schema` (`CU nodes.py:2314-2342`; `CU comfy_api/latest/_io.py:1830-1880, 2132-2175`).
  - V3 `execute` is a classmethod. "Node objects do not expose 'state'"
    (`DOCS custom-nodes/v3_migration.mdx:28`). V1 nodes are instantiated once per node id and
    kept in `caches.objects` across prompts (`CU execution.py:498-501`), so a V1 node can hold
    state.
  - The docs say new features land only in V3 (`DOCS v3_migration.mdx:8`). [V]
- **Adoption is split.** [P]
  - In core, 130 of 140 `comfy_extras/nodes_*.py` define `comfy_entrypoint`.
  - Of the eight sampled packs, only AnimateDiff-Evolved does. The other seven are V1.
- **The interface is reachable only by importing and calling the node.**
  - `/object_info` calls each class's `INPUT_TYPES()` (V1) or `GET_NODE_INFO_V1()` (V3) live
    (`CU server.py:759-819`).
  - Dropdown values are computed at call time from the model folders, for example
    `folder_paths.get_filename_list("checkpoints")` (`DOCS custom-nodes/backend/datatypes.mdx`,
    COMBO).
  - So the descriptor depends on the environment and is not static data. [V]
- **The registry keeps a partial copy.** `api.comfy.org` has `/nodes/{id}/versions/{v}/comfy-nodes`.
  Each entry carries `input_types`, `return_types` and `output_is_list` as JSON strings
  (`https://api.comfy.org/openapi`). [P]
  - Some packs are filled in: `ConstrainImage|pysssss` has full `input_types`.
  - Others are empty: Impact-Pack 8.28.3 has 99 pages of names with blank `input_types`.
  - Neither node in the registry's very first listed pack, `spectrum-ltx`, has an entry.
  - The registry lists 5,817 packs.
- **Features that defeat a static translation:** [V]
  - Lazy inputs (`check_lazy_status`, `CU execution.py:504-511`).
  - Node expansion: a node returns a new subgraph at execution time
    (`DOCS custom-nodes/backend/expansion.mdx`).
  - Hidden inputs: `PROMPT`, `UNIQUE_ID`, `EXTRA_PNGINFO`, `DYNPROMPT`
    (`CU comfy_api/latest/_io.py:1716-1722`).
  - List semantics (`INPUT_IS_LIST` / `OUTPUT_IS_LIST`, `DOCS custom-nodes/backend/lists.mdx`).
  - `VALIDATE_INPUTS` sees only constants (`DOCS custom-nodes/backend/server_overview.mdx`).

### 2. Data on the wire between nodes

| Type | Python value | Can it cross a process boundary? |
|---|---|---|
| `IMAGE` | `torch.Tensor` `[B,H,W,C]`, C=3 (`DOCS images_and_masks.mdx`). `LoadImage` builds it as `float32 /255.0` (`CU nodes.py:1792-1805`). | Yes: a plain tensor |
| `MASK` | `torch.Tensor` `[B,H,W]`, often squeezed to `[H,W]` (`DOCS images_and_masks.mdx`) | Yes |
| `LATENT` | `dict` with `samples` `[B,C,H,W]` plus other keys (`DOCS datatypes.mdx`) | Mostly: a tensor plus a small dict |
| `AUDIO` | `dict` `{waveform [B,C,T], sample_rate}` (`DOCS datatypes.mdx`) | Yes |
| `INT` / `FLOAT` / `STRING` / `BOOLEAN` / `COMBO` | scalars and `str` | Yes: bag fields |
| `MODEL` | `ModelPatcher` (`CU _io.py:614-617`; `CU comfy/model_patcher.py:340`) | **No**: a live object owned by the model manager |
| `CLIP` / `VAE` / `CONTROL_NET` | live Python objects | **No** |
| `CONDITIONING` | `list[(Tensor, PooledDict)]`. The dict can hold a `ControlNet`, a `Gligen` or a `HookGroup` object (`CU _io.py:461-579`). | **No** in general |
| `SAMPLER` / `GUIDER` / `NOISE` | objects with methods (`DOCS datatypes.mdx`) | **No** |
| `VIDEO` | `VideoInput`, an abstract object with `get_components()` (`CU _io.py:661-664`; `CU nodes.py:1770`) | Through its components only |

- **Device and dtype.** The documentation omits two facts that matter for frames. [V]
  - Tensors between nodes live on `intermediate_device()`, which is the **CPU** unless
    `--gpu-only` is set (`CU comfy/model_management.py:1267-1271`).
  - Their dtype is `intermediate_dtype()`: float32, or float16 under `--fp16-intermediates`
    (`:1273-1277`).
- **Even a pure filter copies to the GPU and back.** `ImageBlur` moves its input to
  `get_torch_device()`, computes, and returns `.to(intermediate_device())`
  (`CU comfy_extras/nodes_post_processing.py:98-113`). In the default configuration that is
  two copies across the bus per node per frame. [V]
- **Only the tensor types are translatable** across a Tatolab link:
  - IMAGE, MASK, LATENT samples, AUDIO and scalars.
  - The model-manager types are not translatable, and neither is anything that carries one.

### 3. Execution model: one run per prompt, with a cache. Not streaming.

- **A prompt is one run of the graph.**
  - `POST /prompt` validates the graph. It refuses one with no output node, as
    `prompt_no_outputs` (`CU execution.py:1179`), then puts it on a queue
    (`CU server.py:1080-1153`).
  - A single worker thread runs `PromptExecutor.execute` once per queued item
    (`CU main.py:322-372`).
- **Caching decides what re-runs.**
  - Each node's output is cached. `IsChangedCache` asks `IS_CHANGED` / `fingerprint_inputs` to
    decide whether it re-runs (`CU execution.py:64-106`).
  - Returning `float("NaN")` forces a re-run on every prompt
    (`DOCS server_overview.mdx`, IS_CHANGED).
- **Continuous running means re-submitting.**
  - The frontend has `AutoQueueMode` with the values `disabled` / `change` / `instant-idle` /
    `instant-running` (`FE src/stores/queueSettingsStore.ts`). It re-submits the prompt.
  - Core's `WebcamCapture` subclasses `LoadImage` and takes one still image per queue
    (`CU comfy_extras/nodes_webcam.py:7-29`).
  - The websocket streams progress and previews, and `SaveImageWebsocket` sends full images
    over it (`CU custom_nodes/websocket_image_save.py`).
  - None of these is a per-frame executor. [V]
- Of Comfy-Org's 86 public repositories, none is named for realtime or streaming. `pyisolate`
  is the relevant one (§6). [P] `gh api orgs/Comfy-Org/repos`.

### 4. Loading, headless use and packaging

- **Library mode exists in core.** `comfy.cli_args` parses `sys.argv` only when
  `comfy.options.enable_args_parsing()` has run, and `main.py` does that first thing.
  Otherwise it uses `parse_args([])` defaults (`CU comfy/cli_args.py:282-285`;
  `CU main.py:2`). So `import comfy...` works outside the server. [V]
  - Importing `comfy.model_management` probes the device at module level
    (`total_vram`, `CU comfy/model_management.py:364`). [V]
- **Upstream is not pip-installable.** [P]
  - `pyproject.toml` has `[project]` but no `[build-system]` and no package list.
  - `pip wheel --no-deps .` fails with "Multiple top-level packages discovered in a
    flat-layout: ['app', 'comfy', … 'comfy_api_nodes']" (setuptools 84, Python 3.12).
- **The PyPI name is taken by a placeholder.** `comfyui` on PyPI is one 0.0.1 upload from
  2024-06-13, with no project URLs. [P] `pypi.org/pypi/comfyui/json`.
- **The only installable "ComfyUI as a library" is a third-party fork.**
  - hiddenswitch/ComfyUI is published as `comfyui` 0.37.0.6, licensed `GPL-3.0-or-later`
    (`HS pyproject.toml:2-10`).
  - It is documented as "Use ComfyUI as a Python library (`import comfy`) … without the web
    server" (`HS README.md:27,64-78`).
  - It discovers packs from a `comfyui.custom_nodes` entry-point group
    (`HS comfy/nodes/package.py:204`).
  - comfystream pins this fork (`CS pyproject.toml:12`). [V]
- **Packs touch the server at import time.** [P]
  - In the sample, packs referencing `PromptServer`:

    | Pack | Files |
    |---|---|
    | Impact-Pack | 6 |
    | KJNodes | 6 |
    | Custom-Scripts | 6 |
    | rgthree | 4 |
    | VideoHelperSuite | 4 |
    | AnimateDiff-Evolved | 0 |
    | controlnet_aux | 0 |
    | essentials | 0 |

  - Some do it at module level, for example `routes = PromptServer.instance.routes`
    (`rgthree-comfy/py/server/rgthree_server.py:14`) and
    `@PromptServer.instance.routes.post("/sam/prepare")`
    (`ComfyUI-Impact-Pack/modules/impact/impact_server.py:60`).
  - `PromptServer.instance` is assigned only in `PromptServer.__init__`
    (`CU server.py:218-220`). Importing these packs without a server needs a stub server.
    comfystream's fork supplies a `ServerStub` (`HS comfy/client/embedded_comfy_client.py:158`).
  - All eight packs import `folder_paths` or `comfy.*`. Six of the eight touch
    `model_management`.

### 5. Dependencies and environments

- **One shared environment.**
  - ComfyUI imports every pack into its own process (`CU nodes.py:2352-2400`).
  - ComfyUI-Manager installs each pack's `requirements.txt` with `sys.executable`'s pip, and
    runs `install.py` (`CM glob/manager_core.py:925-951, 2176-2211`).
  - Its `PIPFixer.fix_broken()` then rolls back `torch`/`torchvision`/`torchaudio` if an
    install changed them, and repairs the opencv variants (`CM glob/manager_util.py:549-583`).
    That rollback is evidence that packs routinely fight over the shared torch. [V]
- **Registry rules.**
  - The Registry forbids runtime `pip install` through `subprocess`
    (`DOCS registry/standards.mdx`).
  - Metadata is `pyproject.toml` with `[project]` (name, version, licence, dependencies,
    classifiers for OS and accelerator) plus `[tool.comfy]` (PublisherId, DisplayName, Icon …)
    (`DOCS registry/specifications.mdx`).
  - `comfy node install <id>` installs from it. comfy-cli is GPL-3.0-only (PyPI metadata). [V]
- **Core's dependency floor.** `requirements.txt` pins `comfyui-frontend-package==1.53.10`,
  `comfy-kitchen==0.2.37` and `comfy-aimdo==0.5.5`, and needs `torch` unpinned, `av>=17`,
  `transformers>=4.50.3`, and so on (`CU requirements.txt`). [V]

### 6. GPU memory, and what leaving the process costs

- **The model manager is process-global.**
  - `comfy.model_management` keeps one list, `current_loaded_models` (`:635`), and one
    `vram_state` (`VRAMState` DISABLED … SHARED, `:45-59`).
  - It loads and evicts through `load_models_gpu` / `free_memory` (`:939`, `:893`).
  - The prompt worker unloads on the `free_memory` / `unload_models` flags
    (`CU main.py:392-400`).
  - A node running outside ComfyUI's process gets its own manager. That manager has no view of
    any other process's VRAM use. [V]/[I]
- **Comfy-Org's own isolation work shows the cost.**
  - PR #13324, "process isolation for custom nodes via pyisolate", merged on 2026-04-07 into
    branch `pyisolate-support`, not into master. Today the branch is 8 commits ahead of master
    and 955 behind (`gh api repos/Comfy-Org/ComfyUI/compare/master...pyisolate-support`). [P]
  - It adds `comfy/isolation/` with `model_patcher_proxy.py`, `clip_proxy.py`, `vae_proxy.py`,
    `model_sampling_proxy.py`, `proxies/model_management_proxy.py`,
    `proxies/folder_paths_proxy.py` and `proxies/prompt_server_impl.py`.
  - A pack opts in with `[tool.comfy.isolation]` in its `pyproject.toml`
    (`comfy/isolation/manifest_loader.py:42-47` on that branch).
  - pyisolate (MIT) runs each extension in its own uv venv over JSON-RPC. It shares torch
    tensors through `/dev/shm` and CUDA IPC (`PI README.md:1-20, 92-93, 130`).
  - Those proxy files are the exact list of what breaks once a node leaves ComfyUI's
    interpreter. [V]

### 7. Prior art: comfystream (MIT, livepeer)

- **comfystream runs Option B below.** It wraps the fork's `EmbeddedComfyClient`
  (`CS src/comfystream/client.py:7-21`).
  - A runner loop calls `queue_prompt(prompt)` again and again
    (`CS client.py:79-107`).
  - `LoadTensor` takes a frame from a process-global `Queue(maxsize=1)`
    (`CS src/comfystream/tensor_cache.py:9`) and returns `IS_CHANGED → NaN`
    (`CS nodes/tensor_utils/load_tensor.py:30-39`).
  - `SaveTensor` is an `OUTPUT_NODE` that pushes to an output queue
    (`CS nodes/tensor_utils/save_tensor.py:18-26`).
  - `convert_prompt` rewrites a workflow's `LoadImage`/`SaveImage`/`PreviewImage` into these
    nodes (`CS src/comfystream/utils.py:68-98`).
  - Loader nodes re-run never: their inputs are constant, so the output cache serves them.
  - Frames cross as `rgb24 → float32 /255 → [1,H,W,3]` CPU tensors, and back through
    `*255 → uint8 → .cpu()` (`CS src/comfystream/pipeline.py:604-614, 654-664`).
- **Realtime comes from curated packs, not from ComfyUI.** Its node list is TensorRT,
  DepthAnything-TensorRT, StreamDiffusion, FasterLivePortrait, SAM2-Realtime, Torch-Compile and
  others (`CS configs/nodes.yaml`). The shipped workflows are named `*-tensorrt-*`
  (`CS workflows/comfystream/`). [V]
- **comfystream is also a ComfyUI pack**, which is the direction-2 analogue. Its `__init__.py`
  exports `NODE_CLASS_MAPPINGS` and a `WEB_DIRECTORY`, and its server manager launches the
  comfystream server as a `sys.executable` subprocess (`CS __init__.py`;
  `CS nodes/server_manager.py:163-177`). [V]

## Evidence: streamlib (Tatolab)

- **Placement.** Every Python node runs in its own processor interpreter, exec'd from the
  stream's venv. The runtime process imports nothing from a pack
  (`.claude/rules/placement.md`; `docs/plan/GLOSSARY.md` "Processor interpreter",
  "Placement"; `docs/decisions/one-runtime-per-machine.md:73-76`, which names this as "the
  ComfyUI use, with the isolation ComfyUI lacks"). A ComfyUI host node is an ordinary Python
  node under that rule. [V]
- **Environments are already isolated per stream.** "Streams needing conflicting Python
  packages each start from their own venv". A stream links to another stream's port on the
  same machine without exposing it, and copies no pixels (`docs/plan/ARCHITECTURE.md:1414-1427`,
  DECIDED). So ComfyUI's torch pins can be held to one stream's venv, linked to the rest. This
  is the analogue of pyisolate's per-pack venv. [V]
- **Packs.** The pack and registry model is **OPEN**: pip into a project venv,
  `[tool.tatolab]`, `uv sync` (`ARCHITECTURE.md:578-595`). [V]
- **Frames into torch.**
  - A frame's `__dlpack__` is `kDLCUDA` on Linux, over one engine-side blit, and `kDLMetal` on
    macOS (`sdk/streamlib-python-wheel/python/streamlib/_engine.pyi:1637-1668`).
  - Frames are BGRA/RGBA `uint8` (`_engine.pyi:1085, 1857`).
  - `ModelInputTensorKernel.compile(layout="nhwc", dtype="float32", scale=1/255,
    mean=0, std=1)` produces ComfyUI's IMAGE layout `[1,H,W,3]` float32 0..1 on the GPU in one
    pass (`streamlib/model_input_tensor_kernel.py:47-50, 421-440`).
  - A node publishes a tensor downstream through `acquire_storage_buffer_from_processor_output_pool`
    (`_engine.pyi:1116-1145`). [V]
  - What remains is ComfyUI's CPU intermediate device (§2). Unless the host runs with
    `gpu_only`, each frame pays a device → host copy anyway. [I]
- **Ports carry no types.** Ports are schema-free: "nothing a port declares and nothing
  `connect` compares" (`ARCHITECTURE.md:956-962`, DECIDED). What a port reports about its bags
  is **OPEN** (`ARCHITECTURE.md:1057-1064`). The node catalog serves each class's description,
  config JSON Schema and ports (`GLOSSARY.md` "Node catalog"; `ARCHITECTURE.md:983-1003`). [V]
- **Reaching a port from outside.**
  - "Every exposed port is reachable by URL from any tool" (`ARCHITECTURE.md:4105-4108`,
    DECIDED).
  - The grammar `/<stream>/<node>/<port>/<form>` and the forms `ndjson`/`png`/`ts`/… are
    **OPEN**, a direction only (`:4109-4120`).
  - An exposed port is an output (`GLOSSARY.md` "Exposed port"). The plan has no form for
    writing into a stream from outside.
  - Today, on the same machine, the local API's `tap` returns bags, and `exchange` turns a
    surface id into image bytes (`docs/decisions/control-plane-pixel-exchange.md`). [V]

## Direction 1 options (ComfyUI → streamlib)

| | A: one ComfyUI node → one Tatolab node | B: one Tatolab node hosts a workflow subgraph | C: talk to a running ComfyUI server |
|---|---|---|---|
| Mechanism | Generate a `@node` per class: import it, map its inputs to bags and tensor surfaces, call `FUNCTION`/`execute` per frame | The node's setup builds an embedded executor, and each frame re-queues the API-format prompt with injected input and collected output (comfystream's shape) | `POST /prompt` with an uploaded frame, then read the result through `/history` + `/view` or the websocket |
| Works for | Pure tensor or scalar nodes (filters, masks, compositing, audio DSP) | Any workflow, including loader → sampler chains, since MODEL objects stay inside one interpreter | Any workflow, one shot |
| Breaks on | MODEL/CLIP/VAE/CONDITIONING across processes; lazy inputs, expansion, hidden inputs, list semantics; every node pays torch + comfy import + its own CUDA context and model manager | Arbitrary workflows are not realtime; OUTPUT_NODEs that save files need rewriting (comfystream rewrites Save/Preview) | Latency: queue + PNG upload/download per frame; ComfyUI's single worker; a server the user runs |
| Latency per frame | Lowest per op, but a process hop per node | One process hop in and out, plus CPU intermediates | Highest: HTTP + encode/decode |
| Isolation | Per node | One stream venv for ComfyUI, linked to the rest | Full, separate program |
| Dependency conflicts | ComfyUI's environment in every node's venv | Confined to the ComfyUI stream's venv (`ARCHITECTURE.md:1424-1426`) | None in Tatolab |
| GPL exposure | `comfy` imported beside `tatolab.stream` in each node | The same, in one node | Sockets only; the FSF FAQ's "normally separate programs" case |

**Option A** is viable only as a narrow fast path for pure-tensor V3 nodes, and only if
importing them is acceptable. It is not a general translator.

**Option B** is the shape prior art has proven. Its setup and per-frame split answers the
question directly:

- **Setup** builds the executor and runs or caches the loaders.
- **Per frame** is the frame-dependent tail.

Both happen without Tatolab knowing anything about ComfyUI's types: only the IMAGE in and the
IMAGE out cross the node's ports. [I]

**Option C** suits slow, occasional generation from a frame. It is not per-frame.

**Installing a ComfyUI pack into a Tatolab project would require:**

- **ComfyUI itself.** Either a source checkout on `sys.path`, since upstream is not
  pip-installable, or the third-party hiddenswitch `comfyui` distribution.
- The pack's `requirements.txt` and its `install.py`. The second runs arbitrary code, which uv
  does not do.
- A torch build matching both ComfyUI and the pack.
- A model directory that `folder_paths` resolves.
- A `PromptServer` stub for the packs that touch it at import.

Under the plan, all of that lives in one stream's venv. [V]/[I]

## Direction 2 options (streamlib → ComfyUI)

- **A: a "Tatolab port" pack. This is the feasible one.**
  - **Read side.** A V1 or V3 node with an `INT`/`STRING` widget for the port's URL:
    1. `execute` fetches `.../png` and decodes it.
    2. It returns `float32 /255 [1,H,W,3]`, matching `LoadImage` (`CU nodes.py:1792-1793`).
    3. It returns `NaN` from `IS_CHANGED`, so every queue re-fetches.
    4. The frontend's instant auto-queue then makes it run continuously.
    5. A V3 `Combo` with `RemoteOptions(route=…)` can list ports from a listing URL
       (`CU _io.py:47-71`).
    - `ndjson` gives scalars and strings.
    - Precedent for "a node that is a thin client to an external service" exists in core:
      `comfy_api_nodes/` holds 45 entries.
  - **Write side.** A node that sends an IMAGE into a stream needs a Tatolab ingest path
    (an input form, or a source node with an HTTP endpoint). The plan has neither (§streamlib
    evidence).
  - **Needs from Tatolab:** the URL grammar and the `png`/`ndjson` forms decided and built,
    plus a decision on ingest.
- **B: generate ComfyUI nodes from Tatolab's node catalog. Possible, poor fit.**
  - It is possible: the async `comfy_entrypoint` can fetch the catalog at load and return
    classes built with `type()` (`CU nodes.py:2314-2336`).
  - It is a poor fit for three reasons:
    1. Tatolab ports have no types, so every socket would be `*` (`io.AnyType`), and
       ComfyUI's connection checking would be lost.
    2. A Tatolab node is continuous and lives in a stream. Running it once per ComfyUI prompt
       needs a verb to "instantiate this node, feed one input, return one output", and none
       exists.
    3. The local API is reachable only on its own machine (`GLOSSARY.md` "Local API").
  - Not worth pursuing before the bag-shape **OPEN** closes. [I]

## Licence (facts only; this needs counsel)

**The licences in play:**

- **ComfyUI** is GPL-3.0. `LICENSE` is the GPLv3 text, and `pyproject.toml` points at it
  (`CU LICENSE`, `CU pyproject.toml:5`). ComfyUI-Manager and comfy-cli are GPL-3.0 as well.
  pyisolate and comfystream are MIT. The hiddenswitch fork is GPL-3.0-or-later. [V]
- **Pack licences vary.**

  | Licence | Sampled packs |
  |---|---|
  | GPL-3.0 | Impact-Pack, KJNodes, VideoHelperSuite |
  | MIT | Custom-Scripts, essentials, rgthree |
  | Apache-2.0 | AnimateDiff-Evolved, controlnet_aux |

  The Registry's `license` field is optional (`DOCS registry/specifications.mdx`). [P]
- **StreamLib is BUSL-1.1**, with an Additional Use Grant that restricts some production use
  (`LICENSE:9-85`). [V]

**What GPLv3 and the FSF say:**

- GPLv3 §10: "You may not impose any further restrictions on the exercise of the rights
  granted". §5 covers aggregates (`CU LICENSE:239-243, 463`). [V]
- **When a program and its plug-ins are one.** FSF FAQ `#GPLPlugins`: "Using shared memory to
  communicate with complex data structures is pretty much equivalent to dynamic linking." [V]
- **Separate programs.** FSF FAQ `#MereAggregation`: "pipes, sockets and command-line
  arguments are communication mechanisms normally used between two separate programs", with a
  caveat for "intimate" semantics.
- **Arm's length.** `#GPLInProprietarySystem`: GPL software can sit alongside a proprietary
  system if the two "communicate at arms length". (https://www.gnu.org/licenses/gpl-faq.html) [V]

**Where the question arises:**

1. **Directions 1A and 1B.** One processor interpreter imports `comfy` (GPL) and
   `tatolab.stream` (BUSL) into one address space. It also trades frames with the runtime over
   shared memory and surface ids.
   - Several things may change the answer: whether Tatolab conveys any ComfyUI code (shipping
     it, or a pack that bundles it), whether only the user installs it, and whether the host
     node's own code is GPL.
   - That analysis is for counsel. [I]
2. **Direction 2A.** The ComfyUI pack imports `comfy` and is loaded into ComfyUI's process.
   Under the FSF's view that makes it part of ComfyUI, so the pack would most likely be
   GPL-3.0 and distributed on its own. Its link to Tatolab is HTTP, which is the FAQ's
   "separate programs" mechanism. [I]
3. **Direction 1C** needs no import of GPL code. [I]

## What needs `/align`

- **Whether Tatolab takes on ComfyUI interop at all.** If it does, which direction comes
  first, and whether direction 1 takes Option B's shape.
- **Where a ComfyUI host node would live.** A first-party extension, a third-party pack, or
  neither. Each placement carries different licence terms.
- **A way to write into a stream from outside.** Today an exposed port is output-only.
- **The URL grammar and forms OPEN** (`ARCHITECTURE.md:4109`). Direction 2A depends on it.
- **The bag-shape OPEN** (`ARCHITECTURE.md:1057`). Direction 2B depends on it.

## What remains unknown

- **Counsel's reading of both GPL boundaries**, especially a user-installed `comfy` inside a
  BUSL processor interpreter.
- **Whether core ComfyUI will ever be pip-installable**, or whether the `pyisolate-support`
  branch will merge into master. If it merges, ComfyUI's model objects get official RPC
  proxies, but that does not make them shareable with Tatolab.
- **How many registry packs import `PromptServer` at module level, or need `install.py`.**
  The sample is eight packs, not the 5,817.
- **The real per-frame cost of Option B in a processor interpreter.** This needs measuring:
  - re-queue overhead,
  - CPU intermediates against `gpu_only`,
  - one CUDA context for the host node.
  comfystream publishes no number for this that we could verify.
- **Whether any useful set of packs is realtime without TensorRT or StreamDiffusion
  rewrites.**
- **Why the registry's comfy-node metadata is often empty**, and whether the backfill
  (`POST /comfy-nodes/backfill`) works by running the node. Without that answer, the registry
  is not a dependable catalog to translate from.
