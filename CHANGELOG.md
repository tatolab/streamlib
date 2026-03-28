# Changelog

## [0.4.9](https://github.com/tatolab/streamlib/compare/v0.4.8...v0.4.9) (2026-03-28)


### Bug Fixes

* drain RTCP on RTP senders to unblock interceptor pipeline ([c59624a](https://github.com/tatolab/streamlib/commit/c59624a8b8ab7273a9fd383cdec9f058bfcfa574))

## [0.4.8](https://github.com/tatolab/streamlib/compare/v0.4.7...v0.4.8) (2026-03-28)


### Features

* Vulkan Video H.264/H.265 encoder — zero-copy GPU encoding ([#207](https://github.com/tatolab/streamlib/issues/207)) ([67e5659](https://github.com/tatolab/streamlib/commit/67e5659d7bf0ee2df0bf676dd29dae0e26f0edfc))
* widen WebRTC WHIP/WHEP and RTP to cross-platform ([#197](https://github.com/tatolab/streamlib/issues/197)) ([3ae9f45](https://github.com/tatolab/streamlib/commit/3ae9f451c20447e70356710ec93d7d11273b0e6a))


### Performance

* optimize Vulkan Video encode pipeline — 25fps → 50fps ([#207](https://github.com/tatolab/streamlib/issues/207)) ([c15ce8f](https://github.com/tatolab/streamlib/commit/c15ce8f85a32fec07504c62cfca62166a5529e0a))

## [0.4.7](https://github.com/tatolab/streamlib/compare/v0.4.6...v0.4.7) (2026-03-26)


### Features

* widen CLAP plugin host to cross-platform ([#198](https://github.com/tatolab/streamlib/issues/198)) ([909a11b](https://github.com/tatolab/streamlib/commit/909a11bbfca0501af933a879774f5716635db0f4))

## [0.4.6](https://github.com/tatolab/streamlib/compare/v0.4.5...v0.4.6) (2026-03-25)


### Features

* Phase 6 — timeline semaphores + V4L2 DMABUF zero-copy ([#200](https://github.com/tatolab/streamlib/issues/200)) ([24db878](https://github.com/tatolab/streamlib/commit/24db878e60a8761efb3983f1480a29907e608a06))

## [0.4.5](https://github.com/tatolab/streamlib/compare/v0.4.4...v0.4.5) (2026-03-24)


### Features

* complete Phase 5 — Vulkan RHI sync fixes + GPU format converter ([f0889c2](https://github.com/tatolab/streamlib/commit/f0889c2379f23d8d35ffc4e82bc99891aa4ba1f4))
* Linux rendering perf phases 1-4 + partial phase 5 ([5c266c1](https://github.com/tatolab/streamlib/commit/5c266c1143d18bb844a89550195c33d8b768aad4))
* Linux rendering performance — GPU pipeline parity with macOS ([#200](https://github.com/tatolab/streamlib/issues/200)) ([d1c5205](https://github.com/tatolab/streamlib/commit/d1c5205ea2d55a31e9f76d03e7f22afc072cb1f1))

## [0.4.4](https://github.com/tatolab/streamlib/compare/v0.4.3...v0.4.4) (2026-03-22)


### Features

* add Linux processor implementations for audio capture, output, camera, and display ([f3962b2](https://github.com/tatolab/streamlib/commit/f3962b2b5901da60634cf8ed018011863c9d2d9b)), closes [#166](https://github.com/tatolab/streamlib/issues/166)
* Linux processors — audio capture, output, camera, display ([bf035ae](https://github.com/tatolab/streamlib/commit/bf035ae2590c3b1e4ea6d2647e0f4f6180d5f284))
* **linux:** implement V4L2 camera processor with NV12→BGRA conversion ([784f81e](https://github.com/tatolab/streamlib/commit/784f81e0d7cbb148fad01cb3c8dcbe008e4f893c))
* **linux:** implement Vulkan + winit display processor ([8748cfa](https://github.com/tatolab/streamlib/commit/8748cfa5da4ab8691ad3c57a3d387fa7394767a3))


### Bug Fixes

* **linux:** address code review issues in V4L2 camera processor ([e305462](https://github.com/tatolab/streamlib/commit/e305462f2fb0d50cb75e924f3b2b19534f6599ad))
* **linux:** address display processor review — use-after-free, MAILBOX fallback, swapchain placement ([76a57e1](https://github.com/tatolab/streamlib/commit/76a57e149ab96d9f8a55323bac97099e7179c6d6))
* **linux:** prevent writer fd from closing, breaking Ctrl+C signal handling ([7cdb479](https://github.com/tatolab/streamlib/commit/7cdb479eeaff8c8ab4e25b4ca24ce45191fd10d1))
* make ApiServerProcessor available on Linux ([6b0e0c5](https://github.com/tatolab/streamlib/commit/6b0e0c509f83099dd78901df1fceeff2b39fa9d6))
* use any_thread for winit event loop on Linux ([e5e207e](https://github.com/tatolab/streamlib/commit/e5e207e767373a585f018e855cc42b5477acb24e))
* use default camera instead of hardcoded macOS UUID ([5aaef28](https://github.com/tatolab/streamlib/commit/5aaef283c082591c1d2dd148ebbe089507914dae))
* use reasonable ALSA buffer size instead of device max ([6358fee](https://github.com/tatolab/streamlib/commit/6358fee3d5675145db39a8360e96e7b0394f1c40))
* use try_init() for tracing in all examples ([1148d45](https://github.com/tatolab/streamlib/commit/1148d457f1b09babd7ac08d8aa9bb427b0a787f5))

## [0.4.3](https://github.com/tatolab/streamlib/compare/v0.4.2...v0.4.3) (2026-03-22)


### Features

* FFmpeg H.264 encode/decode/mux for Linux ([#167](https://github.com/tatolab/streamlib/issues/167)) ([3080bd7](https://github.com/tatolab/streamlib/commit/3080bd7851397970ace1cd32fcfe781b94dc3bcb))
* implement DMA-BUF GPU memory sharing for Linux broker ([95cabdd](https://github.com/tatolab/streamlib/commit/95cabdd2381fa323435eddb8581490a544c99182))
* implement FFmpeg H.264 encode/decode/mux and VulkanFormatConverter ([b35f774](https://github.com/tatolab/streamlib/commit/b35f77404728cc099be17493dfa1aa375e28fd97))
* Linux broker backend — Unix sockets + DMA-BUF fd passing ([#164](https://github.com/tatolab/streamlib/issues/164)) ([2df653f](https://github.com/tatolab/streamlib/commit/2df653f648d80b05bed0504e969783acef4bfff7))
* Linux broker backend with Unix sockets + DMA-BUF fd passing ([#164](https://github.com/tatolab/streamlib/issues/164)) ([943e6b3](https://github.com/tatolab/streamlib/commit/943e6b3070648e2c9a672e21dbef0d65f8d16fab))
* replace hand-built EncodedAudioFrame with JTD schema-generated Encodedaudioframe ([dcfa250](https://github.com/tatolab/streamlib/commit/dcfa250d88560e0a2af96a46e3d82c03002a4d34)), closes [#190](https://github.com/tatolab/streamlib/issues/190)
* replace hand-built EncodedAudioFrame with JTD schema-generated type ([4dafc49](https://github.com/tatolab/streamlib/commit/4dafc493a3b479ff87ed4265d88036e692600602))
* widen macOS-only cfg gates for Linux runtime ([#192](https://github.com/tatolab/streamlib/issues/192)) ([d247f11](https://github.com/tatolab/streamlib/commit/d247f1156a0e404990a30d12e6c605b3a614cff3))
* widen macOS-only cfg gates for Linux runtime, telemetry, and codecs ([0ced224](https://github.com/tatolab/streamlib/commit/0ced2246095309160e6f2d2440806e31e9dd0fb0)), closes [#192](https://github.com/tatolab/streamlib/issues/192)


### Bug Fixes

* address critical review feedback on FFmpeg encoder/muxer ([17c9bd6](https://github.com/tatolab/streamlib/commit/17c9bd6ab75b37989c096dd7b5be2f721b1aa020))
* address P0/P1 review feedback on unix socket service ([f42530c](https://github.com/tatolab/streamlib/commit/f42530c90aee3f89f5d46d05aa4c5a61d7b6a92a))
* address P0/P1/P2 review feedback on DMA-BUF memory sharing ([f56fce7](https://github.com/tatolab/streamlib/commit/f56fce732edd0b0fea913687935aae0d1090dd2b))
* replace Cell with OnceLock for cached DMA-BUF fd (soundness fix) ([83b03fa](https://github.com/tatolab/streamlib/commit/83b03fa177ba4ff56c84220949c4b32e3d5ecce8))
* resolve 5 FFmpeg compilation errors in encoder and muxer ([b39efa8](https://github.com/tatolab/streamlib/commit/b39efa8338eccceb2e4a4e91f327bcb351150926))

## [0.4.2](https://github.com/tatolab/streamlib/compare/v0.4.1...v0.4.2) (2026-03-21)


### Features

* gpu-allocator integration for Vulkan sub-allocation ([c91d814](https://github.com/tatolab/streamlib/commit/c91d8148b9d24b02d8e0d89c030b11b68eb0783c))
* Linux platform services — audio clock + thread priority ([347a782](https://github.com/tatolab/streamlib/commit/347a782e6a22c571d636a3276561e78c3f6878c9))
* make PixelFormat enum cross-platform ([bf22190](https://github.com/tatolab/streamlib/commit/bf2219037c48b21a39768f20e37c9ed1b87ac596))

## [0.4.1](https://github.com/tatolab/streamlib/compare/v0.4.0...v0.4.1) (2026-03-21)


### Features

* complete Vulkan RHI integration for Linux GPU pipeline ([ed22dad](https://github.com/tatolab/streamlib/commit/ed22dad11ab2290fe943d800ea3c61e3442cbe81))

## [0.4.0](https://github.com/tatolab/streamlib/compare/v0.3.11...v0.4.0) (2026-03-21)


### ⚠ BREAKING CHANGES

* Old LinkInput<T>/LinkOutput<T> API replaced with InputMailboxes/OutputWriter using iceoryx2 zero-copy shared memory.
* **audio:** CLAP is now a required dependency, not optional
* Major architectural refactor to separate framework from implementations

### Features

* Add adaptive audio output with dynamic SDP-based decoder configuration ([003c8dc](https://github.com/tatolab/streamlib/commit/003c8dcdb4957206b5d6c584f714709ca3535a52))
* Add ApiServerProcessor with REST API for runtime control ([#94](https://github.com/tatolab/streamlib/issues/94)) ([0b85ba8](https://github.com/tatolab/streamlib/commit/0b85ba8e22afe7e790c76970a57a76e1f9756b77))
* Add audio/video synchronization primitives ([ca987bd](https://github.com/tatolab/streamlib/commit/ca987bdfd99aa881d7b54df35188f612c291567e))
* Add AudioRequirements for declarative audio configuration ([8f9b7a6](https://github.com/tatolab/streamlib/commit/8f9b7a6c9791ab466b4513ae3010f5062129bdd0))
* add backward compatibility flag for gradual processor migration ([12e1954](https://github.com/tatolab/streamlib/commit/12e1954acb5493dd868f3096cbea1fb2ebe7003c))
* Add BUSL-1.1 licensing with commercial and partner options ([#73](https://github.com/tatolab/streamlib/issues/73)) ([1ed6c80](https://github.com/tatolab/streamlib/commit/1ed6c80c7e1c8c9a18768697bbea0499e7c4e6bf))
* Add camera-to-display pipeline with decorator API ([39339cd](https://github.com/tatolab/streamlib/commit/39339cd41be1cc8ccee45dd634b09b0bb273ae23))
* Add complete audio integration with GPU-accelerated processing ([e487c24](https://github.com/tatolab/streamlib/commit/e487c244b35adca41769cf9d450cf36d0d31bd66))
* Add comprehensive CLAP audio plugin support with parameter automation ([14dcbb8](https://github.com/tatolab/streamlib/commit/14dcbb805af3aee5031518b56d9836ee0ad6fc0c))
* Add comprehensive diagnostic logging to audio capture ([d793eaa](https://github.com/tatolab/streamlib/commit/d793eaa9d66747a51a9e7ddc5331fe643722bf83))
* Add CVDisplayLink vsync support for display processor ([cb532d6](https://github.com/tatolab/streamlib/commit/cb532d6850c3785f25f1d9585693e17e4ec77753))
* Add DataFrame schema system with derive macro support ([#102](https://github.com/tatolab/streamlib/issues/102)) ([dc92070](https://github.com/tatolab/streamlib/commit/dc9207071f016bbafd373e45800b0115bb2e42b8))
* Add dispatcher inference and function handlers ([9100c99](https://github.com/tatolab/streamlib/commit/9100c99c977d47b1c7839ce5744b330b23605f14))
* Add foundation types for processor registry system ([5feaf08](https://github.com/tatolab/streamlib/commit/5feaf08032d20f7186ca56fff7738f264b509a6e))
* Add FPS counter to display system and bouncing ball example ([3f68600](https://github.com/tatolab/streamlib/commit/3f686009794c38d61827cdaabbbc7fdb3c8e27cc))
* Add GPU acceleration and Metal backend support ([b7cf376](https://github.com/tatolab/streamlib/commit/b7cf376c3e4f01d8d5162be2a4d3e7fb7eb0ee5e))
* Add GPU-accelerated camera capture and handler updates ([fa0d090](https://github.com/tatolab/streamlib/commit/fa0d0902661b8c74e76fe5e544aeea4cb9ba9356))
* Add GPU-accelerated display output with zero-copy rendering ([d06e8d3](https://github.com/tatolab/streamlib/commit/d06e8d30ca31580329407a506043640c89cfefd8))
* Add GPU-accurate performance monitoring with Metal timestamp queries ([62b6916](https://github.com/tatolab/streamlib/commit/62b691609688831d0933a904c10ec39852786667))
* add Grafana + Tempo + Loki docker-compose for local telemetry visualization ([4c4a58c](https://github.com/tatolab/streamlib/commit/4c4a58c46d9a50ed43871144e559c22546d85d39))
* Add graph query interface traits (design only) ([#74](https://github.com/tatolab/streamlib/issues/74)) ([9daecd4](https://github.com/tatolab/streamlib/commit/9daecd4231351b2849d459ac9f3a8877d8e957a4))
* Add HTTP transport and improve MCP resource format ([1691141](https://github.com/tatolab/streamlib/commit/169114171f00906acdc8c7a07458846c822e14d1))
* Add JSON Schema generation for API endpoints ([#105](https://github.com/tatolab/streamlib/issues/105)) ([1b4eeb7](https://github.com/tatolab/streamlib/commit/1b4eeb7ffb750dd6cab9fea3a02260b08af87c6f))
* Add MCP runtime integration for AI agent control ([2b58ccd](https://github.com/tatolab/streamlib/commit/2b58ccd0680ef585d458f3096b02a536a547a332))
* Add MCP server with HTTP transport and enhanced AI discovery ([bc82a61](https://github.com/tatolab/streamlib/commit/bc82a61eeac60c9ba9ed759787c64af50a5a980a))
* Add Metal-native display and runtime extensions ([2633324](https://github.com/tatolab/streamlib/commit/2633324e33f50b858d6334a98f640fdd1ffa37d5))
* Add MP4 writer processor with A/V sync demonstration ([2f0ed97](https://github.com/tatolab/streamlib/commit/2f0ed9732ebad182c75989c4103b5b6fbc288309))
* Add processor descriptor inspector example ([abad52c](https://github.com/tatolab/streamlib/commit/abad52c7b443c752d00a403d54f96b85f1f1f697))
* Add ProcessorRegistry for dynamic processor registration ([bdbbdb9](https://github.com/tatolab/streamlib/commit/bdbbdb9661263ce1c5f0e1ab88534ab54397d361))
* Add Python bindings for event bus with verified delivery ([0dec7ce](https://github.com/tatolab/streamlib/commit/0dec7ce17cb38dd87b86527717519a5d3d172601))
* Add Python event bus bindings and fix processor lifecycle methods ([0b50c04](https://github.com/tatolab/streamlib/commit/0b50c043bc58d98864f63fd1777385304d2a6b24))
* Add Python processor support via PyO3 ([#104](https://github.com/tatolab/streamlib/issues/104)) ([e20e169](https://github.com/tatolab/streamlib/commit/e20e1695732cb5c15647f7062d04f1446a56862f))
* Add Python support and MCP integration for AI agent control ([867ad6c](https://github.com/tatolab/streamlib/commit/867ad6c18ea0f01d5fb916e9e619e59866aee936))
* Add real-time ML object detection with CoreML and GPU rendering ([98b8d34](https://github.com/tatolab/streamlib/commit/98b8d34702c082dd451fe9840497570b34e36fe7))
* Add request_camera() and request_microphone() methods to StreamRuntime ([e651ef9](https://github.com/tatolab/streamlib/commit/e651ef948da126c8448a64edbf8ef3c60895b71c))
* Add RGBA/BGRA color space handling in display processor with Metal shader swizzling ([896ddec](https://github.com/tatolab/streamlib/commit/896ddec0dcac2ab873396d108d5cdefa94a365ab))
* Add runtime.disconnect() with comprehensive event system ([#61](https://github.com/tatolab/streamlib/issues/61)) ([88ec59a](https://github.com/tatolab/streamlib/commit/88ec59adbbfc49b6ae009a9c7904a2a2dd334186))
* Add sample_rate to AudioFrame and create BufferRechunkerProcessor ([2ac7995](https://github.com/tatolab/streamlib/commit/2ac79955f7df1237e3154230dca0c7a230d0fd89))
* Add schema-based processor discovery system for AI agent integration ([8940936](https://github.com/tatolab/streamlib/commit/894093692fa79ec20a95c6f0e4395d8dd1b066ac))
* Add streamlib-mcp crate for AI agent integration ([9019768](https://github.com/tatolab/streamlib/commit/9019768d66bc7c632aaf0d02285fbb87f99a47d2))
* add tracing span instrumentation to runtime/compiler lifecycle ([98d0de6](https://github.com/tatolab/streamlib/commit/98d0de6e3799707081642979ed143ce87323af00))
* Add WebGPU-first architecture with platform-agnostic facade ([afba8b7](https://github.com/tatolab/streamlib/commit/afba8b75c800897c4eb9c9fa0e89a4b60360fd27))
* Add WebSocket event streaming to ApiServerProcessor ([#95](https://github.com/tatolab/streamlib/issues/95)) ([ab3af17](https://github.com/tatolab/streamlib/commit/ab3af1709f1f4192f3ba215cac7c98859b00b5d3))
* Add WHEP (WebRTC HTTP Egress Protocol) support with VideoToolbox H.264 decoding ([5df2844](https://github.com/tatolab/streamlib/commit/5df2844c9f693d539a1521120bd3570f9e05d7df))
* **audio:** Add frame tolerance to AudioMixer for timing jitter ([e8ab622](https://github.com/tatolab/streamlib/commit/e8ab622c2c8beda04e2cf6a482ee2ca1f3dd6877))
* **audio:** Complete audio foundation with SCHEMA_AUDIO_FRAME ([e2113ef](https://github.com/tatolab/streamlib/commit/e2113ef5a00a83c714260717deb1ea5c552b86e7))
* **audio:** Implement AudioMixerProcessor with GStreamer-style Pull mode architecture ([b89305c](https://github.com/tatolab/streamlib/commit/b89305c352fff7464ec6310c74da7e44e45b2c03))
* **audio:** Implement Pull mode pattern for AudioOutput with synchronized buffer sizes ([d73ee33](https://github.com/tatolab/streamlib/commit/d73ee334666eb3c42804eb89f2ca18e514e500db))
* **audio:** Implement timer groups and AudioMixer improvements ([86aa735](https://github.com/tatolab/streamlib/commit/86aa735561d5ab9b97a8de9e5ed4e1bb92c15cf8))
* **audio:** Make CLAP a required core dependency like wgpu ([483d6b4](https://github.com/tatolab/streamlib/commit/483d6b4ffa8f5075e747593eaa1143064e3c896d))
* **broker:** forward ingested telemetry to OTLP endpoint ([182fdee](https://github.com/tatolab/streamlib/commit/182fdeee0060b737a091ead9b2ad913d509c0cc0))
* Channels Architecture for Multi-Platform Processors ([#127](https://github.com/tatolab/streamlib/issues/127)) ([a5e0c89](https://github.com/tatolab/streamlib/commit/a5e0c8966ebd04c4f1c29b6792a1f70157649434))
* **clap:** Add plugin index loading and improve error messages ([8d8deba](https://github.com/tatolab/streamlib/commit/8d8deba905e93186789e9fa2ed4ebafbbe805bfb))
* CLI, Vulkan RHI, and Cross-Platform Codec Abstractions ([#112](https://github.com/tatolab/streamlib/issues/112)) ([415caaa](https://github.com/tatolab/streamlib/commit/415caaa58f411e8f1223b253ba17445924ec60c2))
* **cli:** add `streamlib telemetry export` command for OTLP backfill ([2d0fe5f](https://github.com/tatolab/streamlib/commit/2d0fe5f17a054fecf597a9869dba413c389256f1))
* Complete CLAP audio pipeline with type-safe connections ([d1f41ec](https://github.com/tatolab/streamlib/commit/d1f41ecc135e7b81a5aaf6f287e81924f44dc061))
* Complete event-driven migration and remove legacy fps field ([2bbbe37](https://github.com/tatolab/streamlib/commit/2bbbe37cd5acb66975bd1a0b038aa60def0f462d))
* Complete Rust migration with zero-copy GPU pipeline ([78dcb21](https://github.com/tatolab/streamlib/commit/78dcb2170051df235fe6e3e46f8e2cc97bb939e5))
* Complete sync runtime migration and add thread priority support ([f379e50](https://github.com/tatolab/streamlib/commit/f379e50e623ac06ddd09aff643ba3c2d4ea1079c))
* Consolidate all crates into unified streamlib architecture ([f0bc8c5](https://github.com/tatolab/streamlib/commit/f0bc8c5ce640ce2c93b81cfe406da9a87f93dd11))
* **core:** Complete v3.0 GStreamer-style architecture (Phases 6-9) ([3dbba76](https://github.com/tatolab/streamlib/commit/3dbba763f531683887b630405d7c3a4dbabbc95b))
* **core:** Implement v2.0.0 GStreamer-inspired trait architecture (Phases 1-7) ([8ade768](https://github.com/tatolab/streamlib/commit/8ade76852c0bbb291aa7af2d3cb1304ea4ae5645))
* **core:** Phase 8 Step 1 - Add RuntimeContext and update StreamElement ([83f4328](https://github.com/tatolab/streamlib/commit/83f4328d9d893b35e308f5b529007e75d6ea3a4b))
* **core:** Phase 8 Step 2 - Add DynStreamElement trait definition ([4b0af8f](https://github.com/tatolab/streamlib/commit/4b0af8faf0e8030593117dc39b51754239dcbed7))
* Deno/TypeScript subprocess processors with jtd-codegen unification ([#133](https://github.com/tatolab/streamlib/issues/133)) ([dcab28f](https://github.com/tatolab/streamlib/commit/dcab28fc31f6f2a7d03287fa148137e82850c5fa))
* DMA-BUF external memory support for Vulkan textures on Linux ([b68066a](https://github.com/tatolab/streamlib/commit/b68066a03879648b103225e2eb1151dcefcd40c5))
* Dynamic plugin loading and broker infrastructure ([#125](https://github.com/tatolab/streamlib/issues/125)) ([b9b2f75](https://github.com/tatolab/streamlib/commit/b9b2f75877470a7c18e9f08dbd19649c48a442d2))
* Dynamic processor creation with string-based API ([#80](https://github.com/tatolab/streamlib/issues/80)) ([6df4467](https://github.com/tatolab/streamlib/commit/6df446719b4f0504307a1ba931ceab4d6f42a5f4))
* Enable APPLICATION MODE in MCP server binary ([d4e3f32](https://github.com/tatolab/streamlib/commit/d4e3f32468337a3785a03a3fc1f4820fb9e5f600))
* Enhance MP4 writer with real AVAssetWriter integration ([2b1c04b](https://github.com/tatolab/streamlib/commit/2b1c04b1412f835671d952103ea57bca985341e2))
* Enhance processor descriptors with config schema and OpenAPI docs ([#103](https://github.com/tatolab/streamlib/issues/103)) ([a3b8619](https://github.com/tatolab/streamlib/commit/a3b861977188b0a5cf257b1eafdc7b25f0e352d2))
* Establish docs-first architecture with auto-generated SDK reference ([c08c105](https://github.com/tatolab/streamlib/commit/c08c1052a2e571b83a46580108a5fa0441812f93))
* Event Bus Implementation with Python Bindings ([f1761fc](https://github.com/tatolab/streamlib/commit/f1761fc04c644c91274468cb8a25773269644b51))
* Export wgpu enums from Rust, eliminate wgpu-py dependency ([f6b7b1d](https://github.com/tatolab/streamlib/commit/f6b7b1dfbeef4716bba67d83db2a61aa3bc6d9c8))
* hash-based venv caching for Python subprocess processors ([#131](https://github.com/tatolab/streamlib/issues/131)) ([#138](https://github.com/tatolab/streamlib/issues/138)) ([85d0d7a](https://github.com/tatolab/streamlib/commit/85d0d7a545f18bbd289abd594bb5297ba8a8f015))
* implement complete trait generation for StreamProcessor macro ([a3abe0f](https://github.com/tatolab/streamlib/commit/a3abe0f97b281fd67ed8e5b48a47b061ff691154))
* Implement core streamlib-core modules in Rust ([2bd6903](https://github.com/tatolab/streamlib/commit/2bd69037a0c1f9e53d1887e6e9a030bdfbb2a7fd))
* Implement GPU-accelerated RGBA→NV12 conversion with VTPixelTransferSession ([7f615b6](https://github.com/tatolab/streamlib/commit/7f615b6ca37fe430164d63bf7edec1e86431a246))
* Implement graceful processor shutdown (Phase 3) ([1c43de5](https://github.com/tatolab/streamlib/commit/1c43de536dec99cab449049258c9582bb14217d0))
* Implement GraphOptimizer Phase 0 with ergonomic API and comprehensive testing ([#63](https://github.com/tatolab/streamlib/issues/63)) ([c2c8929](https://github.com/tatolab/streamlib/commit/c2c8929f4af6df22173dcce0fae65e59b86c2710))
* Implement MCP server with stdio transport and auto-registration ([7640df9](https://github.com/tatolab/streamlib/commit/7640df9cfff35e81a6cf23d84e962dd050eaf644))
* Implement Metal/IOSurface zero-copy GPU pipeline for macOS ([0778d39](https://github.com/tatolab/streamlib/commit/0778d39256de96ba36ab507b675e24ffba54e85e))
* Implement monotonic clock-based A/V synchronization foundation ([19ff15e](https://github.com/tatolab/streamlib/commit/19ff15e9a3c6310bee9bfc0bf82b742ca4443206))
* Implement monotonic clock-based audio/video synchronization ([8b15a6e](https://github.com/tatolab/streamlib/commit/8b15a6e7c61dc864c41e3cdd3a522568183a1d28))
* Implement multi-input compositor with GPU tensor caching ([98b56ac](https://github.com/tatolab/streamlib/commit/98b56ac38d901f5c48059732a25e0566dc190ef5))
* Implement Opus audio encoding for WebRTC streaming ([fce9cb8](https://github.com/tatolab/streamlib/commit/fce9cb82e52f4a38e4baab374e3be79c8cfa5a31))
* Implement Opus audio encoding for WebRTC streaming (Phase 2) ([16e1b7f](https://github.com/tatolab/streamlib/commit/16e1b7fe74e4ca845cb0be33ef0e3d407af769ca))
* Implement Phase 1 - Processor Registry System ([c85fdec](https://github.com/tatolab/streamlib/commit/c85fdec0b66e99104e96b3c0afe2697d22d16afc))
* Implement Phase 2 - Connection Registry System ([7b4f04c](https://github.com/tatolab/streamlib/commit/7b4f04cf3dceb162c3aa0f0147fd6e4fdf89a8e7))
* Implement Phase 3.1 - Core Infrastructure (StreamHandler + Runtime) ([bedfe89](https://github.com/tatolab/streamlib/commit/bedfe89372be52a2ba00a6223d8d52ec04cadd2c))
* Implement Phase 3.2 - Basic Handlers (TestPattern + Display) ([4c51d1a](https://github.com/tatolab/streamlib/commit/4c51d1a08812420c2c49f14ce1249f3db11843b3))
* Implement Phase 3.3 - Advanced Handlers ([4a5a6fd](https://github.com/tatolab/streamlib/commit/4a5a6fd150e76374e5300e9fe3b13dc49cd72ad3))
* Implement Phase 3.4 - GPU Support ([60d018f](https://github.com/tatolab/streamlib/commit/60d018f0379406cc41bff5787d4931547f913efd))
* Implement pure Metal pipeline with zero-copy compositing ([cce545b](https://github.com/tatolab/streamlib/commit/cce545b2a518afa5b91cf54256cd8a6e8b0cdeab))
* Implement Python processor GPU wrapper system for zero-copy texture sharing ([4991754](https://github.com/tatolab/streamlib/commit/499175463f29cc91e9b17c94b0ef42f039a986a7))
* Implement RFC 002 Event Bus with lock-free pub/sub architecture ([39f91f0](https://github.com/tatolab/streamlib/commit/39f91f01e434f0aeb5f9b57653a02f6d42b2964b))
* Implement runtime processor addition (Phase 4) ([d0529d3](https://github.com/tatolab/streamlib/commit/d0529d375fc9dfaa4e29da68ef73dd5811de9ada))
* Implement shared GPU context architecture for zero-copy texture sharing ([ed767f1](https://github.com/tatolab/streamlib/commit/ed767f1c37d78ba72f3e18756f3730f11cb74c0b))
* Implement true zero-copy camera capture pipeline for macOS ([d5fe271](https://github.com/tatolab/streamlib/commit/d5fe2710a6ca38ff8c5268b0e0faab6ba141b164))
* Implement unified graceful shutdown for macOS with event bus integration ([d089c8e](https://github.com/tatolab/streamlib/commit/d089c8e41aa26cf25091497d8ee83abf0baf9272))
* Implement Vello 2D graphics compositing with camera feed ([7f562ed](https://github.com/tatolab/streamlib/commit/7f562ed36152c2c6646dd186d7f29a522a73f633))
* Implement VideoToolbox H.264 encoder for WebRTC streaming ([bd7545e](https://github.com/tatolab/streamlib/commit/bd7545ebe47bab23ed5da72820fd7d7a1ef02db1))
* Implement VideoToolbox H.264 encoder for WebRTC streaming (Phase 1) ([6159cb7](https://github.com/tatolab/streamlib/commit/6159cb7eeaaa0a05da70b47ac8bd15d46cd98047))
* Improve error handling in wgpu parser functions ([23c6cb0](https://github.com/tatolab/streamlib/commit/23c6cb087c007df9522138aa3528f215ce39957a))
* Initialize Rust workspace for streamlib core ([23bcdfc](https://github.com/tatolab/streamlib/commit/23bcdfc6c31f43471abcfb246b1354ca806aa461))
* integrate telemetry into StreamRuntime — every runtime gets unified logging ([d6ae795](https://github.com/tatolab/streamlib/commit/d6ae79550919361c47734872af4503a92956d455))
* Inventory-based auto-registration for processors ([#78](https://github.com/tatolab/streamlib/issues/78)) ([60a92d8](https://github.com/tatolab/streamlib/commit/60a92d86971b834e47a9fbb97b92d9e1275dd88f))
* MCP Python execution, processor documentation, and dual-session fix ([16b01b3](https://github.com/tatolab/streamlib/commit/16b01b378f04daf144b2becebed8dfc54d3b805f))
* Migrate AudioFrame from compile-time generic to runtime enum-based architecture ([#59](https://github.com/tatolab/streamlib/issues/59)) ([a56801f](https://github.com/tatolab/streamlib/commit/a56801fae69633b4a09eeb78d94e0e87c006d321))
* Publish ProcessorAdded and ProcessorRemoved events to event bus ([f15cd40](https://github.com/tatolab/streamlib/commit/f15cd406ae4eaa4f638ff3a9fefcb7f5613a2650))
* Python native FFI subprocess + overlay flickering fixes ([#155](https://github.com/tatolab/streamlib/issues/155)) ([077f98f](https://github.com/tatolab/streamlib/commit/077f98f662453d76c32081e60bd73ee7ee87b4cb))
* Python subprocess processors with iceoryx2 IPC ([#130](https://github.com/tatolab/streamlib/issues/130)) ([f4664fc](https://github.com/tatolab/streamlib/commit/f4664fcc4b44b84074b068ef4093b785d3eeabb7))
* **python:** Add Rust-like field marker API matching macro ergonomics ([43e54bb](https://github.com/tatolab/streamlib/commit/43e54bb17a8ab339f5efd0ac8823b2d02a0ada4d))
* Redesign Python API to match Rust patterns and implement port connections ([1b14efe](https://github.com/tatolab/streamlib/commit/1b14efef0732d0d8b793a9abeae8eb3ff75baee6))
* Refactor GPU wrappers to use Arc for automatic memory management ([dceb9f3](https://github.com/tatolab/streamlib/commit/dceb9f3333e69f7bea66a34db4d17dddc5794180))
* Refactor runtime to GStreamer-style synchronous architecture ([a5913a9](https://github.com/tatolab/streamlib/commit/a5913a91ce301df224466e601442b4fc2dddfca1))
* Register platform processors with factory functions for MCP ([9c9512a](https://github.com/tatolab/streamlib/commit/9c9512abe7441d9985909c17828088ba145694ac))
* Rename lifecycle methods to setup()/teardown() per RFC 001 ([092986b](https://github.com/tatolab/streamlib/commit/092986b051a245186f55e6e7fccbfa86e772f11f))
* Rename lifecycle methods to setup()/teardown() per RFC 001 ([c819230](https://github.com/tatolab/streamlib/commit/c81923085562fbf9fc73bfd70c40f8f78ce06a47))
* Reorganize examples as standalone projects and improve Python testing ([da2ce67](https://github.com/tatolab/streamlib/commit/da2ce674329be9141c469f8d33818776fbdb848c))
* Reorganize examples as standalone projects and improve Python testing ([2ee6873](https://github.com/tatolab/streamlib/commit/2ee6873b17ac2da735881ff4cbf8cef738b35734))
* Rust dylib plugin loading + camera-rust-plugin example ([#148](https://github.com/tatolab/streamlib/issues/148)) ([e09a4f3](https://github.com/tatolab/streamlib/commit/e09a4f3f86c8f6dde40780bf0b28ee2456413061))
* Schema registry, pkg CLI, schemas CLI ([#136](https://github.com/tatolab/streamlib/issues/136) Phase 4) ([#141](https://github.com/tatolab/streamlib/issues/141)) ([d4ab458](https://github.com/tatolab/streamlib/commit/d4ab458734f20bbe63d8b57590df8d9877f7eab1))
* span instrumentation + Python subprocess stderr forwarding ([c547491](https://github.com/tatolab/streamlib/commit/c547491c1d419936ac05422989ebc5f85bcb8035))
* Support StreamRuntime integration with existing tokio runtimes ([#96](https://github.com/tatolab/streamlib/issues/96)) ([8236ace](https://github.com/tatolab/streamlib/commit/8236ace6e5a6691ed7fcb5b6792f815e2b5de4fa))
* top-level `streamlib logs` and `streamlib spans` commands with --follow ([6b19534](https://github.com/tatolab/streamlib/commit/6b19534941542eacbbbe0119b66aaf1d15fd03b3))
* Unified Graph API with Gremlin-style traversals and ECS components ([#75](https://github.com/tatolab/streamlib/issues/75)) ([8b439de](https://github.com/tatolab/streamlib/commit/8b439de588cf0552cbdc2d02589d162a65ee9d81))
* unified OpenTelemetry observability with broker-as-collector ([5ec1ab4](https://github.com/tatolab/streamlib/commit/5ec1ab474ef57c2da7121d86399888048964ca9f))
* unified OpenTelemetry observability with broker-as-collector architecture ([1878be3](https://github.com/tatolab/streamlib/commit/1878be35e37c25f56f70ec5c3565d5151abb5078))
* Unify connection system to support any processor type at runtime ([222272d](https://github.com/tatolab/streamlib/commit/222272d27d329dea558795704c650e1ebc1a2eb1))
* unify processor schema into streamlib.yaml ([#150](https://github.com/tatolab/streamlib/issues/150)) ([#151](https://github.com/tatolab/streamlib/issues/151)) ([36f89d4](https://github.com/tatolab/streamlib/commit/36f89d4c77830068fe5f91aa2958b0f8d8af1c04))
* **videotoolbox:** Implement VideoToolboxDecoder for WHEP playback ([164fe1e](https://github.com/tatolab/streamlib/commit/164fe1e1c845df0dfa6372446d4d49f8b7083f24))
* Vulkan blitter — RhiBlitter for Linux ([fcb462f](https://github.com/tatolab/streamlib/commit/fcb462f61a39cf820a475a1af1fa711e4e216f84))
* Vulkan blitter — RhiBlitter implementation for Linux ([4d13dd9](https://github.com/tatolab/streamlib/commit/4d13dd93dd510efb3629543585fbd8a8fe27bd61))
* Vulkan pixel buffer — CPU-visible staging buffer for Linux ([7b4f8f5](https://github.com/tatolab/streamlib/commit/7b4f8f5fc5a7c39888865cf85b010c71378df4db))
* Vulkan pixel buffer pool + format converter for Linux ([2637208](https://github.com/tatolab/streamlib/commit/263720847106186f82c6e0155ef99ff0690d0065))
* Vulkan RHI — complete GPU backend for Linux ([#163](https://github.com/tatolab/streamlib/issues/163)) ([f2072ed](https://github.com/tatolab/streamlib/commit/f2072edb5700ff4ba35bba7d82b51b524e68dcf9))
* Vulkan texture cache — VkImageView caching for Linux ([00a1ca7](https://github.com/tatolab/streamlib/commit/00a1ca7cffc775ee1aa94e91d31eb3f91f67d313))
* **webrtc:** Add WHEP (WebRTC egress) foundation components ([de86796](https://github.com/tatolab/streamlib/commit/de86796188eb7d028938ec04ad4e49ea6509dc28))
* **webrtc:** Complete Phase 6 - StreamProcessor integration and example ([c540bdd](https://github.com/tatolab/streamlib/commit/c540bdd37b4bff4227b6d1797bdf6cedb6cf6800))
* **webrtc:** Implement Phase 3 RTP packetization with pollster integration ([539d81a](https://github.com/tatolab/streamlib/commit/539d81a1871d970bbb99c86c0016f5ca1b7ed75b))
* **webrtc:** Implement Phase 3 RTP Packetization with pollster Integration ([abcd18e](https://github.com/tatolab/streamlib/commit/abcd18e77bd7bd8d31467e0354f934f788ac5135))
* **webrtc:** Implement Phase 4 WHIP signaling with Cloudflare Stream support ([28231d6](https://github.com/tatolab/streamlib/commit/28231d6ffb07ebb9bc74fca5894bac4a641849e3))
* **webrtc:** Implement Phase 4 WHIP signaling with Cloudflare Stream support ([7459f3e](https://github.com/tatolab/streamlib/commit/7459f3ebe3cecf59dd6508794bcf284c22f6081c))
* Zero-copy Python-Rust GPU pipeline with OpenGL interop and timing API ([#106](https://github.com/tatolab/streamlib/issues/106)) ([1e390c5](https://github.com/tatolab/streamlib/commit/1e390c5656476d2d5194c60dff65b3a9f7dffc42))


### Bug Fixes

* Add cfg guards to apple-specific RTP video conversion ([#79](https://github.com/tatolab/streamlib/issues/79)) ([59a6a81](https://github.com/tatolab/streamlib/commit/59a6a81b25f46825351ea74bfdc3d93b68c9be7f))
* **broker:** Add copyright header to generated proto file ([188129b](https://github.com/tatolab/streamlib/commit/188129bfc168f232abc5cd4b3a5eddfcff927b8a))
* **ci:** Add packages:write permission for release workflow ([893ba45](https://github.com/tatolab/streamlib/commit/893ba45a43ccdb8aebbe7e59565a535c39e4a051))
* **ci:** Fix json-schema-to-zod CLI flag (-s -&gt; -i) ([28ba213](https://github.com/tatolab/streamlib/commit/28ba213d4cb98292ee2cf0c1655bc38b9298ed47))
* **ci:** Fix schema generation cache key and add validation ([8b2ae44](https://github.com/tatolab/streamlib/commit/8b2ae447cec3f05c5cf20beaf5b240c746c9aa7b))
* **ci:** Use simple release type for workspace Cargo.toml ([a50656e](https://github.com/tatolab/streamlib/commit/a50656e6a4ea9d9eab66c4b0c7a0ef7d252a5539))
* **clap:** Update clack_host imports for API compatibility ([23d1437](https://github.com/tatolab/streamlib/commit/23d14378f00dc9200a889d2dbcc56a42520d690c))
* Complete zero-copy camera pipeline with IOSurface → Metal → WebGPU ([451ca76](https://github.com/tatolab/streamlib/commit/451ca7611278bcc13e2d76e6be75787c529e8b15))
* **core:** Complete v2.0 AudioFrame API migration and remove stereo hardcoding ([02b3580](https://github.com/tatolab/streamlib/commit/02b358026d71ab93d193ad780837f3fec8d38952))
* correct macro API to match streamlib implementation ([15b63b1](https://github.com/tatolab/streamlib/commit/15b63b1bc4ff21754d9063a294362822c6aa30ac))
* Correct resampler input chunk size calculation ([ec0f61f](https://github.com/tatolab/streamlib/commit/ec0f61fae807ff311708191ba35d4274e22d9b11))
* Fix audio capture by requesting microphone permissions before use ([724f2a3](https://github.com/tatolab/streamlib/commit/724f2a317e003748cd0b1b4e1f3e4c629ff763ed))
* Fix memory leaks in VideoToolboxH264Encoder Drop implementation ([559715e](https://github.com/tatolab/streamlib/commit/559715efe027e3e807ee16af788ada5037c66735))
* gate macOS-only code for Linux compilation ([2bd1711](https://github.com/tatolab/streamlib/commit/2bd1711bbd2da8d1180ed407669399d65d904b5a))
* gate macOS-only code for Linux compilation ([2918696](https://github.com/tatolab/streamlib/commit/291869649ef45b60b5580cdccc3dc29cdfec019a))
* Handle both bundle and binary paths in CLAP plugin loader ([ca23ecf](https://github.com/tatolab/streamlib/commit/ca23ecfeb7707da2a9eecc7041af55195a939acd))
* init Tokio runtime before telemetry in StreamRuntime::new() ([b2ed4ac](https://github.com/tatolab/streamlib/commit/b2ed4ac163f7eea9aab314cf8072888de07330f0))
* Initialize camera on main thread using async dispatch in Pull mode ([4b002f5](https://github.com/tatolab/streamlib/commit/4b002f5f1d28569fe5353743710e84c27a788b68))
* load .env in StreamRuntime::new() before broker port resolution ([a82f4a1](https://github.com/tatolab/streamlib/commit/a82f4a1580d94f7dbd4684f7276550098b63629e))
* Proper graceful shutdown for ManualProcessor lifecycle ([#89](https://github.com/tatolab/streamlib/issues/89)) ([7e49f01](https://github.com/tatolab/streamlib/commit/7e49f01553a9e8a85878ba75f6fca1dea8366e71))
* PubSub publisher lifetime bug + comprehensive test suite ([#157](https://github.com/tatolab/streamlib/issues/157)) ([bf33f17](https://github.com/tatolab/streamlib/commit/bf33f17db6f4089638acabf1adafea4a2fdce8f0))
* Refactor runtime to synchronous architecture with vsync and main thread camera support ([6875b80](https://github.com/tatolab/streamlib/commit/6875b802ad6fd146d7000f5020f31dec2f7cda25))
* **tests:** Update tests for RuntimeContext parameter ([7caed9b](https://github.com/tatolab/streamlib/commit/7caed9b67dfda86f4cb2131e9abf97858f8a0c9d))
* Track CVDisplayLink context pointer for proper cleanup ([c7ccec0](https://github.com/tatolab/streamlib/commit/c7ccec013389f58c5a5b46f0b7de715c1c1ee4d1))
* Update pyproject.toml to reference libs/streamlib ([d46517e](https://github.com/tatolab/streamlib/commit/d46517e405e83e9d87f6bc4addcaa3d02c192626))
* use raw format for MemoryPropertyFlags in error message ([420dd21](https://github.com/tatolab/streamlib/commit/420dd21ae09dc59c38638c00ac891c827833aa5d))
* Use tokio::sync::Mutex for MCP runtime access ([135503f](https://github.com/tatolab/streamlib/commit/135503f437902477724cdcf596e3924ba5407033))
* Vulkan memory type selection — replace hardcoded index 0 ([c324065](https://github.com/tatolab/streamlib/commit/c32406544aa3eb5774d0b3d45967ec560a325e0c))
* WebRTC client lifecycle and proper session cleanup ([#90](https://github.com/tatolab/streamlib/issues/90)) ([6d8d099](https://github.com/tatolab/streamlib/commit/6d8d099cc8f780ab8148ef9218e610c0812108d6))
* **webrtc:** Add SPS/PPS extraction and AVCC to Annex B conversion for H.264 streaming ([c0b3420](https://github.com/tatolab/streamlib/commit/c0b3420aaecef66a582e44b0949ec960a1f7ff93))
* **webrtc:** Fix AVCC double-wrapping and transceiver configuration bugs ([792f8e1](https://github.com/tatolab/streamlib/commit/792f8e1f056dcc1cf025601d0eddc99b5cc213cb))
* **webrtc:** Fix H.264 keyframe detection using CMSampleBuffer attachments ([ba24151](https://github.com/tatolab/streamlib/commit/ba24151560866aa02f27f623d1d631bf667aba46))
* **webrtc:** Fix H.264 keyframe detection using CMSampleBuffer attachments ([ef01295](https://github.com/tatolab/streamlib/commit/ef012953495613690c30f2c0863713da87129371))
* **webrtc:** Fix segmentation fault by dispatching VideoToolbox creation to main thread ([348bd20](https://github.com/tatolab/streamlib/commit/348bd209acba8fe8bddaee7c5f1d07717917eca2))
* **webrtc:** Fix WebRtcWhepProcessor compilation errors ([7aa3b81](https://github.com/tatolab/streamlib/commit/7aa3b815d2747569e5b5069b8514dddc3571bfa8))


### Performance

* Add comprehensive event bus benchmarks with realistic workloads ([e51d335](https://github.com/tatolab/streamlib/commit/e51d33511ad7003b32a1bafc32f90e8986c66d03))
* Remove Mutex from hot read/write paths in ports ([7746661](https://github.com/tatolab/streamlib/commit/7746661005ab29fae4803d3c68b456f6717facd6))


### Code Refactoring

* Split into minimal core SDK + example implementations ([aa6a3fc](https://github.com/tatolab/streamlib/commit/aa6a3fca3fdab2b6a364c9fe06d3749ce4c6347b))

## [0.3.11](https://github.com/tatolab/streamlib/compare/v0.3.10...v0.3.11) (2026-03-21)


### Bug Fixes

* gate macOS-only code for Linux compilation ([2bd1711](https://github.com/tatolab/streamlib/commit/2bd1711bbd2da8d1180ed407669399d65d904b5a))
* gate macOS-only code for Linux compilation ([2918696](https://github.com/tatolab/streamlib/commit/291869649ef45b60b5580cdccc3dc29cdfec019a))

## [0.3.10](https://github.com/tatolab/streamlib/compare/v0.3.9...v0.3.10) (2026-03-21)


### Features

* Vulkan RHI — complete GPU backend for Linux ([#163](https://github.com/tatolab/streamlib/issues/163)) ([f2072ed](https://github.com/tatolab/streamlib/commit/f2072edb5700ff4ba35bba7d82b51b524e68dcf9))

## [0.3.9](https://github.com/tatolab/streamlib/compare/v0.3.8...v0.3.9) (2026-03-20)


### Features

* add Grafana + Tempo + Loki docker-compose for local telemetry visualization ([4c4a58c](https://github.com/tatolab/streamlib/commit/4c4a58c46d9a50ed43871144e559c22546d85d39))
* add tracing span instrumentation to runtime/compiler lifecycle ([98d0de6](https://github.com/tatolab/streamlib/commit/98d0de6e3799707081642979ed143ce87323af00))
* **broker:** forward ingested telemetry to OTLP endpoint ([182fdee](https://github.com/tatolab/streamlib/commit/182fdeee0060b737a091ead9b2ad913d509c0cc0))
* **cli:** add `streamlib telemetry export` command for OTLP backfill ([2d0fe5f](https://github.com/tatolab/streamlib/commit/2d0fe5f17a054fecf597a9869dba413c389256f1))
* integrate telemetry into StreamRuntime — every runtime gets unified logging ([d6ae795](https://github.com/tatolab/streamlib/commit/d6ae79550919361c47734872af4503a92956d455))
* span instrumentation + Python subprocess stderr forwarding ([c547491](https://github.com/tatolab/streamlib/commit/c547491c1d419936ac05422989ebc5f85bcb8035))
* top-level `streamlib logs` and `streamlib spans` commands with --follow ([6b19534](https://github.com/tatolab/streamlib/commit/6b19534941542eacbbbe0119b66aaf1d15fd03b3))
* unified OpenTelemetry observability with broker-as-collector ([5ec1ab4](https://github.com/tatolab/streamlib/commit/5ec1ab474ef57c2da7121d86399888048964ca9f))
* unified OpenTelemetry observability with broker-as-collector architecture ([1878be3](https://github.com/tatolab/streamlib/commit/1878be35e37c25f56f70ec5c3565d5151abb5078))


### Bug Fixes

* init Tokio runtime before telemetry in StreamRuntime::new() ([b2ed4ac](https://github.com/tatolab/streamlib/commit/b2ed4ac163f7eea9aab314cf8072888de07330f0))
* load .env in StreamRuntime::new() before broker port resolution ([a82f4a1](https://github.com/tatolab/streamlib/commit/a82f4a1580d94f7dbd4684f7276550098b63629e))

## [0.3.8](https://github.com/tatolab/streamlib/compare/v0.3.7...v0.3.8) (2026-03-05)


### Bug Fixes

* PubSub publisher lifetime bug + comprehensive test suite ([#157](https://github.com/tatolab/streamlib/issues/157)) ([bf33f17](https://github.com/tatolab/streamlib/commit/bf33f17db6f4089638acabf1adafea4a2fdce8f0))

## [0.3.7](https://github.com/tatolab/streamlib/compare/v0.3.6...v0.3.7) (2026-03-01)


### Features

* Python native FFI subprocess + overlay flickering fixes ([#155](https://github.com/tatolab/streamlib/issues/155)) ([077f98f](https://github.com/tatolab/streamlib/commit/077f98f662453d76c32081e60bd73ee7ee87b4cb))

## [0.3.6](https://github.com/tatolab/streamlib/compare/v0.3.5...v0.3.6) (2026-02-10)


### Features

* unify processor schema into streamlib.yaml ([#150](https://github.com/tatolab/streamlib/issues/150)) ([#151](https://github.com/tatolab/streamlib/issues/151)) ([36f89d4](https://github.com/tatolab/streamlib/commit/36f89d4c77830068fe5f91aa2958b0f8d8af1c04))

## [0.3.5](https://github.com/tatolab/streamlib/compare/v0.3.4...v0.3.5) (2026-02-10)


### Features

* Rust dylib plugin loading + camera-rust-plugin example ([#148](https://github.com/tatolab/streamlib/issues/148)) ([e09a4f3](https://github.com/tatolab/streamlib/commit/e09a4f3f86c8f6dde40780bf0b28ee2456413061))

## [0.3.4](https://github.com/tatolab/streamlib/compare/v0.3.3...v0.3.4) (2026-02-07)


### Features

* Schema registry, pkg CLI, schemas CLI ([#136](https://github.com/tatolab/streamlib/issues/136) Phase 4) ([#141](https://github.com/tatolab/streamlib/issues/141)) ([d4ab458](https://github.com/tatolab/streamlib/commit/d4ab458734f20bbe63d8b57590df8d9877f7eab1))

## [0.3.3](https://github.com/tatolab/streamlib/compare/v0.3.2...v0.3.3) (2026-02-01)


### Features

* hash-based venv caching for Python subprocess processors ([#131](https://github.com/tatolab/streamlib/issues/131)) ([#138](https://github.com/tatolab/streamlib/issues/138)) ([85d0d7a](https://github.com/tatolab/streamlib/commit/85d0d7a545f18bbd289abd594bb5297ba8a8f015))

## [0.3.2](https://github.com/tatolab/streamlib/compare/v0.3.1...v0.3.2) (2026-02-01)


### Features

* Deno/TypeScript subprocess processors with jtd-codegen unification ([#133](https://github.com/tatolab/streamlib/issues/133)) ([dcab28f](https://github.com/tatolab/streamlib/commit/dcab28fc31f6f2a7d03287fa148137e82850c5fa))

## [0.3.1](https://github.com/tatolab/streamlib/compare/v0.3.0...v0.3.1) (2026-01-31)


### Features

* Python subprocess processors with iceoryx2 IPC ([#130](https://github.com/tatolab/streamlib/issues/130)) ([f4664fc](https://github.com/tatolab/streamlib/commit/f4664fcc4b44b84074b068ef4093b785d3eeabb7))

## [0.3.0](https://github.com/tatolab/streamlib/compare/v0.2.5...v0.3.0) (2026-01-23)


### ⚠ BREAKING CHANGES

* Old LinkInput<T>/LinkOutput<T> API replaced with InputMailboxes/OutputWriter using iceoryx2 zero-copy shared memory.

### Features

* Channels Architecture for Multi-Platform Processors ([#127](https://github.com/tatolab/streamlib/issues/127)) ([a5e0c89](https://github.com/tatolab/streamlib/commit/a5e0c8966ebd04c4f1c29b6792a1f70157649434))

## [0.2.5](https://github.com/tatolab/streamlib/compare/v0.2.4...v0.2.5) (2026-01-17)


### Features

* Dynamic plugin loading and broker infrastructure ([#125](https://github.com/tatolab/streamlib/issues/125)) ([b9b2f75](https://github.com/tatolab/streamlib/commit/b9b2f75877470a7c18e9f08dbd19649c48a442d2))


### Bug Fixes

* **broker:** Add copyright header to generated proto file ([188129b](https://github.com/tatolab/streamlib/commit/188129bfc168f232abc5cd4b3a5eddfcff927b8a))

## [0.2.4](https://github.com/tatolab/streamlib/compare/v0.2.3...v0.2.4) (2026-01-10)


### Features

* CLI, Vulkan RHI, and Cross-Platform Codec Abstractions ([#112](https://github.com/tatolab/streamlib/issues/112)) ([415caaa](https://github.com/tatolab/streamlib/commit/415caaa58f411e8f1223b253ba17445924ec60c2))

## [0.2.3](https://github.com/tatolab/streamlib/compare/v0.2.2...v0.2.3) (2026-01-08)


### Bug Fixes

* **ci:** Fix json-schema-to-zod CLI flag (-s -&gt; -i) ([28ba213](https://github.com/tatolab/streamlib/commit/28ba213d4cb98292ee2cf0c1655bc38b9298ed47))

## [0.2.2](https://github.com/tatolab/streamlib/compare/v0.2.1...v0.2.2) (2026-01-08)


### Bug Fixes

* **ci:** Fix schema generation cache key and add validation ([8b2ae44](https://github.com/tatolab/streamlib/commit/8b2ae447cec3f05c5cf20beaf5b240c746c9aa7b))

## [0.2.1](https://github.com/tatolab/streamlib/compare/v0.2.0...v0.2.1) (2026-01-08)


### Bug Fixes

* **clap:** Update clack_host imports for API compatibility ([23d1437](https://github.com/tatolab/streamlib/commit/23d14378f00dc9200a889d2dbcc56a42520d690c))

## [0.2.0](https://github.com/tatolab/streamlib/compare/v0.1.0...v0.2.0) (2026-01-08)


### ⚠ BREAKING CHANGES

* **audio:** CLAP is now a required dependency, not optional
* Major architectural refactor to separate framework from implementations

### Features

* Add adaptive audio output with dynamic SDP-based decoder configuration ([003c8dc](https://github.com/tatolab/streamlib/commit/003c8dcdb4957206b5d6c584f714709ca3535a52))
* Add ApiServerProcessor with REST API for runtime control ([#94](https://github.com/tatolab/streamlib/issues/94)) ([0b85ba8](https://github.com/tatolab/streamlib/commit/0b85ba8e22afe7e790c76970a57a76e1f9756b77))
* Add audio/video synchronization primitives ([ca987bd](https://github.com/tatolab/streamlib/commit/ca987bdfd99aa881d7b54df35188f612c291567e))
* Add AudioRequirements for declarative audio configuration ([8f9b7a6](https://github.com/tatolab/streamlib/commit/8f9b7a6c9791ab466b4513ae3010f5062129bdd0))
* add backward compatibility flag for gradual processor migration ([12e1954](https://github.com/tatolab/streamlib/commit/12e1954acb5493dd868f3096cbea1fb2ebe7003c))
* Add BUSL-1.1 licensing with commercial and partner options ([#73](https://github.com/tatolab/streamlib/issues/73)) ([1ed6c80](https://github.com/tatolab/streamlib/commit/1ed6c80c7e1c8c9a18768697bbea0499e7c4e6bf))
* Add camera-to-display pipeline with decorator API ([39339cd](https://github.com/tatolab/streamlib/commit/39339cd41be1cc8ccee45dd634b09b0bb273ae23))
* Add complete audio integration with GPU-accelerated processing ([e487c24](https://github.com/tatolab/streamlib/commit/e487c244b35adca41769cf9d450cf36d0d31bd66))
* Add comprehensive CLAP audio plugin support with parameter automation ([14dcbb8](https://github.com/tatolab/streamlib/commit/14dcbb805af3aee5031518b56d9836ee0ad6fc0c))
* Add comprehensive diagnostic logging to audio capture ([d793eaa](https://github.com/tatolab/streamlib/commit/d793eaa9d66747a51a9e7ddc5331fe643722bf83))
* Add CVDisplayLink vsync support for display processor ([cb532d6](https://github.com/tatolab/streamlib/commit/cb532d6850c3785f25f1d9585693e17e4ec77753))
* Add DataFrame schema system with derive macro support ([#102](https://github.com/tatolab/streamlib/issues/102)) ([dc92070](https://github.com/tatolab/streamlib/commit/dc9207071f016bbafd373e45800b0115bb2e42b8))
* Add dispatcher inference and function handlers ([9100c99](https://github.com/tatolab/streamlib/commit/9100c99c977d47b1c7839ce5744b330b23605f14))
* Add foundation types for processor registry system ([5feaf08](https://github.com/tatolab/streamlib/commit/5feaf08032d20f7186ca56fff7738f264b509a6e))
* Add FPS counter to display system and bouncing ball example ([3f68600](https://github.com/tatolab/streamlib/commit/3f686009794c38d61827cdaabbbc7fdb3c8e27cc))
* Add GPU acceleration and Metal backend support ([b7cf376](https://github.com/tatolab/streamlib/commit/b7cf376c3e4f01d8d5162be2a4d3e7fb7eb0ee5e))
* Add GPU-accelerated camera capture and handler updates ([fa0d090](https://github.com/tatolab/streamlib/commit/fa0d0902661b8c74e76fe5e544aeea4cb9ba9356))
* Add GPU-accelerated display output with zero-copy rendering ([d06e8d3](https://github.com/tatolab/streamlib/commit/d06e8d30ca31580329407a506043640c89cfefd8))
* Add GPU-accurate performance monitoring with Metal timestamp queries ([62b6916](https://github.com/tatolab/streamlib/commit/62b691609688831d0933a904c10ec39852786667))
* Add graph query interface traits (design only) ([#74](https://github.com/tatolab/streamlib/issues/74)) ([9daecd4](https://github.com/tatolab/streamlib/commit/9daecd4231351b2849d459ac9f3a8877d8e957a4))
* Add HTTP transport and improve MCP resource format ([1691141](https://github.com/tatolab/streamlib/commit/169114171f00906acdc8c7a07458846c822e14d1))
* Add JSON Schema generation for API endpoints ([#105](https://github.com/tatolab/streamlib/issues/105)) ([1b4eeb7](https://github.com/tatolab/streamlib/commit/1b4eeb7ffb750dd6cab9fea3a02260b08af87c6f))
* Add MCP runtime integration for AI agent control ([2b58ccd](https://github.com/tatolab/streamlib/commit/2b58ccd0680ef585d458f3096b02a536a547a332))
* Add MCP server with HTTP transport and enhanced AI discovery ([bc82a61](https://github.com/tatolab/streamlib/commit/bc82a61eeac60c9ba9ed759787c64af50a5a980a))
* Add Metal-native display and runtime extensions ([2633324](https://github.com/tatolab/streamlib/commit/2633324e33f50b858d6334a98f640fdd1ffa37d5))
* Add MP4 writer processor with A/V sync demonstration ([2f0ed97](https://github.com/tatolab/streamlib/commit/2f0ed9732ebad182c75989c4103b5b6fbc288309))
* Add processor descriptor inspector example ([abad52c](https://github.com/tatolab/streamlib/commit/abad52c7b443c752d00a403d54f96b85f1f1f697))
* Add ProcessorRegistry for dynamic processor registration ([bdbbdb9](https://github.com/tatolab/streamlib/commit/bdbbdb9661263ce1c5f0e1ab88534ab54397d361))
* Add Python bindings for event bus with verified delivery ([0dec7ce](https://github.com/tatolab/streamlib/commit/0dec7ce17cb38dd87b86527717519a5d3d172601))
* Add Python event bus bindings and fix processor lifecycle methods ([0b50c04](https://github.com/tatolab/streamlib/commit/0b50c043bc58d98864f63fd1777385304d2a6b24))
* Add Python processor support via PyO3 ([#104](https://github.com/tatolab/streamlib/issues/104)) ([e20e169](https://github.com/tatolab/streamlib/commit/e20e1695732cb5c15647f7062d04f1446a56862f))
* Add Python support and MCP integration for AI agent control ([867ad6c](https://github.com/tatolab/streamlib/commit/867ad6c18ea0f01d5fb916e9e619e59866aee936))
* Add real-time ML object detection with CoreML and GPU rendering ([98b8d34](https://github.com/tatolab/streamlib/commit/98b8d34702c082dd451fe9840497570b34e36fe7))
* Add request_camera() and request_microphone() methods to StreamRuntime ([e651ef9](https://github.com/tatolab/streamlib/commit/e651ef948da126c8448a64edbf8ef3c60895b71c))
* Add RGBA/BGRA color space handling in display processor with Metal shader swizzling ([896ddec](https://github.com/tatolab/streamlib/commit/896ddec0dcac2ab873396d108d5cdefa94a365ab))
* Add runtime.disconnect() with comprehensive event system ([#61](https://github.com/tatolab/streamlib/issues/61)) ([88ec59a](https://github.com/tatolab/streamlib/commit/88ec59adbbfc49b6ae009a9c7904a2a2dd334186))
* Add sample_rate to AudioFrame and create BufferRechunkerProcessor ([2ac7995](https://github.com/tatolab/streamlib/commit/2ac79955f7df1237e3154230dca0c7a230d0fd89))
* Add schema-based processor discovery system for AI agent integration ([8940936](https://github.com/tatolab/streamlib/commit/894093692fa79ec20a95c6f0e4395d8dd1b066ac))
* Add streamlib-mcp crate for AI agent integration ([9019768](https://github.com/tatolab/streamlib/commit/9019768d66bc7c632aaf0d02285fbb87f99a47d2))
* Add WebGPU-first architecture with platform-agnostic facade ([afba8b7](https://github.com/tatolab/streamlib/commit/afba8b75c800897c4eb9c9fa0e89a4b60360fd27))
* Add WebSocket event streaming to ApiServerProcessor ([#95](https://github.com/tatolab/streamlib/issues/95)) ([ab3af17](https://github.com/tatolab/streamlib/commit/ab3af1709f1f4192f3ba215cac7c98859b00b5d3))
* Add WHEP (WebRTC HTTP Egress Protocol) support with VideoToolbox H.264 decoding ([5df2844](https://github.com/tatolab/streamlib/commit/5df2844c9f693d539a1521120bd3570f9e05d7df))
* **audio:** Add frame tolerance to AudioMixer for timing jitter ([e8ab622](https://github.com/tatolab/streamlib/commit/e8ab622c2c8beda04e2cf6a482ee2ca1f3dd6877))
* **audio:** Complete audio foundation with SCHEMA_AUDIO_FRAME ([e2113ef](https://github.com/tatolab/streamlib/commit/e2113ef5a00a83c714260717deb1ea5c552b86e7))
* **audio:** Implement AudioMixerProcessor with GStreamer-style Pull mode architecture ([b89305c](https://github.com/tatolab/streamlib/commit/b89305c352fff7464ec6310c74da7e44e45b2c03))
* **audio:** Implement Pull mode pattern for AudioOutput with synchronized buffer sizes ([d73ee33](https://github.com/tatolab/streamlib/commit/d73ee334666eb3c42804eb89f2ca18e514e500db))
* **audio:** Implement timer groups and AudioMixer improvements ([86aa735](https://github.com/tatolab/streamlib/commit/86aa735561d5ab9b97a8de9e5ed4e1bb92c15cf8))
* **audio:** Make CLAP a required core dependency like wgpu ([483d6b4](https://github.com/tatolab/streamlib/commit/483d6b4ffa8f5075e747593eaa1143064e3c896d))
* **clap:** Add plugin index loading and improve error messages ([8d8deba](https://github.com/tatolab/streamlib/commit/8d8deba905e93186789e9fa2ed4ebafbbe805bfb))
* Complete CLAP audio pipeline with type-safe connections ([d1f41ec](https://github.com/tatolab/streamlib/commit/d1f41ecc135e7b81a5aaf6f287e81924f44dc061))
* Complete event-driven migration and remove legacy fps field ([2bbbe37](https://github.com/tatolab/streamlib/commit/2bbbe37cd5acb66975bd1a0b038aa60def0f462d))
* Complete Rust migration with zero-copy GPU pipeline ([78dcb21](https://github.com/tatolab/streamlib/commit/78dcb2170051df235fe6e3e46f8e2cc97bb939e5))
* Complete sync runtime migration and add thread priority support ([f379e50](https://github.com/tatolab/streamlib/commit/f379e50e623ac06ddd09aff643ba3c2d4ea1079c))
* Consolidate all crates into unified streamlib architecture ([f0bc8c5](https://github.com/tatolab/streamlib/commit/f0bc8c5ce640ce2c93b81cfe406da9a87f93dd11))
* **core:** Complete v3.0 GStreamer-style architecture (Phases 6-9) ([3dbba76](https://github.com/tatolab/streamlib/commit/3dbba763f531683887b630405d7c3a4dbabbc95b))
* **core:** Implement v2.0.0 GStreamer-inspired trait architecture (Phases 1-7) ([8ade768](https://github.com/tatolab/streamlib/commit/8ade76852c0bbb291aa7af2d3cb1304ea4ae5645))
* **core:** Phase 8 Step 1 - Add RuntimeContext and update StreamElement ([83f4328](https://github.com/tatolab/streamlib/commit/83f4328d9d893b35e308f5b529007e75d6ea3a4b))
* **core:** Phase 8 Step 2 - Add DynStreamElement trait definition ([4b0af8f](https://github.com/tatolab/streamlib/commit/4b0af8faf0e8030593117dc39b51754239dcbed7))
* Dynamic processor creation with string-based API ([#80](https://github.com/tatolab/streamlib/issues/80)) ([6df4467](https://github.com/tatolab/streamlib/commit/6df446719b4f0504307a1ba931ceab4d6f42a5f4))
* Enable APPLICATION MODE in MCP server binary ([d4e3f32](https://github.com/tatolab/streamlib/commit/d4e3f32468337a3785a03a3fc1f4820fb9e5f600))
* Enhance MP4 writer with real AVAssetWriter integration ([2b1c04b](https://github.com/tatolab/streamlib/commit/2b1c04b1412f835671d952103ea57bca985341e2))
* Enhance processor descriptors with config schema and OpenAPI docs ([#103](https://github.com/tatolab/streamlib/issues/103)) ([a3b8619](https://github.com/tatolab/streamlib/commit/a3b861977188b0a5cf257b1eafdc7b25f0e352d2))
* Establish docs-first architecture with auto-generated SDK reference ([c08c105](https://github.com/tatolab/streamlib/commit/c08c1052a2e571b83a46580108a5fa0441812f93))
* Event Bus Implementation with Python Bindings ([f1761fc](https://github.com/tatolab/streamlib/commit/f1761fc04c644c91274468cb8a25773269644b51))
* Export wgpu enums from Rust, eliminate wgpu-py dependency ([f6b7b1d](https://github.com/tatolab/streamlib/commit/f6b7b1dfbeef4716bba67d83db2a61aa3bc6d9c8))
* implement complete trait generation for StreamProcessor macro ([a3abe0f](https://github.com/tatolab/streamlib/commit/a3abe0f97b281fd67ed8e5b48a47b061ff691154))
* Implement core streamlib-core modules in Rust ([2bd6903](https://github.com/tatolab/streamlib/commit/2bd69037a0c1f9e53d1887e6e9a030bdfbb2a7fd))
* Implement GPU-accelerated RGBA→NV12 conversion with VTPixelTransferSession ([7f615b6](https://github.com/tatolab/streamlib/commit/7f615b6ca37fe430164d63bf7edec1e86431a246))
* Implement graceful processor shutdown (Phase 3) ([1c43de5](https://github.com/tatolab/streamlib/commit/1c43de536dec99cab449049258c9582bb14217d0))
* Implement GraphOptimizer Phase 0 with ergonomic API and comprehensive testing ([#63](https://github.com/tatolab/streamlib/issues/63)) ([c2c8929](https://github.com/tatolab/streamlib/commit/c2c8929f4af6df22173dcce0fae65e59b86c2710))
* Implement MCP server with stdio transport and auto-registration ([7640df9](https://github.com/tatolab/streamlib/commit/7640df9cfff35e81a6cf23d84e962dd050eaf644))
* Implement Metal/IOSurface zero-copy GPU pipeline for macOS ([0778d39](https://github.com/tatolab/streamlib/commit/0778d39256de96ba36ab507b675e24ffba54e85e))
* Implement monotonic clock-based A/V synchronization foundation ([19ff15e](https://github.com/tatolab/streamlib/commit/19ff15e9a3c6310bee9bfc0bf82b742ca4443206))
* Implement monotonic clock-based audio/video synchronization ([8b15a6e](https://github.com/tatolab/streamlib/commit/8b15a6e7c61dc864c41e3cdd3a522568183a1d28))
* Implement multi-input compositor with GPU tensor caching ([98b56ac](https://github.com/tatolab/streamlib/commit/98b56ac38d901f5c48059732a25e0566dc190ef5))
* Implement Opus audio encoding for WebRTC streaming ([fce9cb8](https://github.com/tatolab/streamlib/commit/fce9cb82e52f4a38e4baab374e3be79c8cfa5a31))
* Implement Opus audio encoding for WebRTC streaming (Phase 2) ([16e1b7f](https://github.com/tatolab/streamlib/commit/16e1b7fe74e4ca845cb0be33ef0e3d407af769ca))
* Implement Phase 1 - Processor Registry System ([c85fdec](https://github.com/tatolab/streamlib/commit/c85fdec0b66e99104e96b3c0afe2697d22d16afc))
* Implement Phase 2 - Connection Registry System ([7b4f04c](https://github.com/tatolab/streamlib/commit/7b4f04cf3dceb162c3aa0f0147fd6e4fdf89a8e7))
* Implement Phase 3.1 - Core Infrastructure (StreamHandler + Runtime) ([bedfe89](https://github.com/tatolab/streamlib/commit/bedfe89372be52a2ba00a6223d8d52ec04cadd2c))
* Implement Phase 3.2 - Basic Handlers (TestPattern + Display) ([4c51d1a](https://github.com/tatolab/streamlib/commit/4c51d1a08812420c2c49f14ce1249f3db11843b3))
* Implement Phase 3.3 - Advanced Handlers ([4a5a6fd](https://github.com/tatolab/streamlib/commit/4a5a6fd150e76374e5300e9fe3b13dc49cd72ad3))
* Implement Phase 3.4 - GPU Support ([60d018f](https://github.com/tatolab/streamlib/commit/60d018f0379406cc41bff5787d4931547f913efd))
* Implement pure Metal pipeline with zero-copy compositing ([cce545b](https://github.com/tatolab/streamlib/commit/cce545b2a518afa5b91cf54256cd8a6e8b0cdeab))
* Implement Python processor GPU wrapper system for zero-copy texture sharing ([4991754](https://github.com/tatolab/streamlib/commit/499175463f29cc91e9b17c94b0ef42f039a986a7))
* Implement RFC 002 Event Bus with lock-free pub/sub architecture ([39f91f0](https://github.com/tatolab/streamlib/commit/39f91f01e434f0aeb5f9b57653a02f6d42b2964b))
* Implement runtime processor addition (Phase 4) ([d0529d3](https://github.com/tatolab/streamlib/commit/d0529d375fc9dfaa4e29da68ef73dd5811de9ada))
* Implement shared GPU context architecture for zero-copy texture sharing ([ed767f1](https://github.com/tatolab/streamlib/commit/ed767f1c37d78ba72f3e18756f3730f11cb74c0b))
* Implement true zero-copy camera capture pipeline for macOS ([d5fe271](https://github.com/tatolab/streamlib/commit/d5fe2710a6ca38ff8c5268b0e0faab6ba141b164))
* Implement unified graceful shutdown for macOS with event bus integration ([d089c8e](https://github.com/tatolab/streamlib/commit/d089c8e41aa26cf25091497d8ee83abf0baf9272))
* Implement Vello 2D graphics compositing with camera feed ([7f562ed](https://github.com/tatolab/streamlib/commit/7f562ed36152c2c6646dd186d7f29a522a73f633))
* Implement VideoToolbox H.264 encoder for WebRTC streaming ([bd7545e](https://github.com/tatolab/streamlib/commit/bd7545ebe47bab23ed5da72820fd7d7a1ef02db1))
* Implement VideoToolbox H.264 encoder for WebRTC streaming (Phase 1) ([6159cb7](https://github.com/tatolab/streamlib/commit/6159cb7eeaaa0a05da70b47ac8bd15d46cd98047))
* Improve error handling in wgpu parser functions ([23c6cb0](https://github.com/tatolab/streamlib/commit/23c6cb087c007df9522138aa3528f215ce39957a))
* Initialize Rust workspace for streamlib core ([23bcdfc](https://github.com/tatolab/streamlib/commit/23bcdfc6c31f43471abcfb246b1354ca806aa461))
* Inventory-based auto-registration for processors ([#78](https://github.com/tatolab/streamlib/issues/78)) ([60a92d8](https://github.com/tatolab/streamlib/commit/60a92d86971b834e47a9fbb97b92d9e1275dd88f))
* MCP Python execution, processor documentation, and dual-session fix ([16b01b3](https://github.com/tatolab/streamlib/commit/16b01b378f04daf144b2becebed8dfc54d3b805f))
* Migrate AudioFrame from compile-time generic to runtime enum-based architecture ([#59](https://github.com/tatolab/streamlib/issues/59)) ([a56801f](https://github.com/tatolab/streamlib/commit/a56801fae69633b4a09eeb78d94e0e87c006d321))
* Publish ProcessorAdded and ProcessorRemoved events to event bus ([f15cd40](https://github.com/tatolab/streamlib/commit/f15cd406ae4eaa4f638ff3a9fefcb7f5613a2650))
* **python:** Add Rust-like field marker API matching macro ergonomics ([43e54bb](https://github.com/tatolab/streamlib/commit/43e54bb17a8ab339f5efd0ac8823b2d02a0ada4d))
* Redesign Python API to match Rust patterns and implement port connections ([1b14efe](https://github.com/tatolab/streamlib/commit/1b14efef0732d0d8b793a9abeae8eb3ff75baee6))
* Refactor GPU wrappers to use Arc for automatic memory management ([dceb9f3](https://github.com/tatolab/streamlib/commit/dceb9f3333e69f7bea66a34db4d17dddc5794180))
* Refactor runtime to GStreamer-style synchronous architecture ([a5913a9](https://github.com/tatolab/streamlib/commit/a5913a91ce301df224466e601442b4fc2dddfca1))
* Register platform processors with factory functions for MCP ([9c9512a](https://github.com/tatolab/streamlib/commit/9c9512abe7441d9985909c17828088ba145694ac))
* Rename lifecycle methods to setup()/teardown() per RFC 001 ([092986b](https://github.com/tatolab/streamlib/commit/092986b051a245186f55e6e7fccbfa86e772f11f))
* Rename lifecycle methods to setup()/teardown() per RFC 001 ([c819230](https://github.com/tatolab/streamlib/commit/c81923085562fbf9fc73bfd70c40f8f78ce06a47))
* Reorganize examples as standalone projects and improve Python testing ([da2ce67](https://github.com/tatolab/streamlib/commit/da2ce674329be9141c469f8d33818776fbdb848c))
* Reorganize examples as standalone projects and improve Python testing ([2ee6873](https://github.com/tatolab/streamlib/commit/2ee6873b17ac2da735881ff4cbf8cef738b35734))
* Support StreamRuntime integration with existing tokio runtimes ([#96](https://github.com/tatolab/streamlib/issues/96)) ([8236ace](https://github.com/tatolab/streamlib/commit/8236ace6e5a6691ed7fcb5b6792f815e2b5de4fa))
* Unified Graph API with Gremlin-style traversals and ECS components ([#75](https://github.com/tatolab/streamlib/issues/75)) ([8b439de](https://github.com/tatolab/streamlib/commit/8b439de588cf0552cbdc2d02589d162a65ee9d81))
* Unify connection system to support any processor type at runtime ([222272d](https://github.com/tatolab/streamlib/commit/222272d27d329dea558795704c650e1ebc1a2eb1))
* **videotoolbox:** Implement VideoToolboxDecoder for WHEP playback ([164fe1e](https://github.com/tatolab/streamlib/commit/164fe1e1c845df0dfa6372446d4d49f8b7083f24))
* **webrtc:** Add WHEP (WebRTC egress) foundation components ([de86796](https://github.com/tatolab/streamlib/commit/de86796188eb7d028938ec04ad4e49ea6509dc28))
* **webrtc:** Complete Phase 6 - StreamProcessor integration and example ([c540bdd](https://github.com/tatolab/streamlib/commit/c540bdd37b4bff4227b6d1797bdf6cedb6cf6800))
* **webrtc:** Implement Phase 3 RTP packetization with pollster integration ([539d81a](https://github.com/tatolab/streamlib/commit/539d81a1871d970bbb99c86c0016f5ca1b7ed75b))
* **webrtc:** Implement Phase 3 RTP Packetization with pollster Integration ([abcd18e](https://github.com/tatolab/streamlib/commit/abcd18e77bd7bd8d31467e0354f934f788ac5135))
* **webrtc:** Implement Phase 4 WHIP signaling with Cloudflare Stream support ([28231d6](https://github.com/tatolab/streamlib/commit/28231d6ffb07ebb9bc74fca5894bac4a641849e3))
* **webrtc:** Implement Phase 4 WHIP signaling with Cloudflare Stream support ([7459f3e](https://github.com/tatolab/streamlib/commit/7459f3ebe3cecf59dd6508794bcf284c22f6081c))
* Zero-copy Python-Rust GPU pipeline with OpenGL interop and timing API ([#106](https://github.com/tatolab/streamlib/issues/106)) ([1e390c5](https://github.com/tatolab/streamlib/commit/1e390c5656476d2d5194c60dff65b3a9f7dffc42))


### Bug Fixes

* Add cfg guards to apple-specific RTP video conversion ([#79](https://github.com/tatolab/streamlib/issues/79)) ([59a6a81](https://github.com/tatolab/streamlib/commit/59a6a81b25f46825351ea74bfdc3d93b68c9be7f))
* **ci:** Add packages:write permission for release workflow ([893ba45](https://github.com/tatolab/streamlib/commit/893ba45a43ccdb8aebbe7e59565a535c39e4a051))
* **ci:** Use simple release type for workspace Cargo.toml ([a50656e](https://github.com/tatolab/streamlib/commit/a50656e6a4ea9d9eab66c4b0c7a0ef7d252a5539))
* Complete zero-copy camera pipeline with IOSurface → Metal → WebGPU ([451ca76](https://github.com/tatolab/streamlib/commit/451ca7611278bcc13e2d76e6be75787c529e8b15))
* **core:** Complete v2.0 AudioFrame API migration and remove stereo hardcoding ([02b3580](https://github.com/tatolab/streamlib/commit/02b358026d71ab93d193ad780837f3fec8d38952))
* correct macro API to match streamlib implementation ([15b63b1](https://github.com/tatolab/streamlib/commit/15b63b1bc4ff21754d9063a294362822c6aa30ac))
* Correct resampler input chunk size calculation ([ec0f61f](https://github.com/tatolab/streamlib/commit/ec0f61fae807ff311708191ba35d4274e22d9b11))
* Fix audio capture by requesting microphone permissions before use ([724f2a3](https://github.com/tatolab/streamlib/commit/724f2a317e003748cd0b1b4e1f3e4c629ff763ed))
* Fix memory leaks in VideoToolboxH264Encoder Drop implementation ([559715e](https://github.com/tatolab/streamlib/commit/559715efe027e3e807ee16af788ada5037c66735))
* Handle both bundle and binary paths in CLAP plugin loader ([ca23ecf](https://github.com/tatolab/streamlib/commit/ca23ecfeb7707da2a9eecc7041af55195a939acd))
* Initialize camera on main thread using async dispatch in Pull mode ([4b002f5](https://github.com/tatolab/streamlib/commit/4b002f5f1d28569fe5353743710e84c27a788b68))
* Proper graceful shutdown for ManualProcessor lifecycle ([#89](https://github.com/tatolab/streamlib/issues/89)) ([7e49f01](https://github.com/tatolab/streamlib/commit/7e49f01553a9e8a85878ba75f6fca1dea8366e71))
* Refactor runtime to synchronous architecture with vsync and main thread camera support ([6875b80](https://github.com/tatolab/streamlib/commit/6875b802ad6fd146d7000f5020f31dec2f7cda25))
* **tests:** Update tests for RuntimeContext parameter ([7caed9b](https://github.com/tatolab/streamlib/commit/7caed9b67dfda86f4cb2131e9abf97858f8a0c9d))
* Track CVDisplayLink context pointer for proper cleanup ([c7ccec0](https://github.com/tatolab/streamlib/commit/c7ccec013389f58c5a5b46f0b7de715c1c1ee4d1))
* Update pyproject.toml to reference libs/streamlib ([d46517e](https://github.com/tatolab/streamlib/commit/d46517e405e83e9d87f6bc4addcaa3d02c192626))
* Use tokio::sync::Mutex for MCP runtime access ([135503f](https://github.com/tatolab/streamlib/commit/135503f437902477724cdcf596e3924ba5407033))
* WebRTC client lifecycle and proper session cleanup ([#90](https://github.com/tatolab/streamlib/issues/90)) ([6d8d099](https://github.com/tatolab/streamlib/commit/6d8d099cc8f780ab8148ef9218e610c0812108d6))
* **webrtc:** Add SPS/PPS extraction and AVCC to Annex B conversion for H.264 streaming ([c0b3420](https://github.com/tatolab/streamlib/commit/c0b3420aaecef66a582e44b0949ec960a1f7ff93))
* **webrtc:** Fix AVCC double-wrapping and transceiver configuration bugs ([792f8e1](https://github.com/tatolab/streamlib/commit/792f8e1f056dcc1cf025601d0eddc99b5cc213cb))
* **webrtc:** Fix H.264 keyframe detection using CMSampleBuffer attachments ([ba24151](https://github.com/tatolab/streamlib/commit/ba24151560866aa02f27f623d1d631bf667aba46))
* **webrtc:** Fix H.264 keyframe detection using CMSampleBuffer attachments ([ef01295](https://github.com/tatolab/streamlib/commit/ef012953495613690c30f2c0863713da87129371))
* **webrtc:** Fix segmentation fault by dispatching VideoToolbox creation to main thread ([348bd20](https://github.com/tatolab/streamlib/commit/348bd209acba8fe8bddaee7c5f1d07717917eca2))
* **webrtc:** Fix WebRtcWhepProcessor compilation errors ([7aa3b81](https://github.com/tatolab/streamlib/commit/7aa3b815d2747569e5b5069b8514dddc3571bfa8))


### Performance

* Add comprehensive event bus benchmarks with realistic workloads ([e51d335](https://github.com/tatolab/streamlib/commit/e51d33511ad7003b32a1bafc32f90e8986c66d03))
* Remove Mutex from hot read/write paths in ports ([7746661](https://github.com/tatolab/streamlib/commit/7746661005ab29fae4803d3c68b456f6717facd6))


### Code Refactoring

* Split into minimal core SDK + example implementations ([aa6a3fc](https://github.com/tatolab/streamlib/commit/aa6a3fca3fdab2b6a364c9fe06d3749ce4c6347b))
