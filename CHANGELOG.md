# Changelog

## [0.7.40](https://github.com/tatolab/streamlib/compare/v0.7.39...v0.7.40) (2026-07-22)


### Features

* **examples:** dynamic-reconfigure — live camera→display rewiring harness ([#340](https://github.com/tatolab/streamlib/issues/340)) ([#1500](https://github.com/tatolab/streamlib/issues/1500)) ([01c587d](https://github.com/tatolab/streamlib/commit/01c587de673122069bc2790ac74b39f4be02c466))

## [0.7.39](https://github.com/tatolab/streamlib/compare/v0.7.38...v0.7.39) (2026-07-22)


### Features

* **idents:** version never blocks a load — remove hard VersionRangeConflict stop ([#1546](https://github.com/tatolab/streamlib/issues/1546)) ([72632bd](https://github.com/tatolab/streamlib/commit/72632bd55aba540821ee7fb117b00855af2b39d7))

## [0.7.38](https://github.com/tatolab/streamlib/compare/v0.7.37...v0.7.38) (2026-07-22)


### Features

* **engine:** installed slot is load-only — typed InstalledPackageNotBuilt, never a cold-build ([#1544](https://github.com/tatolab/streamlib/issues/1544)) ([fb30cd1](https://github.com/tatolab/streamlib/commit/fb30cd12bd6eaf7936df41a36306ad3cb49a794d))

## [0.7.37](https://github.com/tatolab/streamlib/compare/v0.7.36...v0.7.37) (2026-07-22)


### Features

* **cli:** add/install compile packages on-the-box + fail-fast ([#1542](https://github.com/tatolab/streamlib/issues/1542)) ([51bd820](https://github.com/tatolab/streamlib/commit/51bd820581cdeb048cdff13f3d7f8b1e57c3c2d0))

## [0.7.36](https://github.com/tatolab/streamlib/compare/v0.7.35...v0.7.36) (2026-07-22)


### Features

* **engine:** retire .streamlib/cache/packages, fold packages.yaml into streamlib_modules ([#1540](https://github.com/tatolab/streamlib/issues/1540)) ([d344fce](https://github.com/tatolab/streamlib/commit/d344fce90450bf3938b073ddd9dedca4fd83c7b7))

## [0.7.35](https://github.com/tatolab/streamlib/compare/v0.7.34...v0.7.35) (2026-07-22)


### Features

* **engine:** flip installed-package slot to co-located streamlib_modules ([#1538](https://github.com/tatolab/streamlib/issues/1538)) ([235ee5d](https://github.com/tatolab/streamlib/commit/235ee5df0b277df08eb872cdaabbe847dcbc2d6a))

## [0.7.34](https://github.com/tatolab/streamlib/compare/v0.7.33...v0.7.34) (2026-07-22)


### Bug Fixes

* **engine:** dev sources build in-tree beside their source ([#1536](https://github.com/tatolab/streamlib/issues/1536)) ([2443a0e](https://github.com/tatolab/streamlib/commit/2443a0edc0c26d6b0fda45f1dfbb60337e16a447))

## [0.7.33](https://github.com/tatolab/streamlib/compare/v0.7.32...v0.7.33) (2026-07-22)


### Features

* **packages:** promote-outputs-last materialize for a source-dir destination ([#1534](https://github.com/tatolab/streamlib/issues/1534)) ([ebde93d](https://github.com/tatolab/streamlib/commit/ebde93d0ab173222099df4e03cc56f57fc05b6a5))

## [0.7.32](https://github.com/tatolab/streamlib/compare/v0.7.31...v0.7.32) (2026-07-22)


### Features

* **engine:** BuildRequest carries the engine-computed staging destination ([#1531](https://github.com/tatolab/streamlib/issues/1531)) ([2638edb](https://github.com/tatolab/streamlib/commit/2638edb2667f7c25e5c93f77012bee9ff341f334))

## [0.7.31](https://github.com/tatolab/streamlib/compare/v0.7.30...v0.7.31) (2026-07-22)


### Features

* **engine:** loader slot derives version from typed identity, not path string ([#1529](https://github.com/tatolab/streamlib/issues/1529)) ([19fa840](https://github.com/tatolab/streamlib/commit/19fa8407e46e37ef07ed88a7ffe8ab3f977be78c)), closes [#1518](https://github.com/tatolab/streamlib/issues/1518)

## [0.7.30](https://github.com/tatolab/streamlib/compare/v0.7.29...v0.7.30) (2026-07-22)


### Features

* **engine:** app-root-aware installed-package slot seam (behavior-preserving) ([#1527](https://github.com/tatolab/streamlib/issues/1527)) ([c23b552](https://github.com/tatolab/streamlib/commit/c23b552402b90ac52865ec83ba9429365823ab67))

## [0.7.29](https://github.com/tatolab/streamlib/compare/v0.7.28...v0.7.29) (2026-07-22)


### Features

* **plugin-sdk:** wire runtime_id/processor_id/is_paused/should_process on cdylib-arm RuntimeContext views ([#1525](https://github.com/tatolab/streamlib/issues/1525)) ([884fe99](https://github.com/tatolab/streamlib/commit/884fe9994038c7a5e5357927f5916bb94197c63a))

## [0.7.28](https://github.com/tatolab/streamlib/compare/v0.7.27...v0.7.28) (2026-07-22)


### Features

* **packages:** package-cache build-once-reuse + disk-discipline (precursor to [#1506](https://github.com/tatolab/streamlib/issues/1506)) ([#1515](https://github.com/tatolab/streamlib/issues/1515)) ([f49ede8](https://github.com/tatolab/streamlib/commit/f49ede84540234297d5e792e855f1846be11ee6f))

## [0.7.27](https://github.com/tatolab/streamlib/compare/v0.7.26...v0.7.27) (2026-07-22)


### Bug Fixes

* **engine:** restore grant-gated RuntimeContextFullAccess::new in tap-wiring test ([#1513](https://github.com/tatolab/streamlib/issues/1513)) ([19c742b](https://github.com/tatolab/streamlib/commit/19c742b7fecd329d721d4c32adf76b1421ce6a29))

## [0.7.26](https://github.com/tatolab/streamlib/compare/v0.7.25...v0.7.26) (2026-07-22)


### Bug Fixes

* **packages:** debug-utilities cdylib-safe frame upload; delete build debris; stale tests ([#1511](https://github.com/tatolab/streamlib/issues/1511)) ([8da8324](https://github.com/tatolab/streamlib/commit/8da83241d992b6e1fa34aa6c7c0dcc767e22a7e6))

## [0.7.25](https://github.com/tatolab/streamlib/compare/v0.7.24...v0.7.25) (2026-07-21)


### Features

* **packages:** MCP veneer over the api-server ([#1499](https://github.com/tatolab/streamlib/issues/1499)) ([30311b6](https://github.com/tatolab/streamlib/commit/30311b697cb95f06bfff8859ebcdddea8207ca3c))

## [0.7.24](https://github.com/tatolab/streamlib/compare/v0.7.23...v0.7.24) (2026-07-21)


### Features

* **engine:** isolation policy tier — trust model + capability moat for loaded code ([#1494](https://github.com/tatolab/streamlib/issues/1494)) ([1811b84](https://github.com/tatolab/streamlib/commit/1811b8469c6e6adb7a87b82395673f5998680ae4))


### Bug Fixes

* **runtime:** resolve api-server bootstrap by version-free reference ([#1496](https://github.com/tatolab/streamlib/issues/1496)) ([9544553](https://github.com/tatolab/streamlib/commit/954455376e54880b70f3b9b684481d805bb456c9))

## [0.7.23](https://github.com/tatolab/streamlib/compare/v0.7.22...v0.7.23) (2026-07-21)


### Features

* **packages:** api-server POST /api/processor/source + replace endpoints ([#1492](https://github.com/tatolab/streamlib/issues/1492)) ([8781b94](https://github.com/tatolab/streamlib/commit/8781b943b3b41bbe152de80b79b1c8dc80f1807a))

## [0.7.22](https://github.com/tatolab/streamlib/compare/v0.7.21...v0.7.22) (2026-07-21)


### Features

* **runtime:** register-from-source + replace runtime ops (RuntimeOpsVTable v3) ([#1489](https://github.com/tatolab/streamlib/issues/1489)) ([c7a6f68](https://github.com/tatolab/streamlib/commit/c7a6f68342925f0579b5b8d4de9334f46bba383e))

## [0.7.21](https://github.com/tatolab/streamlib/compare/v0.7.20...v0.7.21) (2026-07-21)


### Features

* **packages:** api-server bearer-token auth + localhost-default bind ([#1488](https://github.com/tatolab/streamlib/issues/1488)) ([5069cc0](https://github.com/tatolab/streamlib/commit/5069cc072e272460fa2dcc520d7f73cfc62ecbd7))

## [0.7.20](https://github.com/tatolab/streamlib/compare/v0.7.19...v0.7.20) (2026-07-21)


### Features

* **sdk:** unify read_mode/overflow/buffer_size/max_queued_messages into one delivery profile ([#1486](https://github.com/tatolab/streamlib/issues/1486)) ([c774809](https://github.com/tatolab/streamlib/commit/c77480954fec407b0bc9cbb3c425414b8756d639))

## [0.7.19](https://github.com/tatolab/streamlib/compare/v0.7.18...v0.7.19) (2026-07-20)


### Features

* **engine:** dynamic iceoryx2 PowerOfTwo slot allocation; retire authored max_payload_bytes ([#1482](https://github.com/tatolab/streamlib/issues/1482)) ([a050eb7](https://github.com/tatolab/streamlib/commit/a050eb79f6c041521f0f3aefc929ee6341f633f4))

## [0.7.18](https://github.com/tatolab/streamlib/compare/v0.7.17...v0.7.18) (2026-07-20)


### Features

* **engine:** channel-centric iceoryx2 transport, zero-copy fan-out ([#1480](https://github.com/tatolab/streamlib/issues/1480)) ([f248808](https://github.com/tatolab/streamlib/commit/f248808ae7d7f3a986e013d314a94f444c45e613))

## [0.7.17](https://github.com/tatolab/streamlib/compare/v0.7.16...v0.7.17) (2026-07-20)


### Features

* **engine:** read the wire schema-ident tag — connect-time + runtime schema-mismatch checks ([#1477](https://github.com/tatolab/streamlib/issues/1477)) ([a158b50](https://github.com/tatolab/streamlib/commit/a158b508abcac760b02a0913fafd94012010095f))

## [0.7.16](https://github.com/tatolab/streamlib/compare/v0.7.15...v0.7.16) (2026-07-20)


### Features

* **manifest:** drop the vestigial per-processor version field ([#1474](https://github.com/tatolab/streamlib/issues/1474)) ([08c3c7b](https://github.com/tatolab/streamlib/commit/08c3c7b9958e1484beac1c52af4c141a5a57238b))

## [0.7.15](https://github.com/tatolab/streamlib/compare/v0.7.14...v0.7.15) (2026-07-20)


### Bug Fixes

* **cli:** make streamlib add format-preserving ([#1473](https://github.com/tatolab/streamlib/issues/1473)) ([9caa64f](https://github.com/tatolab/streamlib/commit/9caa64fdc8a2631bd068cbcce605df1226ab3bea))

## [0.7.14](https://github.com/tatolab/streamlib/compare/v0.7.13...v0.7.14) (2026-07-19)


### Features

* **runtime:** load-gate installed-set-only resolution; apps carry no dependencies ([#1467](https://github.com/tatolab/streamlib/issues/1467)) ([f21b3d3](https://github.com/tatolab/streamlib/commit/f21b3d3e0c73da0760c5365d302acb21f84072c1))

## [0.7.13](https://github.com/tatolab/streamlib/compare/v0.7.12...v0.7.13) (2026-07-19)


### Features

* **cli:** derive + reconcile package dependencies; streamlib add records the range ([#1465](https://github.com/tatolab/streamlib/issues/1465)) ([e5cf615](https://github.com/tatolab/streamlib/commit/e5cf6152ca7d68bdccfc52aa446a3a90d78d032c))

## [0.7.12](https://github.com/tatolab/streamlib/compare/v0.7.11...v0.7.12) (2026-07-19)


### Bug Fixes

* **engine:** make the staged-processor cross-check version-blind ([#1462](https://github.com/tatolab/streamlib/issues/1462)) ([c9cc665](https://github.com/tatolab/streamlib/commit/c9cc665499ebacaf9b33495c3219c009dcb7fc55))

## [0.7.11](https://github.com/tatolab/streamlib/compare/v0.7.10...v0.7.11) (2026-07-19)


### Features

* **examples:** hello-streamlib inline-processor example + E2E ([#1458](https://github.com/tatolab/streamlib/issues/1458)) ([f84b839](https://github.com/tatolab/streamlib/commit/f84b839eccb9ba6071cf5df9256bfd4583f971d1))
* **pack:** derive processor manifests from code with a drift gate (closes [#1411](https://github.com/tatolab/streamlib/issues/1411)) ([#1459](https://github.com/tatolab/streamlib/issues/1459)) ([476e9a6](https://github.com/tatolab/streamlib/commit/476e9a6958c11155c3457f00af08fde1d1f664af))

## [0.7.10](https://github.com/tatolab/streamlib/compare/v0.7.9...v0.7.10) (2026-07-19)


### Features

* **polyglot:** ports-in-code parity for the Python/Deno processor decorators ([#1456](https://github.com/tatolab/streamlib/issues/1456)) ([acbf828](https://github.com/tatolab/streamlib/commit/acbf828a0ef3843671bd626bc5b89926e73f749e))

## [0.7.9](https://github.com/tatolab/streamlib/compare/v0.7.8...v0.7.9) (2026-07-19)


### Features

* **sdk:** App entrypoint sugar over Runner ([#1412](https://github.com/tatolab/streamlib/issues/1412)) ([#1453](https://github.com/tatolab/streamlib/issues/1453)) ([9ed563a](https://github.com/tatolab/streamlib/commit/9ed563ac1cb015bfa5a23499adcefdb3b2067d4e))

## [0.7.8](https://github.com/tatolab/streamlib/compare/v0.7.7...v0.7.8) (2026-07-19)


### Bug Fixes

* **runtime:** connect() with default processor ids no longer returns InvalidLink ([#1416](https://github.com/tatolab/streamlib/issues/1416) regression) ([#1451](https://github.com/tatolab/streamlib/issues/1451)) ([06c39c8](https://github.com/tatolab/streamlib/commit/06c39c86b0d5f70503a4439b13e03750086a27ef))

## [0.7.7](https://github.com/tatolab/streamlib/compare/v0.7.6...v0.7.7) (2026-07-19)


### Features

* **polyglot:** manifest extraction inversion — decorator is the truth-source, not sibling streamlib.yaml ([#1448](https://github.com/tatolab/streamlib/issues/1448)) ([a990d07](https://github.com/tatolab/streamlib/commit/a990d0733e78515f1561744604dd14676a9ad3a2))

## [0.7.6](https://github.com/tatolab/streamlib/compare/v0.7.5...v0.7.6) (2026-07-19)


### Features

* **idents:** channel-name grammar + connect() generator + fix PortKey silent truncation ([#1416](https://github.com/tatolab/streamlib/issues/1416)) ([#1446](https://github.com/tatolab/streamlib/issues/1446)) ([15eb9c0](https://github.com/tatolab/streamlib/commit/15eb9c07d6447f939d50b19c9125fdf99fe868ae))

## [0.7.5](https://github.com/tatolab/streamlib/compare/v0.7.4...v0.7.5) (2026-07-19)


### Features

* **cli:** reachability-precursor extraction for [#1411](https://github.com/tatolab/streamlib/issues/1411) manifest scan ([#1445](https://github.com/tatolab/streamlib/issues/1445)) ([05c3e99](https://github.com/tatolab/streamlib/commit/05c3e99933f6de86fccb6de764de585eef990511))

## [0.7.4](https://github.com/tatolab/streamlib/compare/v0.7.3...v0.7.4) (2026-07-19)


### Features

* **cli:** manifest extraction capability — syn source-scan + shared grammar ([#1411](https://github.com/tatolab/streamlib/issues/1411) stage 1) ([#1439](https://github.com/tatolab/streamlib/issues/1439)) ([50df8cf](https://github.com/tatolab/streamlib/commit/50df8cfe859d4deb1f1354d1335b8ea7e1697aa3))

## [0.7.3](https://github.com/tatolab/streamlib/compare/v0.7.2...v0.7.3) (2026-07-19)


### Features

* **engine:** Runner::add_local — register a #[processor] host type live ([#1441](https://github.com/tatolab/streamlib/issues/1441)) ([8aeba38](https://github.com/tatolab/streamlib/commit/8aeba387935ac47ece9678f2f5690a26ce05a05d))

## [0.7.2](https://github.com/tatolab/streamlib/compare/v0.7.1...v0.7.2) (2026-07-19)


### Features

* **macros:** ports-in-code — #[processor] attribute is the single source of truth ([#1437](https://github.com/tatolab/streamlib/issues/1437)) ([bfc0f14](https://github.com/tatolab/streamlib/commit/bfc0f142c22e7fd6d653568de277f8304a1f4c76))

## [0.7.1](https://github.com/tatolab/streamlib/compare/v0.7.0...v0.7.1) (2026-07-18)


### Features

* **plugin-sdk:** dynamic Bag read/write over the msgpack wire ([#1431](https://github.com/tatolab/streamlib/issues/1431)) ([f5e5d89](https://github.com/tatolab/streamlib/commit/f5e5d8952f9f8e2907f478e05199c4b2a4da39ff))

## [0.5.1](https://github.com/tatolab/streamlib/compare/v0.5.0...v0.5.1) (2026-06-08)


### Features

* **mavlink:** surface all sim-emitted messages (LOCAL_POSITION_NED, ODOMETRY, ACTUATOR_OUTPUT_STATUS, COLLISION, COMMAND_ACK) ([#1231](https://github.com/tatolab/streamlib/issues/1231)) ([76096cf](https://github.com/tatolab/streamlib/commit/76096cf89069cf2812d6d3b917b6f1124876d42a))

## [0.5.0](https://github.com/tatolab/streamlib/compare/v0.4.36...v0.5.0) (2026-06-07)


### Features

* **examples:** engine-free grayscale-compute example (validates the SDK surface-consumer path) ([7f39052](https://github.com/tatolab/streamlib/commit/7f390524321e545e4af272d29d187e087afcf82e))
* **plugin-sdk:** expose GPU surface-consumer + graphics-effect methods on the engine-free cdylib twin ([c443ac5](https://github.com/tatolab/streamlib/commit/c443ac5aa5517361bde5c08ed25bd471bcb0da43))
* **plugin-sdk:** expose graphics-effect kernel on the engine-free twin ([95fa05f](https://github.com/tatolab/streamlib/commit/95fa05f4645f9b64cb12e0b5939c2a7ed36eb590))
* **plugin-sdk:** expose pixel-buffer + pooled-texture consumer/lifecycle methods ([540408c](https://github.com/tatolab/streamlib/commit/540408cd96b492fafca5f107f9537bb20ed76af9))
* **plugin-sdk:** expose surface→texture resolution on engine-free twin ([2119306](https://github.com/tatolab/streamlib/commit/21193060507edaa7802129dace0e245ed40ecc8a))


### Miscellaneous

* release as 0.5.0 ([ba953d6](https://github.com/tatolab/streamlib/commit/ba953d61219a85294e7c8c513046e40bf288b309))

## [0.4.36](https://github.com/tatolab/streamlib/compare/v0.4.35...v0.4.36) (2026-06-03)


### Features

* **jpeg:** STREAMLIB_JPEG_BACKEND override + retarget plugin pins to the dev-loop engine ([c272c6a](https://github.com/tatolab/streamlib/commit/c272c6a9d44c07ad8f8299a485485fd8ebb63213))
* **mavlink:** add command_long + encapsulated_data to MavlinkMessage union ([#1198](https://github.com/tatolab/streamlib/issues/1198)) ([2b69a8f](https://github.com/tatolab/streamlib/commit/2b69a8f68be345245ebe467eacc57d0e7285c0cb))
* **mavlink:** complete the engine-free schema — add command_long + encapsulated_data (1.1.2) ([8fcfd50](https://github.com/tatolab/streamlib/commit/8fcfd5045078fcf5032a15b9eefe5e1f0e5aeff0))
* **plugin-sdk:** auto-detect the real SDK crate in #[processor] + make export_plugin! SDK-agnostic ([c857902](https://github.com/tatolab/streamlib/commit/c85790236cd9abbaa919762e6ea16994723b8f16))
* **plugin-sdk:** migrate network / vadr-vision / mavlink packages to the engine-free SDK ([1e9d122](https://github.com/tatolab/streamlib/commit/1e9d122522ee5092a59f8baa1573c7a9403db8f4))
* **plugin-sdk:** relocate the cdylib arm of the dual-mode plugin machinery ([efccdfe](https://github.com/tatolab/streamlib/commit/efccdfe266b91f9166734517c0d0fb0cbc79196f))
* **plugin-sdk:** relocate the Vulkan-compute GPU FullAccess surface into the SDK ([131f049](https://github.com/tatolab/streamlib/commit/131f049c7d01ed40e416809cb9fb9bd1d966e543))
* **plugin-sdk:** scaffold streamlib-plugin-sdk with the engine-free shared surface ([f4a70d6](https://github.com/tatolab/streamlib/commit/f4a70d61bebbf05bc0f2fbfa8ea59706c5403369))
* **plugin-sdk:** split vulkan-jpeg — engine-free plugin/vulkan-jpeg + parked nvJPEG ([f37d23f](https://github.com/tatolab/streamlib/commit/f37d23f10b2e4794a3407f18abcffb9c089fd897))


### Bug Fixes

* **gitea-publish:** bump the plugin/ zone Cargo.tomls in the dev-version rewrite ([8bf03d6](https://github.com/tatolab/streamlib/commit/8bf03d698a60e6c325bf6ca6731216790eda2fda))
* **ipc:** size shared per-destination iceoryx2 service to its deepest inbound link ([3a81f42](https://github.com/tatolab/streamlib/commit/3a81f42566f4cb2ca0a13c1c0261e77fc77c696a))
* **jpeg:** build GPU kernel + buffers via FullAccess primitives ([#1199](https://github.com/tatolab/streamlib/issues/1199)) ([769975b](https://github.com/tatolab/streamlib/commit/769975ba98174d38d11b20b02249d0222f6455c0))
* **jpeg:** pin @tatolab/jpeg to streamlib 0.4.35-dev.1 (GPU plugin/host coherence) ([e92d3de](https://github.com/tatolab/streamlib/commit/e92d3defb0c551b2aac1a7988314d8f97e9fd964))
* **jpeg:** use cdylib-safe host_vulkan_device_arc() in vulkan-jpeg ([407cb78](https://github.com/tatolab/streamlib/commit/407cb7864c7b853b136ef8c0bc8da1feea1e3e96))
* **mavlink:** publish engine-free .slpkg at 1.1.1 (above the stale 1.1.0) ([ed9d97c](https://github.com/tatolab/streamlib/commit/ed9d97c884b9b8c50019c9c2a4df6ec379af665b))
* **rhi:** drain the device inside the escalate gate, not after releasing it ([e8d865a](https://github.com/tatolab/streamlib/commit/e8d865ab1fb0e14d98dc3e7ab24d99352f03f758))
* **rhi:** pre-warm the shader-compiler pipeline path at device init ([#1203](https://github.com/tatolab/streamlib/issues/1203)) ([5290ddd](https://github.com/tatolab/streamlib/commit/5290ddd70c3f3c0d72c87ca5211b949b13573d95))
* **rhi:** route vkDeviceWaitIdle through the queue-mutex helper + co-locate the pipeline cache under STREAMLIB_HOME ([70012bd](https://github.com/tatolab/streamlib/commit/70012bd728cf7bbcbdc48ba774192969c2b02234))

## [0.4.35](https://github.com/tatolab/streamlib/compare/v0.4.34...v0.4.35) (2026-05-31)


### Features

* **1119:** anonymous generic-registry version index + bulk package publish ([#1163](https://github.com/tatolab/streamlib/issues/1163)) ([522762d](https://github.com/tatolab/streamlib/commit/522762d2a7802991ad3f34219ce1079a91f605cd))
* **1131:** convert api-server example to registry-only standalone ([#1165](https://github.com/tatolab/streamlib/issues/1165)) ([23416d4](https://github.com/tatolab/streamlib/commit/23416d45a65640464655ec22188cb1f50718e60f)), closes [#1131](https://github.com/tatolab/streamlib/issues/1131)
* **1132:** camera-audio-recorder → registry-only no-op (deferred) ([#1177](https://github.com/tatolab/streamlib/issues/1177)) ([9dc7ef8](https://github.com/tatolab/streamlib/commit/9dc7ef8727b2dc478cab9d1e0228166899c9f24f)), closes [#1132](https://github.com/tatolab/streamlib/issues/1132)
* **1134:** convert camera-rust-plugin to registry-only + Linux GrayscaleRust ([#1170](https://github.com/tatolab/streamlib/issues/1170)) ([eb265aa](https://github.com/tatolab/streamlib/commit/eb265aa7dc4d6e41199c9e17f9cd80e468fe6b0b)), closes [#1134](https://github.com/tatolab/streamlib/issues/1134)
* **1135:** convert cuda-fisheye-detection to registry-only standalone ([#1191](https://github.com/tatolab/streamlib/issues/1191)) ([44201c1](https://github.com/tatolab/streamlib/commit/44201c14aefb9e928ec40a80fa8b943473fffbf5)), closes [#1135](https://github.com/tatolab/streamlib/issues/1135)
* **1136:** convert h264-opus-validator to standalone (registry-only) ([#1175](https://github.com/tatolab/streamlib/issues/1175)) ([5950c1d](https://github.com/tatolab/streamlib/commit/5950c1ded0c37870bc65cb6217e4f0078657067b)), closes [#1136](https://github.com/tatolab/streamlib/issues/1136)
* **1137:** convert jpeg-psnr example to registry-only standalone ([#1166](https://github.com/tatolab/streamlib/issues/1166)) ([5d50a0e](https://github.com/tatolab/streamlib/commit/5d50a0e1072ef4001796ba85d368757e78bfbe79)), closes [#1137](https://github.com/tatolab/streamlib/issues/1137)
* **1138:** microphone-reverb-speaker → registry-only no-op (deferred) ([#1174](https://github.com/tatolab/streamlib/issues/1174)) ([3a48f4c](https://github.com/tatolab/streamlib/commit/3a48f4c3e9552e29fe92358e2331bf8b4a5822b3)), closes [#1138](https://github.com/tatolab/streamlib/issues/1138)
* **1139:** convert moq-roundtrip to registry-only standalone ([#1173](https://github.com/tatolab/streamlib/issues/1173)) ([6d3f0ca](https://github.com/tatolab/streamlib/commit/6d3f0ca1c91d486c051a9e0f642aa944d60e7830)), closes [#1139](https://github.com/tatolab/streamlib/issues/1139)
* **1140:** convert polyglot-continuous-processor to registry-only standalone ([#1180](https://github.com/tatolab/streamlib/issues/1180)) ([3925c48](https://github.com/tatolab/streamlib/commit/3925c481eb9ed26330d83c42638b5f8530ac2476)), closes [#1140](https://github.com/tatolab/streamlib/issues/1140)
* **1141:** convert polyglot-cpu-readback-blur to registry-only standalone ([#1182](https://github.com/tatolab/streamlib/issues/1182)) ([faa49b2](https://github.com/tatolab/streamlib/commit/faa49b26fd011ef8d7d83d485171302eede9fdf6)), closes [#1141](https://github.com/tatolab/streamlib/issues/1141)
* **1142:** convert polyglot-cuda-inference to registry-only standalone ([#1189](https://github.com/tatolab/streamlib/issues/1189)) ([23f6ebe](https://github.com/tatolab/streamlib/commit/23f6ebea69a7fef1ee12714017245e110148cf6c)), closes [#1142](https://github.com/tatolab/streamlib/issues/1142)
* **1143:** convert polyglot-dma-buf-consumer to registry-only standalone ([#1188](https://github.com/tatolab/streamlib/issues/1188)) ([c1933d1](https://github.com/tatolab/streamlib/commit/c1933d1ae2bb99c763d616f3f90ca9426ff51c7b)), closes [#1143](https://github.com/tatolab/streamlib/issues/1143)
* **1144:** convert polyglot-manual-source to registry-only standalone ([#1192](https://github.com/tatolab/streamlib/issues/1192)) ([2af5dae](https://github.com/tatolab/streamlib/commit/2af5daeeff4a74b4bdf4b19f82e7ffd511ec7537)), closes [#1144](https://github.com/tatolab/streamlib/issues/1144)
* **1145:** convert polyglot-opengl-fragment-shader to registry-only standalone ([#1186](https://github.com/tatolab/streamlib/issues/1186)) ([d594874](https://github.com/tatolab/streamlib/commit/d5948743d87c6f4faf5e47e2e2c365f026093dc4)), closes [#1145](https://github.com/tatolab/streamlib/issues/1145)
* **1146:** convert polyglot-skia-canvas to registry-only standalone ([#1187](https://github.com/tatolab/streamlib/issues/1187)) ([5bdf6de](https://github.com/tatolab/streamlib/commit/5bdf6dec80189ace776824e7f6805afb2b860a4b)), closes [#1146](https://github.com/tatolab/streamlib/issues/1146)
* **1147:** convert polyglot-venv-isolation to registry-only standalone ([#1181](https://github.com/tatolab/streamlib/issues/1181)) ([4f2b836](https://github.com/tatolab/streamlib/commit/4f2b836690d618a14342212f317be6aaaab0de48)), closes [#1147](https://github.com/tatolab/streamlib/issues/1147)
* **1148:** convert polyglot-vulkan-compute to registry-only standalone ([#1183](https://github.com/tatolab/streamlib/issues/1183)) ([d5e6af4](https://github.com/tatolab/streamlib/commit/d5e6af4058f8ec1aef94924dceeed68332ee9570)), closes [#1148](https://github.com/tatolab/streamlib/issues/1148)
* **1149:** convert polyglot-vulkan-graphics to registry-only standalone ([#1184](https://github.com/tatolab/streamlib/issues/1184)) ([cb97f6f](https://github.com/tatolab/streamlib/commit/cb97f6fa0199053e8383e8572f645f961a692f37)), closes [#1149](https://github.com/tatolab/streamlib/issues/1149)
* **1150:** convert polyglot-vulkan-ray-tracing to registry-only standalone ([#1185](https://github.com/tatolab/streamlib/issues/1185)) ([600cd81](https://github.com/tatolab/streamlib/commit/600cd8122a1e7f16677b007288b0a4171f484139)), closes [#1150](https://github.com/tatolab/streamlib/issues/1150)
* **1151:** convert raytracing-showcase to registry-only standalone ([#1171](https://github.com/tatolab/streamlib/issues/1171)) ([365af1b](https://github.com/tatolab/streamlib/commit/365af1bef6e1acf3c9720e3258728b95db14b1e9)), closes [#1151](https://github.com/tatolab/streamlib/issues/1151)
* **1152:** convert runtime-graph-json-demo to registry-only standalone ([#1167](https://github.com/tatolab/streamlib/issues/1167)) ([45e8465](https://github.com/tatolab/streamlib/commit/45e8465aa0dde85aa408972ddcab0644bd19cdfc)), closes [#1152](https://github.com/tatolab/streamlib/issues/1152)
* **1153:** screen-recorder → registry-only no-op (deferred) ([#1176](https://github.com/tatolab/streamlib/issues/1176)) ([c02a093](https://github.com/tatolab/streamlib/commit/c02a0931dd7552bf9f99e9fb51b1565a6f0c05b5)), closes [#1153](https://github.com/tatolab/streamlib/issues/1153)
* **1154:** convert vulkan-video-psnr example to registry-only standalone ([#1168](https://github.com/tatolab/streamlib/issues/1168)) ([e904c6b](https://github.com/tatolab/streamlib/commit/e904c6bdc693de3aa034cd8ad19de1d47676a971))
* **1155:** convert vulkan-video-roundtrip example to registry-only standalone ([#1169](https://github.com/tatolab/streamlib/issues/1169)) ([2497caf](https://github.com/tatolab/streamlib/commit/2497caf75880775b784b2ea49949f9d8349f3a1f)), closes [#1155](https://github.com/tatolab/streamlib/issues/1155)
* **1156:** convert vulkan-video-roundtrip-cdylib-camera to registry-only ([#1172](https://github.com/tatolab/streamlib/issues/1172)) ([124a531](https://github.com/tatolab/streamlib/commit/124a531c0911173c9d2772f8352d98bc7cf7de94)), closes [#1156](https://github.com/tatolab/streamlib/issues/1156)
* **1157:** convert webrtc-cloudflare-stream to registry-only standalone ([#1179](https://github.com/tatolab/streamlib/issues/1179)) ([ebe1333](https://github.com/tatolab/streamlib/commit/ebe1333c6a08dabcae177cf141d12b4a17579955)), closes [#1157](https://github.com/tatolab/streamlib/issues/1157)
* **1158:** whep-player → registry-only no-op (deferred) ([#1178](https://github.com/tatolab/streamlib/issues/1178)) ([7d2c124](https://github.com/tatolab/streamlib/commit/7d2c124c59e031607c8d5105d459cfb8951972c3)), closes [#1158](https://github.com/tatolab/streamlib/issues/1158)


### Bug Fixes

* **1142:** drop accidentally-committed yolov8n.pt weights from cuda-inference ([#1190](https://github.com/tatolab/streamlib/issues/1190)) ([07b8489](https://github.com/tatolab/streamlib/commit/07b84896e11cec263d240ffbf9b989f05324e39e))
* **core:** make @tatolab/core package registry-only (drop jtd-codegen path dep) ([#1194](https://github.com/tatolab/streamlib/issues/1194)) ([3bf8e30](https://github.com/tatolab/streamlib/commit/3bf8e306ce7cb8d0f4394ddea7a0454ca81a3271))
* **pack:** defer Python/TS entrypoint resolution to the runtime (PyPA) ([#1193](https://github.com/tatolab/streamlib/issues/1193)) ([d44a176](https://github.com/tatolab/streamlib/commit/d44a176b5c31e08fe6394c542452697e8d2bb050))

## [0.4.34](https://github.com/tatolab/streamlib/compare/v0.4.33...v0.4.34) (2026-05-31)


### Features

* **1133:** camera-deno-subprocess standalone registry-only + Deno mirror-staging ([#1162](https://github.com/tatolab/streamlib/issues/1162)) ([8085a9d](https://github.com/tatolab/streamlib/commit/8085a9d6655bba7c1af96cd33a22e28fa8752a3d))
* **deno-sdk:** publish to Gitea npm + bare-specifier resolution + protocol handshake ([#1118](https://github.com/tatolab/streamlib/issues/1118)) ([#1160](https://github.com/tatolab/streamlib/issues/1160)) ([f39833a](https://github.com/tatolab/streamlib/commit/f39833adabf3c109c022fe2b0cfa99397ff1126e))

## [0.4.33](https://github.com/tatolab/streamlib/compare/v0.4.32...v0.4.33) (2026-05-31)


### Features

* **dist:** registry-only source distribution — Strategy::Registry, source-only .slpkg, de-workspace packages + examples ([#1128](https://github.com/tatolab/streamlib/issues/1128)) ([f644c39](https://github.com/tatolab/streamlib/commit/f644c390f6a5b3b592f6b0056f62a62e01f9f660))

## [0.4.32](https://github.com/tatolab/streamlib/compare/v0.4.31...v0.4.32) (2026-05-30)


### Features

* **orchestrator:** lift Python venv provisioning + codegen into the build orchestrator ([#1126](https://github.com/tatolab/streamlib/issues/1126)) ([#1127](https://github.com/tatolab/streamlib/issues/1127)) ([b69db4a](https://github.com/tatolab/streamlib/commit/b69db4a7e823a98a528752cc367e4d9a57792bf5))

## [0.4.31](https://github.com/tatolab/streamlib/compare/v0.4.30...v0.4.31) (2026-05-30)


### Features

* **display:** graceful headless degradation — drain-and-drop when no surface ([#1112](https://github.com/tatolab/streamlib/issues/1112)) ([8abf1c4](https://github.com/tatolab/streamlib/commit/8abf1c4f707a4c11cd1971f1b55c31335624e39f))
* **gitea:** resolve Rust SDK + vulkanalia by version from the Gitea registry ([#1123](https://github.com/tatolab/streamlib/issues/1123)) ([55f639d](https://github.com/tatolab/streamlib/commit/55f639dcd0d352375bae4aa7b6f4d6f0bad28743))
* **idents:** schema-package registry resolution + cargo-publish manifest strip capability ([#1122](https://github.com/tatolab/streamlib/issues/1122)) ([7a23126](https://github.com/tatolab/streamlib/commit/7a231269de684b0618fb694ac31a79e22a8fe8aa))
* **python:** publish SDK to Gitea + declare/install/handshake, de-magic PYTHONPATH ([#1125](https://github.com/tatolab/streamlib/issues/1125)) ([421ac6f](https://github.com/tatolab/streamlib/commit/421ac6fd41598390a1207d720c0bb4c4f5398b96))

## [0.4.30](https://github.com/tatolab/streamlib/compare/v0.4.29...v0.4.30) (2026-05-29)


### Features

* **build:** remote .slpkg distribution — fetch + verify Strategy::Url ([#1101](https://github.com/tatolab/streamlib/issues/1101)) ([c99bb61](https://github.com/tatolab/streamlib/commit/c99bb619994a80f303467c90693de02e4a0076f6))
* **effects:** migrate camera-python-display kernel wrappers off vulkanalia onto engine RHI ([#1086](https://github.com/tatolab/streamlib/issues/1086)) ([5097f64](https://github.com/tatolab/streamlib/commit/5097f644ed9ed9965adcd1bb147d20c3194e93d5))
* **engine:** capstone — delete RhiQueueSubmitter trait + close milestone ([#938](https://github.com/tatolab/streamlib/issues/938), closes [#270](https://github.com/tatolab/streamlib/issues/270)) ([#944](https://github.com/tatolab/streamlib/issues/944)) ([a71a968](https://github.com/tatolab/streamlib/commit/a71a968f527ea069ff359953834de46e0dd058a8))
* **engine:** codec converters onto VulkanComputeKernel + kernel API growth ([#935](https://github.com/tatolab/streamlib/issues/935), [#821](https://github.com/tatolab/streamlib/issues/821)) ([#943](https://github.com/tatolab/streamlib/issues/943)) ([4deaf79](https://github.com/tatolab/streamlib/commit/4deaf797e682c5033640d5f0a8b25911471e2ec4))
* **engine:** codec DPB + bitstream onto HostVulkanTexture/Buffer ([#932](https://github.com/tatolab/streamlib/issues/932), [#933](https://github.com/tatolab/streamlib/issues/933)) ([#941](https://github.com/tatolab/streamlib/issues/941)) ([518c31d](https://github.com/tatolab/streamlib/commit/518c31d727e8d49df13d8d1b1b3bb81200740c80))
* **engine:** codec from_full_access + remove from_device shim ([#915](https://github.com/tatolab/streamlib/issues/915), [#916](https://github.com/tatolab/streamlib/issues/916)) ([#931](https://github.com/tatolab/streamlib/issues/931)) ([1908b5f](https://github.com/tatolab/streamlib/commit/1908b5f9380b84b00429ddce87c92c52785e1d42))
* **engine:** fold libs/vulkan-video into streamlib-engine codec layer ([#915](https://github.com/tatolab/streamlib/issues/915)) ([#930](https://github.com/tatolab/streamlib/issues/930)) ([7b2502a](https://github.com/tatolab/streamlib/commit/7b2502af535cdb2e77eb9d091ecca408ccbb5f71))
* **engine:** HostVulkanQueryPool — generic VkQueryPool RHI primitive ([#937](https://github.com/tatolab/streamlib/issues/937)) ([#942](https://github.com/tatolab/streamlib/issues/942)) ([6f935b6](https://github.com/tatolab/streamlib/commit/6f935b6e2eac5c63cc9af0248d2768cecdf77fdd))
* **engine:** HostVulkanVideoSession + Parameters as privileged RHI primitives ([#936](https://github.com/tatolab/streamlib/issues/936)) ([#939](https://github.com/tatolab/streamlib/issues/939)) ([8999015](https://github.com/tatolab/streamlib/commit/89990156ce00a23f0d1cedbf08e7311db623f8c3))
* **examples:** commit cdylib-camera manual gate harness + lock all 4 make_*_borrow helpers ([#990](https://github.com/tatolab/streamlib/issues/990)) ([11e9953](https://github.com/tatolab/streamlib/commit/11e9953fb71b3668d6db9b72b77a5abdcb8260aa))
* **examples:** migrate audio-mixer-demo to load_workspace_packages ([#881](https://github.com/tatolab/streamlib/issues/881)) ([#1031](https://github.com/tatolab/streamlib/issues/1031)) ([151fa7f](https://github.com/tatolab/streamlib/commit/151fa7f0c91694f2ba1594b090dd36f18cb3d6c9))
* **packages:** sync-lifecycle + own-runtime sweep ([#895](https://github.com/tatolab/streamlib/issues/895)) ([#900](https://github.com/tatolab/streamlib/issues/900)) ([bc0400f](https://github.com/tatolab/streamlib/commit/bc0400f86c5ab729ac74b3a3b400f7901ac36619))
* **packaging:** pre-built Python wheel + deno/ layout inside .slpkg ([#1056](https://github.com/tatolab/streamlib/issues/1056)) ([61613f1](https://github.com/tatolab/streamlib/commit/61613f1cc77c06c4efd39f856ef41a15980e9cb6))
* **plugin-abi:** [#914](https://github.com/tatolab/streamlib/issues/914) Phase 1 — gpu_capabilities vtable slot + camera capability migration ([#919](https://github.com/tatolab/streamlib/issues/919)) ([7a50012](https://github.com/tatolab/streamlib/commit/7a500122cef132325c428af2f35a1eb28ec7e4de))
* **plugin-abi:** cdylib dispatch for every GpuContextFullAccess method (Phase D of [#886](https://github.com/tatolab/streamlib/issues/886)) ([#906](https://github.com/tatolab/streamlib/issues/906)) ([#913](https://github.com/tatolab/streamlib/issues/913)) ([f976684](https://github.com/tatolab/streamlib/commit/f976684a5f7d33ec8bfcda3aaed9461f02811201))
* **plugin-abi:** cpu-readback adapter callback table ([#890](https://github.com/tatolab/streamlib/issues/890)) ([#997](https://github.com/tatolab/streamlib/issues/997)) ([bd604db](https://github.com/tatolab/streamlib/commit/bd604dbb7231177211b6a099b63306980af95007))
* **plugin-abi:** cuda adapter callback table ([#891](https://github.com/tatolab/streamlib/issues/891)) ([#998](https://github.com/tatolab/streamlib/issues/998)) ([249237f](https://github.com/tatolab/streamlib/commit/249237fb8fc8aa4be9e97102fdb29f7055dd22b9))
* **plugin-abi:** GpuContext vtable — escalate scope-token + acquire_render_target (Phase C3 of [#886](https://github.com/tatolab/streamlib/issues/886)) ([#912](https://github.com/tatolab/streamlib/issues/912)) ([a4b717b](https://github.com/tatolab/streamlib/commit/a4b717be1b62f32abd041ef7c46905ef27a3fb24))
* **plugin-abi:** GpuContext vtable — kernel construction (Phase C2 of [#886](https://github.com/tatolab/streamlib/issues/886)) ([#905](https://github.com/tatolab/streamlib/issues/905)) ([6cc83fe](https://github.com/tatolab/streamlib/commit/6cc83fe1c8883742d5849e61ffd34eedf50ea32d))
* **plugin-abi:** GpuContext vtable scaffold + pixel buffer / texture acquire-release (Phase C1 of [#886](https://github.com/tatolab/streamlib/issues/886)) ([#904](https://github.com/tatolab/streamlib/issues/904)) ([a452894](https://github.com/tatolab/streamlib/commit/a45289498abe8e8b0ee0b2e765baeac4267c2757))
* **plugin-abi:** host_vulkan_texture_arc vtable slot ([#1012](https://github.com/tatolab/streamlib/issues/1012)) ([#1021](https://github.com/tatolab/streamlib/issues/1021)) ([f3ebf02](https://github.com/tatolab/streamlib/commit/f3ebf020007d34a1e1ef0c986837a2b7c23e05e3))
* **plugin-abi:** opengl adapter callback table ([#888](https://github.com/tatolab/streamlib/issues/888)) ([#996](https://github.com/tatolab/streamlib/issues/996)) ([021fcdd](https://github.com/tatolab/streamlib/commit/021fcdd78b5765f1f9d7866f4239ee24809e5d3d))
* **plugin-abi:** Phase B — RuntimeContext / AudioClock / RuntimeOps callback tables ([#885](https://github.com/tatolab/streamlib/issues/885)) ([#899](https://github.com/tatolab/streamlib/issues/899)) ([f5546c3](https://github.com/tatolab/streamlib/commit/f5546c3365b2a5bba4e6b4d15353160c7c779d48))
* **plugin-abi:** processor lifecycle vtable (phase A of full callback-table ABI) ([#893](https://github.com/tatolab/streamlib/issues/893)) ([bc6c74a](https://github.com/tatolab/streamlib/commit/bc6c74a1d120fbb015386c6c65c91d3667225ef9))
* **plugin-abi:** pure callback-table ABI for cross-DSO bridges ([#884](https://github.com/tatolab/streamlib/issues/884)) ([06d6e5b](https://github.com/tatolab/streamlib/commit/06d6e5b3586089fa55186e3378bdfbce1fd53f30))
* **plugin-abi:** RhiCommandRecorder methods vtable (Phase E Slice B, [#984](https://github.com/tatolab/streamlib/issues/984)) ([#985](https://github.com/tatolab/streamlib/issues/985)) ([f0d38c1](https://github.com/tatolab/streamlib/commit/f0d38c19e7a390be947dd942099bb72c723b1e7d))
* **plugin-abi:** RhiCommandRecorderMethodsVTable PixelBuffer sibling slots + cdylib pixel-flow fix ([#988](https://github.com/tatolab/streamlib/issues/988)) ([#989](https://github.com/tatolab/streamlib/issues/989)) ([b49d368](https://github.com/tatolab/streamlib/commit/b49d36890c50b93537f52c8f056a9c3199188fe9))
* **plugin-abi:** skia adapter callback table ([#889](https://github.com/tatolab/streamlib/issues/889)) ([#995](https://github.com/tatolab/streamlib/issues/995)) ([83a4fa0](https://github.com/tatolab/streamlib/commit/83a4fa0aba1eab91677fab0489f9009319b710e9))
* **plugin-abi:** Texture::native_handle DMA-BUF FD vtable accessor ([#957](https://github.com/tatolab/streamlib/issues/957)) ([#973](https://github.com/tatolab/streamlib/issues/973)) ([55f0933](https://github.com/tatolab/streamlib/commit/55f0933e8cbcc3fc5aac555d4bb55e44b8cf0f30))
* **plugin-abi:** TextureRing per-type vtable shell + POD caching ([#907](https://github.com/tatolab/streamlib/issues/907) PR 1/5) ([#946](https://github.com/tatolab/streamlib/issues/946)) ([2554447](https://github.com/tatolab/streamlib/commit/2554447de68369df3a4517c5da182d3b4112398d))
* **plugin-abi:** TextureRingSlot β-shape + acquire_next / copy_pixel_buffer_to_slot / slot method dispatch ([#947](https://github.com/tatolab/streamlib/issues/947)) ([#968](https://github.com/tatolab/streamlib/issues/968)) ([615ee4b](https://github.com/tatolab/streamlib/commit/615ee4b22072f1e5986a4769314030037fd862d3))
* **plugin-abi:** vulkan adapter callback table ([#887](https://github.com/tatolab/streamlib/issues/887)) ([#994](https://github.com/tatolab/streamlib/issues/994)) ([80efbc8](https://github.com/tatolab/streamlib/commit/80efbc88bbf52cf6adde25c1c308406ff1c664ee))
* **plugin-abi:** VulkanAccelerationStructure build_*_blas out-params + label method dispatch ([#955](https://github.com/tatolab/streamlib/issues/955)) ([#969](https://github.com/tatolab/streamlib/issues/969)) ([f62421c](https://github.com/tatolab/streamlib/commit/f62421c7dab7eac442aa603dd71498165fe8629a))
* **plugin-abi:** VulkanAccelerationStructure per-type vtable shell + POD caching ([#907](https://github.com/tatolab/streamlib/issues/907) PR 5/5) ([#954](https://github.com/tatolab/streamlib/issues/954)) ([f0f8d90](https://github.com/tatolab/streamlib/commit/f0f8d90bba5ac4168418a40e5a2f742e8d635f1f))
* **plugin-abi:** VulkanComputeKernel per-type vtable shell + POD caching ([#907](https://github.com/tatolab/streamlib/issues/907) PR 2/5) ([#948](https://github.com/tatolab/streamlib/issues/948)) ([d75a70f](https://github.com/tatolab/streamlib/commit/d75a70fd496bfec2d555a8bcdabd700d51fec0e3))
* **plugin-abi:** VulkanComputeKernel set_push_constants + dispatch method dispatch ([#949](https://github.com/tatolab/streamlib/issues/949) slice) ([#962](https://github.com/tatolab/streamlib/issues/962)) ([b5035d3](https://github.com/tatolab/streamlib/commit/b5035d359fc4c9f2bd1b29377af4b8ccdd12ed86))
* **plugin-abi:** VulkanComputeKernel typed binding-method dispatch + CPU-ref dlopen test ([#963](https://github.com/tatolab/streamlib/issues/963)) ([#965](https://github.com/tatolab/streamlib/issues/965)) ([462512b](https://github.com/tatolab/streamlib/commit/462512b7dfd89320a2e65cd5cf28fa9ec8c13a0a))
* **plugin-abi:** VulkanGraphicsKernel per-type vtable shell + POD caching ([#907](https://github.com/tatolab/streamlib/issues/907) PR 3/5) ([#950](https://github.com/tatolab/streamlib/issues/950)) ([729fa76](https://github.com/tatolab/streamlib/commit/729fa7672a474f5adb029f9c4c2793bd6e9ba17d))
* **plugin-abi:** VulkanGraphicsKernel typed binding-method dispatch + offscreen-render dlopen smoke test ([#951](https://github.com/tatolab/streamlib/issues/951)) ([#966](https://github.com/tatolab/streamlib/issues/966)) ([399ac85](https://github.com/tatolab/streamlib/commit/399ac85a143b9ce94ff153c9d4dee49c21064881))
* **plugin-abi:** VulkanRayTracingKernel per-type vtable shell + POD caching ([#907](https://github.com/tatolab/streamlib/issues/907) PR 4/5) ([#952](https://github.com/tatolab/streamlib/issues/952)) ([a4a0468](https://github.com/tatolab/streamlib/commit/a4a0468e9645ae86358b3670a7ea64e258d00f56))
* **plugin-abi:** VulkanRayTracingKernel typed binding-method dispatch + trace_rays dlopen smoke test ([#953](https://github.com/tatolab/streamlib/issues/953)) ([#967](https://github.com/tatolab/streamlib/issues/967)) ([022889b](https://github.com/tatolab/streamlib/commit/022889b2a8e9620f0fec3a9a504cc8ea9d9ed264))
* **plugin-abi:** β-shape Phase D return types — close cross-repo plugin distribution coupling ([#917](https://github.com/tatolab/streamlib/issues/917)) ([#918](https://github.com/tatolab/streamlib/issues/918)) ([a90ffa2](https://github.com/tatolab/streamlib/commit/a90ffa2777d84fe89ea259ec0c7d5d75d9f449e8))
* **runtime:** imperative Runner::add_module + ModuleIdent (closes [#878](https://github.com/tatolab/streamlib/issues/878)) ([#1041](https://github.com/tatolab/streamlib/issues/1041)) ([de3d8d3](https://github.com/tatolab/streamlib/commit/de3d8d30be53d7e3b0bcbe19e0ca364f3e8570b6))
* **sdk:** graph snapshot save + load symmetry; rename load_graph_file ([#1098](https://github.com/tatolab/streamlib/issues/1098)) ([d774b2d](https://github.com/tatolab/streamlib/commit/d774b2d89faa0357c90e80f5ff14e782648e30ef))
* **xtask, sdk:** cargo xtask build-plugins + Runner::load_workspace_packages ([#991](https://github.com/tatolab/streamlib/issues/991)) ([#1030](https://github.com/tatolab/streamlib/issues/1030)) ([19447d9](https://github.com/tatolab/streamlib/commit/19447d97d2f749e8af13649968625365e56adaaf))


### Bug Fixes

* **1065:** camera-python-display effects cdylib reachability — graphics-kernel v4 + recorder v5 slots + lint sweep ([#1083](https://github.com/tatolab/streamlib/issues/1083)) ([92c882a](https://github.com/tatolab/streamlib/commit/92c882a1ca0ae865637237bbb186af1e5e6e7de6))
* **1066:** camera-display cdylib render path — host_video_source_timeline_arc + recorder vtable slots ([#1068](https://github.com/tatolab/streamlib/issues/1068)) ([ae32f6e](https://github.com/tatolab/streamlib/commit/ae32f6e20f8aac4729b75b27588cceb022bd175f))
* **1069:** PNG-sampling cdylib path — drop pixel-buffer reach-through + texture-readback arc transit ([#1070](https://github.com/tatolab/streamlib/issues/1070)) ([696de29](https://github.com/tatolab/streamlib/commit/696de29740dee270c025b8f90f081174eefefaa3))
* **1071:** h264/h265 encoder + camera_to_cuda_copy cdylib path — vulkan_inner + engine SDK device() reaches ([#1074](https://github.com/tatolab/streamlib/issues/1074)) ([eec5c6a](https://github.com/tatolab/streamlib/commit/eec5c6a8ff405bc15548a457e1c67e59da3382f2))
* **1072:** cdylib setup/teardown ScopeToken wrap + escalate-from-setup lock-in ([#1075](https://github.com/tatolab/streamlib/issues/1075)) ([5dcabb7](https://github.com/tatolab/streamlib/commit/5dcabb704cee8491bb2a80d5bbdd93c13b8caa43))
* **1073:** VulkanComputeKernel raw vk::* methods — v5 vtable slots ([#1078](https://github.com/tatolab/streamlib/issues/1078)) ([4f99f7b](https://github.com/tatolab/streamlib/commit/4f99f7b360c7928261b3d63240dffbae5ef3ee62))
* **1082:** producer-side overflow knob + mp4 reliable delivery ([#1084](https://github.com/tatolab/streamlib/issues/1084)) ([d0a1013](https://github.com/tatolab/streamlib/commit/d0a101391008a7e333ca64c80add9a45c3f2a97a))
* **engine:** port-spec helpers error on registry miss (closes [#869](https://github.com/tatolab/streamlib/issues/869)) ([#1055](https://github.com/tatolab/streamlib/issues/1055)) ([6f0d89d](https://github.com/tatolab/streamlib/commit/6f0d89d72800a98b4ef20cdee4574682146dba11))
* **engine:** swapchain barrier dispatcher arg order (VUID-03911 + VUID-03913) ([#1096](https://github.com/tatolab/streamlib/issues/1096)) ([840a9db](https://github.com/tatolab/streamlib/commit/840a9dbabdba41ea27795f3491c0bfd057d4197c)), closes [#1089](https://github.com/tatolab/streamlib/issues/1089)
* **engine:** tone-mapper layout-tracker desync (VUID-01197) ([#1095](https://github.com/tatolab/streamlib/issues/1095)) ([2b38267](https://github.com/tatolab/streamlib/commit/2b38267ec74c4f11a7e6c5a4f8137e64bc324a21)), closes [#1088](https://github.com/tatolab/streamlib/issues/1088)
* **plugin-abi:** GpuContextLimitedAccess video_source_timeline_semaphore panic guards ([#971](https://github.com/tatolab/streamlib/issues/971)) ([#975](https://github.com/tatolab/streamlib/issues/975)) ([914eff0](https://github.com/tatolab/streamlib/commit/914eff0e9d8a49023c0d49db6b039b75faace32a))
* **plugin-abi:** lossless PortSchemaSpec wire serde for cdylib FFI ([#869](https://github.com/tatolab/streamlib/issues/869)) ([#986](https://github.com/tatolab/streamlib/issues/986)) ([f476339](https://github.com/tatolab/streamlib/commit/f47633915dbb501d91dd05f75d5c76309e59a555))
* **plugin-abi:** null-handle guards + on_tick double-free for RCV/ACV/ROV vtables ([#977](https://github.com/tatolab/streamlib/issues/977)) ([6ff80ab](https://github.com/tatolab/streamlib/commit/6ff80ab1401ba96de840cf64b9820d95016c1ae7))
* **plugin-abi:** PixelBuffer::buffer_ref cdylib panic guard ([#908](https://github.com/tatolab/streamlib/issues/908)) ([#956](https://github.com/tatolab/streamlib/issues/956)) ([8e6a0d4](https://github.com/tatolab/streamlib/commit/8e6a0d40d60171926ae9147469201f3485c8a4e8))
* **plugin-abi:** SurfaceStore engine-only registration moves to HostSurfaceStoreExt ([#970](https://github.com/tatolab/streamlib/issues/970)) ([#974](https://github.com/tatolab/streamlib/issues/974)) ([89596f2](https://github.com/tatolab/streamlib/commit/89596f2f11f232dbf40423f435c36707d9887ead))
* **plugin-abi:** tighten kernel β-shapes + finish Phase E sub-lift + bindings vtable ([#1009](https://github.com/tatolab/streamlib/issues/1009)) ([5f2a47d](https://github.com/tatolab/streamlib/commit/5f2a47dca9a6e6bf25953a0cdc7ae788125fad5c))
* **vulkan-video:** edition 2024 unsafe_op_in_unsafe_fn cleanup (closes [#352](https://github.com/tatolab/streamlib/issues/352)) ([#929](https://github.com/tatolab/streamlib/issues/929)) ([62ea329](https://github.com/tatolab/streamlib/commit/62ea32970846771eccafa315ffe6ee86ec1b56c3))

## [0.4.29](https://github.com/tatolab/streamlib/compare/v0.4.28...v0.4.29) (2026-05-20)


### Features

* **704:** codegen-emitted SchemaIdent on Python dataclasses + remove [@schema](https://github.com/schema) ([#713](https://github.com/tatolab/streamlib/issues/713)) ([6bf4d7c](https://github.com/tatolab/streamlib/commit/6bf4d7cc21379c9118f6fb22840ff5b6e41eecc7))
* **adapter-cuda:** register_host_image_surface + CudaTextureView/CudaSurfaceView ([#807](https://github.com/tatolab/streamlib/issues/807)) ([e0cb12b](https://github.com/tatolab/streamlib/commit/e0cb12ba1c40823b7a9a0b52a6913379bd228ad8))
* **cli:** streamlib pack auto-builds dylib when lib/ is empty ([#749](https://github.com/tatolab/streamlib/issues/749)) ([3180eb0](https://github.com/tatolab/streamlib/commit/3180eb0974aae04b2e93e2f682ed81919136d992))
* **codec:** H.264/H.265 VUI — encoder writes from ColorInfo, decoder parses into frame.color_info ([#828](https://github.com/tatolab/streamlib/issues/828)) ([63b4aa7](https://github.com/tatolab/streamlib/commit/63b4aa7a9ddd0b09c9fae0cf61ca137ab795a0cc))
* **codegen:** streamlib.yaml resolver + lockfile + sentinel codegen + cutover (closes [#402](https://github.com/tatolab/streamlib/issues/402)) ([#696](https://github.com/tatolab/streamlib/issues/696)) ([44000d8](https://github.com/tatolab/streamlib/commit/44000d8df947728ef86697da4a269724a0f0ca78))
* **core:** ColorInfo + HDR sidecar metadata on VideoFrame / EncodedVideoFrame ([#811](https://github.com/tatolab/streamlib/issues/811)) ([#814](https://github.com/tatolab/streamlib/issues/814)) ([57450b8](https://github.com/tatolab/streamlib/commit/57450b8ab4dcc4df259b3777a736dfd0884e74d9))
* **deno:** [@streamlib](https://github.com/streamlib).processor decorator parity with Rust short-name macro ([#701](https://github.com/tatolab/streamlib/issues/701)) ([#715](https://github.com/tatolab/streamlib/issues/715)) ([b64f786](https://github.com/tatolab/streamlib/commit/b64f7868121013c00ac8cb7517e505b67110b707))
* **display:** negotiate VkColorSpaceKHR from frame ColorInfo + wire vkSetHdrMetadataEXT ([#817](https://github.com/tatolab/streamlib/issues/817)) ([#824](https://github.com/tatolab/streamlib/issues/824)) ([288a136](https://github.com/tatolab/streamlib/commit/288a13638aaadeb52f2638f1c25d0309ad641df5))
* **engine:** per-schema iceoryx2 ring depth via metadata.max_queued_messages ([#862](https://github.com/tatolab/streamlib/issues/862)) ([588fb2e](https://github.com/tatolab/streamlib/commit/588fb2ead05099b422e5ab0165ec0d80bc365a25))
* **example:** cuda-fisheye-detection — OPAQUE_FD VkImage texture interop validation ([#809](https://github.com/tatolab/streamlib/issues/809)) ([2057c1a](https://github.com/tatolab/streamlib/commit/2057c1af89ea51733a5d8577542e0b0c02b5d058))
* **idents:** SchemaIdent crate + architecture doc + CI lint (closes [#399](https://github.com/tatolab/streamlib/issues/399)) ([#692](https://github.com/tatolab/streamlib/issues/692)) ([e3a1c9a](https://github.com/tatolab/streamlib/commit/e3a1c9aefdaf7f0aa7f812d2712b6768c79cb678))
* **jpeg:** @tatolab/jpeg JpegDecoder processor wrapping libs/vulkan-jpeg ([#858](https://github.com/tatolab/streamlib/issues/858)) ([d069cc9](https://github.com/tatolab/streamlib/commit/d069cc92a5d9bde4cd3623efc2d0892890ebc70e))
* **manifest:** named schemas: map + bare-name refs ([#767](https://github.com/tatolab/streamlib/issues/767)) ([#768](https://github.com/tatolab/streamlib/issues/768)) ([7f64159](https://github.com/tatolab/streamlib/commit/7f64159449f77571d38b414ea3dc3fa8f50e20b8))
* **mavlink:** @tatolab/mavlink + reactive-runner burst-drain fix ([#836](https://github.com/tatolab/streamlib/issues/836)) ([f7cfe71](https://github.com/tatolab/streamlib/commit/f7cfe719c9d5d19b609d3baf6dce135a712cc98c))
* **network:** @tatolab/network — generic UDP source/sink processors ([#835](https://github.com/tatolab/streamlib/issues/835)) ([a9f6c6b](https://github.com/tatolab/streamlib/commit/a9f6c6b759a9c6d4db438ce0c0003c6fc01a1eeb))
* **network:** recvmmsg batching + 4 MiB default SO_RCVBUF in UdpSource ([#863](https://github.com/tatolab/streamlib/issues/863)) ([c87e4f8](https://github.com/tatolab/streamlib/commit/c87e4f8b8c91a2a49a5e169118b824a841d2bd32))
* **packages:** carve @tatolab/api-server out of libs/streamlib-engine ([#681](https://github.com/tatolab/streamlib/issues/681)) ([#772](https://github.com/tatolab/streamlib/issues/772)) ([6951946](https://github.com/tatolab/streamlib/commit/69519463be9c6c5b14a8e8f01cdcc4d981ef654c))
* **packages:** carve @tatolab/audio out of libs/streamlib ([#672](https://github.com/tatolab/streamlib/issues/672)) ([#728](https://github.com/tatolab/streamlib/issues/728)) ([8734808](https://github.com/tatolab/streamlib/commit/8734808df44e093dbe65c3394e964fd9e9d05061))
* **packages:** carve @tatolab/camera out of libs/streamlib-engine ([#673](https://github.com/tatolab/streamlib/issues/673)) ([#757](https://github.com/tatolab/streamlib/issues/757)) ([1389150](https://github.com/tatolab/streamlib/commit/1389150656e5a34f7908f755b10a28d067bdf1c0))
* **packages:** carve @tatolab/clap out of libs/streamlib-engine ([#682](https://github.com/tatolab/streamlib/issues/682)) ([#776](https://github.com/tatolab/streamlib/issues/776)) ([d71131e](https://github.com/tatolab/streamlib/commit/d71131e23cb5657fcda1368db1d77bbcb20069e8))
* **packages:** carve @tatolab/debug-utilities out of libs/streamlib-engine ([#783](https://github.com/tatolab/streamlib/issues/783)) ([#786](https://github.com/tatolab/streamlib/issues/786)) ([165a9bc](https://github.com/tatolab/streamlib/commit/165a9bc9238f16bca1ad5ad3cac3888ebe17d5e1))
* **packages:** carve @tatolab/display out of libs/streamlib-engine ([#674](https://github.com/tatolab/streamlib/issues/674)) ([#764](https://github.com/tatolab/streamlib/issues/764)) ([3ab6e06](https://github.com/tatolab/streamlib/commit/3ab6e0660cdf712f9796a564691f42115d210870))
* **packages:** carve @tatolab/escalate out of libs/streamlib-engine ([#779](https://github.com/tatolab/streamlib/issues/779)) ([#784](https://github.com/tatolab/streamlib/issues/784)) ([2b829cb](https://github.com/tatolab/streamlib/commit/2b829cbe78ae3eeb676a36fb94942ce653cccb30))
* **packages:** carve @tatolab/h264 out of libs/streamlib-engine ([#675](https://github.com/tatolab/streamlib/issues/675)) ([#766](https://github.com/tatolab/streamlib/issues/766)) ([d4d05a1](https://github.com/tatolab/streamlib/commit/d4d05a117689af82fa9f3c885d74e7b39ebbf100))
* **packages:** carve @tatolab/h265 out of libs/streamlib-engine ([#676](https://github.com/tatolab/streamlib/issues/676)) ([#769](https://github.com/tatolab/streamlib/issues/769)) ([208c6a6](https://github.com/tatolab/streamlib/commit/208c6a6cc65542bac4f80891f7522a18dfc90b0c))
* **packages:** carve @tatolab/moq out of libs/streamlib-engine ([#680](https://github.com/tatolab/streamlib/issues/680)) ([#775](https://github.com/tatolab/streamlib/issues/775)) ([8a4c39e](https://github.com/tatolab/streamlib/commit/8a4c39e7573022943ff170735cd80eeece3ddae0))
* **packages:** carve @tatolab/mp4 out of libs/streamlib-engine ([#678](https://github.com/tatolab/streamlib/issues/678)) ([#773](https://github.com/tatolab/streamlib/issues/773)) ([22ec323](https://github.com/tatolab/streamlib/commit/22ec323f5610644ab6d43609d446395fc3df4741))
* **packages:** carve @tatolab/opus out of libs/streamlib-engine ([#677](https://github.com/tatolab/streamlib/issues/677)) ([#770](https://github.com/tatolab/streamlib/issues/770)) ([8905b58](https://github.com/tatolab/streamlib/commit/8905b5874a670dd3530ec4a5819223dd38c2fb9b))
* **packages:** carve @tatolab/test-fixtures out of libs/streamlib-engine ([#780](https://github.com/tatolab/streamlib/issues/780)) ([#785](https://github.com/tatolab/streamlib/issues/785)) ([532ce80](https://github.com/tatolab/streamlib/commit/532ce806d9811f2a0fb61c3e3b8fad835636eb9e))
* **packages:** carve @tatolab/webrtc out of libs/streamlib-engine ([#679](https://github.com/tatolab/streamlib/issues/679)) ([#774](https://github.com/tatolab/streamlib/issues/774)) ([201612a](https://github.com/tatolab/streamlib/commit/201612ae1c0fd01703e65f6814f79739ff3bba63))
* **packages:** emit export_plugin! from every Rust-impl package's lib.rs ([#874](https://github.com/tatolab/streamlib/issues/874)) ([8109633](https://github.com/tatolab/streamlib/commit/810963363b56755d2b9b345a03ea04e8a12106e2))
* **packages:** extract streamlib-sdk from libs/streamlib (engine + authoring-API split) ([#737](https://github.com/tatolab/streamlib/issues/737)) ([7e73173](https://github.com/tatolab/streamlib/commit/7e73173d9cb4ffb9ed8ea800c91fe86a9c565c04))
* **packaging:** @tatolab/core + structured wire-format ([#401](https://github.com/tatolab/streamlib/issues/401)) ([#699](https://github.com/tatolab/streamlib/issues/699)) ([a9e2094](https://github.com/tatolab/streamlib/commit/a9e2094cb8e6ce3235b61ec1e916f56305cdc799))
* **packaging:** canonical dep references + workspace [patch] resolution ([#717](https://github.com/tatolab/streamlib/issues/717)) ([#727](https://github.com/tatolab/streamlib/issues/727)) ([ad8977b](https://github.com/tatolab/streamlib/commit/ad8977baccf47ff31962b7fb342e0008257c6f92))
* **packaging:** cross-platform per-target-triple .slpkg pack + load ([#872](https://github.com/tatolab/streamlib/issues/872)) ([bd13602](https://github.com/tatolab/streamlib/commit/bd1360272c529bb1e4ea860620041f7d049d89ee))
* **packaging:** JSON Schema for streamlib.yaml manifest ([#714](https://github.com/tatolab/streamlib/issues/714)) ([#718](https://github.com/tatolab/streamlib/issues/718)) ([0007011](https://github.com/tatolab/streamlib/commit/0007011043c4da57dc0e64db23c63955181201fc))
* **packaging:** processor short-name macros + structured PortDescriptor.schema ([#404](https://github.com/tatolab/streamlib/issues/404)) ([#703](https://github.com/tatolab/streamlib/issues/703)) ([a77e427](https://github.com/tatolab/streamlib/commit/a77e4276fcc23935bc8e8cc01a3a5e159186d7ae))
* **pipeline:** resolution propagation via first-frame inspection ([#810](https://github.com/tatolab/streamlib/issues/810)) ([#820](https://github.com/tatolab/streamlib/issues/820)) ([53a87ab](https://github.com/tatolab/streamlib/commit/53a87ab5f58dab3eb14257e0442e30d1b74a1c4e))
* **polyglot-native:** cudaExternalMemoryGetMappedMipmappedArray + texture/surface object creation ([#808](https://github.com/tatolab/streamlib/issues/808)) ([b4304d4](https://github.com/tatolab/streamlib/commit/b4304d43aeb7fd1912dd26dc871151ba4cd0b815))
* **python:** [@processor](https://github.com/processor) decorator parity with structured SchemaIdent ([#706](https://github.com/tatolab/streamlib/issues/706)) ([ad0fe7d](https://github.com/tatolab/streamlib/commit/ad0fe7d770abf917a02c0c0093d6b55347699559))
* **rhi:** HostVulkanPixelBuffer SSBO constructors + GpuContext::acquire_storage_buffer ([#752](https://github.com/tatolab/streamlib/issues/752)) ([57b2fe6](https://github.com/tatolab/streamlib/commit/57b2fe6f8b4902a9717b63c09f3288bb4464a2ef))
* **rhi:** OPAQUE_FD VkImage export/import + pool pre-warm ([#799](https://github.com/tatolab/streamlib/issues/799)) ([#805](https://github.com/tatolab/streamlib/issues/805)) ([4849a71](https://github.com/tatolab/streamlib/commit/4849a71b92991add0e58084bd46160824a1756db))
* **rhi:** RhiCommandRecorder + VulkanComputeKernel::record + VulkanBufferLike ([#754](https://github.com/tatolab/streamlib/issues/754)) ([51812ae](https://github.com/tatolab/streamlib/commit/51812ae430103fb66da522e393ad101147760597))
* **rhi:** RhiToneMapper — BT.2390 + BT.2446a image→image tone-curve kernel ([#825](https://github.com/tatolab/streamlib/issues/825)) ([6e46803](https://github.com/tatolab/streamlib/commit/6e46803e78ae3f8f890fc17af7de70189bb8ebc4))
* **rhi:** VulkanColorConverter — engine-owned (src,dst) color converter ([#823](https://github.com/tatolab/streamlib/issues/823)) ([5221568](https://github.com/tatolab/streamlib/commit/52215680f20e72178356ddd44abc50abcb6ad5f4))
* **runtime:** prctl(PR_SET_TIMERSLACK, 1) at top of Runner::new() ([#865](https://github.com/tatolab/streamlib/issues/865)) ([9dcdddb](https://github.com/tatolab/streamlib/commit/9dcdddb00e39e31d3542dfa7620a1f4e79599bf0)), closes [#839](https://github.com/tatolab/streamlib/issues/839)
* **runtime:** runtime-mutable schema registry + load_project schema reg ([#729](https://github.com/tatolab/streamlib/issues/729)) ([#747](https://github.com/tatolab/streamlib/issues/747)) ([101b7fb](https://github.com/tatolab/streamlib/commit/101b7fb0496d92ab6f3388177ae57773a6dddbf2))
* **scheduling:** declarative per-processor scheduling in streamlib.yaml ([#722](https://github.com/tatolab/streamlib/issues/722)) ([#748](https://github.com/tatolab/streamlib/issues/748)) ([d603b4e](https://github.com/tatolab/streamlib/commit/d603b4e903f4421e00056f1e51991224a9817ec1))
* schema_ident! / schema_ident_any_version! macros + typed runtime errors for unknown processors and missing ports ([#745](https://github.com/tatolab/streamlib/issues/745)) ([1e2553d](https://github.com/tatolab/streamlib/commit/1e2553da6d0c78c12b74b081872e6e399b1516db))
* **sdk:** expose core::pubsub via streamlib::sdk facade ([#763](https://github.com/tatolab/streamlib/issues/763)) ([6cd989a](https://github.com/tatolab/streamlib/commit/6cd989aebe158f783ca7697c0b93fdd533942096))
* **surface-share:** additive VkImageCreateInfo round-trip in wire format ([#806](https://github.com/tatolab/streamlib/issues/806)) ([621c102](https://github.com/tatolab/streamlib/commit/621c1020d3e47dd622de43402929b85d6fc5c8d8)), closes [#800](https://github.com/tatolab/streamlib/issues/800)
* **vadr-vision:** VADR-TS-002 §4.6 chunked-JPEG depayloader for AGP UDP 5600 ([#861](https://github.com/tatolab/streamlib/issues/861)) ([8ef14ec](https://github.com/tatolab/streamlib/commit/8ef14ec4e2eda33b6bd04d9cae60fb02394fcb83))
* **vulkan-jpeg:** fused Vulkan compute kernel for 4:2:0 JPEG decode ([#853](https://github.com/tatolab/streamlib/issues/853)) ([032e730](https://github.com/tatolab/streamlib/commit/032e730f637292dd302ad5c73bfe1365fa9a55e5)), closes [#841](https://github.com/tatolab/streamlib/issues/841)
* **vulkan-jpeg:** honor declared colorimetry (EXIF / ICC / Adobe APP14) ([#856](https://github.com/tatolab/streamlib/issues/856)) ([b40d5fd](https://github.com/tatolab/streamlib/commit/b40d5fdfe3c969d46b0740086d55cf7536214cf8))
* **vulkan-jpeg:** nvJPEG backend + ThirdPartyGpuCapabilities + arch doc ([#857](https://github.com/tatolab/streamlib/issues/857)) ([2aece27](https://github.com/tatolab/streamlib/commit/2aece276e4c1bc40dc844f162a0fb62a6d4eb290))
* **vulkan-jpeg:** scaffold libs/vulkan-jpeg with baseline parser + Huffman entropy decode ([#852](https://github.com/tatolab/streamlib/issues/852)) ([292e9cd](https://github.com/tatolab/streamlib/commit/292e9cd00c5afed9742932e0daf89a38c9d0031e))
* **vulkan-jpeg:** SimpleJpegDecoder API + internal texture ring ([#842](https://github.com/tatolab/streamlib/issues/842)) ([#855](https://github.com/tatolab/streamlib/issues/855)) ([6c05f83](https://github.com/tatolab/streamlib/commit/6c05f83ccaf3536e5a8f4dbe62912a5a3636a9c7))


### Bug Fixes

* **codegen:** root-name sentinel substitution across all 3 backends (closes [#541](https://github.com/tatolab/streamlib/issues/541)) ([#697](https://github.com/tatolab/streamlib/issues/697)) ([e364cfe](https://github.com/tatolab/streamlib/commit/e364cfe322ecc8a3d3a1e6a4eda579365ba21958))
* **packaging:** reconcile ProjectConfig.dependencies with structured Manifest shape ([#716](https://github.com/tatolab/streamlib/issues/716)) ([#726](https://github.com/tatolab/streamlib/issues/726)) ([72395f0](https://github.com/tatolab/streamlib/commit/72395f09d230fb21aaec1afecf95691e17ac70e8))
* **rust:** retype ProcessorSpec.name to structured SchemaIdent ([#721](https://github.com/tatolab/streamlib/issues/721)) ([a4f2b1d](https://github.com/tatolab/streamlib/commit/a4f2b1d6e09a9ba86feaf23965b2e61a0ffe6317)), closes [#707](https://github.com/tatolab/streamlib/issues/707) [#708](https://github.com/tatolab/streamlib/issues/708) [#709](https://github.com/tatolab/streamlib/issues/709) [#710](https://github.com/tatolab/streamlib/issues/710) [#711](https://github.com/tatolab/streamlib/issues/711) [#712](https://github.com/tatolab/streamlib/issues/712)
* **tests:** migrate pack + manifest-reader fixtures to named-schemas form ([#771](https://github.com/tatolab/streamlib/issues/771)) ([3c224be](https://github.com/tatolab/streamlib/commit/3c224be726b0755c1fa8e90e817af62e1c3eb91b))


### Performance

* **jtd-codegen:** emit serde_bytes for elements: uint8 binary fields ([#864](https://github.com/tatolab/streamlib/issues/864)) ([d684bcd](https://github.com/tatolab/streamlib/commit/d684bcd0a86864d1bcf79d64ff9e7eb8c806e6be))

## [0.4.28](https://github.com/tatolab/streamlib/compare/v0.4.27...v0.4.28) (2026-05-04)


### Features

* **adapter-skia:** producer-side QFOT release via dual-registration ([#645](https://github.com/tatolab/streamlib/issues/645)) ([#650](https://github.com/tatolab/streamlib/issues/650)) ([ba4ec9a](https://github.com/tatolab/streamlib/commit/ba4ec9a33734d6b410f1bd5f020999722355994a))
* **blending-compositor:** rewrite on graphics-kernel + texture-cache RHI; Skia overlays Linux ports (closes [#485](https://github.com/tatolab/streamlib/issues/485)) ([#671](https://github.com/tatolab/streamlib/issues/671)) ([307817c](https://github.com/tatolab/streamlib/commit/307817ccb003c2b07df571a0c97438f755a28670))
* **example,linux-python:** wire CrtFilmGrain + sandbox demo kernels out of engine ([#487](https://github.com/tatolab/streamlib/issues/487)) ([#690](https://github.com/tatolab/streamlib/issues/690)) ([9838d22](https://github.com/tatolab/streamlib/commit/9838d22aea17c7fe9dfdabaaa271b699fe375649))
* **linux-python:** Cyberpunk Glitch + full-frame grade Linux port ([#486](https://github.com/tatolab/streamlib/issues/486)) ([#686](https://github.com/tatolab/streamlib/issues/686)) ([a0567f9](https://github.com/tatolab/streamlib/commit/a0567f901fa9e121c2710baa8b57757e3be5f217))
* **polyglot:** subprocess escalate IPC for VulkanGraphicsKernel ([#656](https://github.com/tatolab/streamlib/issues/656)) ([#666](https://github.com/tatolab/streamlib/issues/666)) ([02c243a](https://github.com/tatolab/streamlib/commit/02c243a44dc0170c95332dbd56dffa2b71372117))
* **polyglot:** subprocess escalate IPC for VulkanRayTracingKernel ([#667](https://github.com/tatolab/streamlib/issues/667)) ([#670](https://github.com/tatolab/streamlib/issues/670)) ([ed8b498](https://github.com/tatolab/streamlib/commit/ed8b498563528f18af525d0f32930ebcc55bf965))
* **rhi:** VulkanGraphicsKernel — canonical graphics-pipeline kernel ([#609](https://github.com/tatolab/streamlib/issues/609)) ([#655](https://github.com/tatolab/streamlib/issues/655)) ([257c11f](https://github.com/tatolab/streamlib/commit/257c11fea197c4ed16355aeeec78b3b1d088914e))
* **rhi:** VulkanRayTracingKernel + VulkanAccelerationStructure ([#610](https://github.com/tatolab/streamlib/issues/610)) ([#669](https://github.com/tatolab/streamlib/issues/669)) ([c086fd1](https://github.com/tatolab/streamlib/commit/c086fd154885bfb9ea1319172808a4151e407676))

## [0.4.27](https://github.com/tatolab/streamlib/compare/v0.4.26...v0.4.27) (2026-05-03)


### Features

* **adapter-cuda + linux-python:** GPU-resident camera→cuda inference path ([#619](https://github.com/tatolab/streamlib/issues/619)) ([b0bb41e](https://github.com/tatolab/streamlib/commit/b0bb41e10c69ae8efd4ede29cd850a418a8e4815))
* **adapter-opengl:** producer-side QFOT release via dual-registration ([#644](https://github.com/tatolab/streamlib/issues/644)) ([#648](https://github.com/tatolab/streamlib/issues/648)) ([4878e86](https://github.com/tatolab/streamlib/commit/4878e864e17da4801ce65e9219497cc247b5e20d))
* **display:** PNG sampler captures from texture surfaces via VulkanTextureReadback ([#627](https://github.com/tatolab/streamlib/issues/627)) ([c24abf8](https://github.com/tatolab/streamlib/commit/c24abf85e585246bd94259bba362492e29fabfb2))
* **linux-python:** AvatarCharacter Linux port (cuda + opengl adapters) ([#611](https://github.com/tatolab/streamlib/issues/611)) ([cea4787](https://github.com/tatolab/streamlib/commit/cea47872be934f99a60a5a6d417eba01327256d0))
* **linux-python:** EGL DMA-BUF GL_TEXTURE_EXTERNAL_OES camera-bg path ([#615](https://github.com/tatolab/streamlib/issues/615)) ([#622](https://github.com/tatolab/streamlib/issues/622)) ([4292fc1](https://github.com/tatolab/streamlib/commit/4292fc1a5d8aab356110af2be28835b9b9ae85c1))
* **polyglot:** cdylib FFI for producer-side QFOT release / IPC layout publish ([#643](https://github.com/tatolab/streamlib/issues/643)) ([#647](https://github.com/tatolab/streamlib/issues/647)) ([7e70d6a](https://github.com/tatolab/streamlib/commit/7e70d6abb10462ab7378518fe2b8b4aacfa69db2))
* **rhi,ipc:** cross-process VkImageLayout coordination ([#633](https://github.com/tatolab/streamlib/issues/633)) ([#646](https://github.com/tatolab/streamlib/issues/646)) ([eb9553d](https://github.com/tatolab/streamlib/commit/eb9553df70c08aa7d7858c47599c9135c4e8c466))


### Bug Fixes

* **display:** adapter-output layout barrier via engine-wide TextureRegistration ([#616](https://github.com/tatolab/streamlib/issues/616)) ([#632](https://github.com/tatolab/streamlib/issues/632)) ([0d746cd](https://github.com/tatolab/streamlib/commit/0d746cd3730d4c5c0f1bc21d5155c6b46474e1d6))
* **linux-python:** AvatarCharacter Linux GLSL + EXTERNAL_OES end-to-end ([#629](https://github.com/tatolab/streamlib/issues/629)) ([d6bf98c](https://github.com/tatolab/streamlib/commit/d6bf98c393f184733fdb8a9ab4826eb7225cc592))
* **linux-python:** AvatarCharacter Vulkan-conventional Y in pose overlay ([#621](https://github.com/tatolab/streamlib/issues/621)) ([#636](https://github.com/tatolab/streamlib/issues/636)) ([82f4ace](https://github.com/tatolab/streamlib/commit/82f4ace07cfda762b344ed8a2666c6ac8475e680))
* **rhi,camera:** NVIDIA OPAQUE_FD reliability ([#637](https://github.com/tatolab/streamlib/issues/637) + [#638](https://github.com/tatolab/streamlib/issues/638)) ([#639](https://github.com/tatolab/streamlib/issues/639)) ([d8be611](https://github.com/tatolab/streamlib/commit/d8be6114581fbdba09e78edacf870292c5c8e813))
* **rhi:** pre-warm export VMA pools at HostVulkanDevice construction ([#624](https://github.com/tatolab/streamlib/issues/624)) ([#625](https://github.com/tatolab/streamlib/issues/625)) ([0836e33](https://github.com/tatolab/streamlib/commit/0836e3347180e10d2b58d35d4aecde68fee93dba))


### Performance

* **adapter:** amortize per-frame command-pool churn in cpu-readback + cuda ([#641](https://github.com/tatolab/streamlib/issues/641)) ([2b08c21](https://github.com/tatolab/streamlib/commit/2b08c21cb3bc0b70bac67e2497264fe495abbc99))

## [0.4.26](https://github.com/tatolab/streamlib/compare/v0.4.25...v0.4.26) (2026-05-01)


### Features

* **adapter-cuda:** host-flavor crate scaffold + carve-out test ([#587](https://github.com/tatolab/streamlib/issues/587)) ([#592](https://github.com/tatolab/streamlib/issues/592)) ([0092e03](https://github.com/tatolab/streamlib/commit/0092e032f8200bfa518e149ba0372a06e72d0350))
* **adapter-cuda:** OPAQUE_FD plumbing chain for CUDA cdylib runtimes ([#588](https://github.com/tatolab/streamlib/issues/588)) ([#594](https://github.com/tatolab/streamlib/issues/594)) ([ed372da](https://github.com/tatolab/streamlib/commit/ed372da61367253f849fb3a13f7414ede2f78687))
* **adapter-cuda:** polyglot E2E with concrete CUDA inference ([#591](https://github.com/tatolab/streamlib/issues/591)) ([#598](https://github.com/tatolab/streamlib/issues/598)) ([156b29b](https://github.com/tatolab/streamlib/commit/156b29bda3e9b340dd1e992feb829702e4b4ed64))
* **adapter-cuda:** subprocess CUDA runtimes for Python + Deno cdylibs ([#589](https://github.com/tatolab/streamlib/issues/589) + [#590](https://github.com/tatolab/streamlib/issues/590)) ([#597](https://github.com/tatolab/streamlib/issues/597)) ([5f7a0d8](https://github.com/tatolab/streamlib/commit/5f7a0d825c61c02ffabe00e68af2033c4fe68d97))
* **linux:** VulkanBlendingCompositor + manual-mode vsync render loop + display_info helper ([#607](https://github.com/tatolab/streamlib/issues/607)) ([87f0eef](https://github.com/tatolab/streamlib/commit/87f0eefdad9efb38deabeb004e394209d05ca69e))
* **linux:** VulkanCrtFilmGrain Metal→Vulkan compute port ([#608](https://github.com/tatolab/streamlib/issues/608)) ([9ca916c](https://github.com/tatolab/streamlib/commit/9ca916c92cfba1d8f0c8884e9aed0ee621ea5e6b)), closes [#483](https://github.com/tatolab/streamlib/issues/483)
* **polyglot:** manual + continuous execution mode examples + tests + docs ([#602](https://github.com/tatolab/streamlib/issues/602)) ([3cd02c1](https://github.com/tatolab/streamlib/commit/3cd02c1abbb518d678e097d467508822c9b2aad9))
* **polyglot:** thread-safe escalate IPC + worker-thread iceoryx2 publishing ([#606](https://github.com/tatolab/streamlib/issues/606)) ([11dd780](https://github.com/tatolab/streamlib/commit/11dd780e80bc66a86d6e6f3b38f72f935aef90da))
* **polyglot:** uniform monotonic_now_ns() API across Python + Deno SDKs ([#601](https://github.com/tatolab/streamlib/issues/601)) ([e49b601](https://github.com/tatolab/streamlib/commit/e49b601b7c7e69034d80d5c892b8e1d582dd93db))


### Bug Fixes

* **adapter-cpu-readback:** lazy numpy import on plane views ([#605](https://github.com/tatolab/streamlib/issues/605)) ([3ffca17](https://github.com/tatolab/streamlib/commit/3ffca17ad78b862374c07c9a1e135cc99cba1317))

## [0.4.25](https://github.com/tatolab/streamlib/compare/v0.4.24...v0.4.25) (2026-04-30)


### Features

* **adapter-cpu-readback:** non-blocking try_acquire_cpu_readback wire op + polyglot SDKs ([#544](https://github.com/tatolab/streamlib/issues/544)) ([#546](https://github.com/tatolab/streamlib/issues/546)) ([9f531de](https://github.com/tatolab/streamlib/commit/9f531de2a0feafa8e364f89955cf618e56b31145))
* **adapter-opengl:** subprocess OpenGlContext runtime + polyglot scenario ([#530](https://github.com/tatolab/streamlib/issues/530)) ([#548](https://github.com/tatolab/streamlib/issues/548)) ([487dd20](https://github.com/tatolab/streamlib/commit/487dd2008f2938895c8584abf8565fe543a241d7))
* **adapter-skia:** GL backend (Skia-on-OpenGL composition) ([#576](https://github.com/tatolab/streamlib/issues/576)) ([#579](https://github.com/tatolab/streamlib/issues/579)) ([da9ac51](https://github.com/tatolab/streamlib/commit/da9ac5170df11eed51d6848b543445e98ed72a5a))
* **adapter-skia:** rewire Python wrapper onto GL backend + polyglot E2E + crash test ([#580](https://github.com/tatolab/streamlib/issues/580)) ([6a843b4](https://github.com/tatolab/streamlib/commit/6a843b4c9b8f8650e2915ee814443aab7427541f))
* **adapter-skia:** Skia surface adapter — host crate + polyglot Python wrapper ([#513](https://github.com/tatolab/streamlib/issues/513)) ([#578](https://github.com/tatolab/streamlib/issues/578)) ([f5c2d25](https://github.com/tatolab/streamlib/commit/f5c2d2595f01d24d9a2496aa51cb4e731af6dbd4))
* **adapter-vulkan:** escalate-IPC RegisterComputeKernel + RunComputeKernel ([#571](https://github.com/tatolab/streamlib/issues/571)) ([b01bb4b](https://github.com/tatolab/streamlib/commit/b01bb4b9ae8fabcd96558460fdcaf0c1bf2de692))
* **adapter-vulkan:** subprocess VulkanContext runtime + polyglot scenario ([#531](https://github.com/tatolab/streamlib/issues/531)) ([#549](https://github.com/tatolab/streamlib/issues/549)) ([4795a60](https://github.com/tatolab/streamlib/commit/4795a60c9f85783a5266c78195b4bdedf6dbf821))
* **ci:** boundary-grep CI gate — five invariants for the Vulkan RHI capability split ([#570](https://github.com/tatolab/streamlib/issues/570)) ([3b35f40](https://github.com/tatolab/streamlib/commit/3b35f40b8323236c7aa2c618095b99bc581edcad))
* **python:** concrete StreamlibSurface dataclass ([#581](https://github.com/tatolab/streamlib/issues/581)) ([#582](https://github.com/tatolab/streamlib/issues/582)) ([00f4591](https://github.com/tatolab/streamlib/commit/00f45914779b02d3ddbfa7ab118d3b0b471bc03b))
* **rhi:** host-side TextureReadback API + migrate polyglot examples ([#583](https://github.com/tatolab/streamlib/issues/583)) ([#585](https://github.com/tatolab/streamlib/issues/585)) ([3838a61](https://github.com/tatolab/streamlib/commit/3838a61cfa12c6f00158aa8562ab7bae63e53f33))

## [0.4.24](https://github.com/tatolab/streamlib/compare/v0.4.23...v0.4.24) (2026-04-27)


### Features

* **adapter-abi:** SurfaceAdapter trait + StreamlibSurface descriptor types ([#521](https://github.com/tatolab/streamlib/issues/521)) ([d25d77f](https://github.com/tatolab/streamlib/commit/d25d77f9fe71db1905a0bba4aae8a21e0a86e73d))
* **adapter-cpu-readback:** explicit GPU→CPU surface adapter crate ([#514](https://github.com/tatolab/streamlib/issues/514)) ([#527](https://github.com/tatolab/streamlib/issues/527)) ([5796040](https://github.com/tatolab/streamlib/commit/5796040e8550ada540cdfaddd789817f5ad26873))
* **adapter-cpu-readback:** NV12 + multi-plane format support ([#536](https://github.com/tatolab/streamlib/issues/536)) ([8df391a](https://github.com/tatolab/streamlib/commit/8df391a6fd922f10a6d77e5b23aa19aa2554d464))
* **adapter-opengl:** OpenGL/EGL surface adapter crate (closes [#512](https://github.com/tatolab/streamlib/issues/512)) ([#526](https://github.com/tatolab/streamlib/issues/526)) ([6d1620e](https://github.com/tatolab/streamlib/commit/6d1620e3b3b093557f1c1d635490f81aeda32714))
* **adapter-vulkan:** Vulkan-native surface adapter crate + cross-process timeline sync (closes [#511](https://github.com/tatolab/streamlib/issues/511)) ([#524](https://github.com/tatolab/streamlib/issues/524)) ([0d229d3](https://github.com/tatolab/streamlib/commit/0d229d3e92bd397ec440f8c4f2d7976f7210b53f))
* **broker, rhi:** multi-FD SCM_RIGHTS + Rust-side multi-plane import (closes [#423](https://github.com/tatolab/streamlib/issues/423)) ([#462](https://github.com/tatolab/streamlib/issues/462)) ([aa6f0e7](https://github.com/tatolab/streamlib/commit/aa6f0e7c38e9bed571aab688097456c74810ed06))
* **cli:** streamlib-cli logs reads JSONL with filters + --list (closes [#440](https://github.com/tatolab/streamlib/issues/440)) ([#470](https://github.com/tatolab/streamlib/issues/470)) ([0a3c1de](https://github.com/tatolab/streamlib/commit/0a3c1de3ae8f23a31915b4b3c33859bcde11a19d))
* **polyglot:** event-driven wake for reactive runner loops (closes [#153](https://github.com/tatolab/streamlib/issues/153)) ([#488](https://github.com/tatolab/streamlib/issues/488)) ([4eef9c0](https://github.com/tatolab/streamlib/commit/4eef9c0b1724e695ebb78eb70442cdc002568c83))
* **rhi:** host VkImage pool with DMA-BUF export and RT-capable DRM modifier ([#510](https://github.com/tatolab/streamlib/issues/510)) ([#523](https://github.com/tatolab/streamlib/issues/523)) ([6274713](https://github.com/tatolab/streamlib/commit/6274713b2adbe0d4599aa9a5942f077961a08f12))
* **rhi:** multi-input Vulkan compute kernel + GpuContext dispatch helper (closes [#480](https://github.com/tatolab/streamlib/issues/480)) ([#495](https://github.com/tatolab/streamlib/issues/495)) ([2c69c66](https://github.com/tatolab/streamlib/commit/2c69c66eb06359984213c67e135b59f32c5ae760))
* **surface-share:** EPOLLHUP watchdog for crashed-subprocess refcount cleanup ([#522](https://github.com/tatolab/streamlib/issues/522)) ([5bf3d9c](https://github.com/tatolab/streamlib/commit/5bf3d9cc953fbac08aa3de539d636b2fa2d1a794))


### Bug Fixes

* **polyglot-python-native:** one-shot cleanup via try/finally (closes [#469](https://github.com/tatolab/streamlib/issues/469)) ([#471](https://github.com/tatolab/streamlib/issues/471)) ([b72b04a](https://github.com/tatolab/streamlib/commit/b72b04a0de030c7841566562d2073d15aafbe338))
* **polyglot:** wire halftone + grayscale examples to shipped escalate API (closes [#475](https://github.com/tatolab/streamlib/issues/475)) ([#478](https://github.com/tatolab/streamlib/issues/478)) ([5d17473](https://github.com/tatolab/streamlib/commit/5d1747339a059d8159156849c41821e25c52ecf8))


### Performance

* **adapter-cpu-readback:** per-submit fence wait instead of vkQueueWaitIdle ([#539](https://github.com/tatolab/streamlib/issues/539)) ([6786eb6](https://github.com/tatolab/streamlib/commit/6786eb674385e7cb3c9c6ecfbde76c770c99d712))
* **thread-runner:** eventfd shutdown for truly-zero idle CPU ([#492](https://github.com/tatolab/streamlib/issues/492)) ([71cdeaf](https://github.com/tatolab/streamlib/commit/71cdeafc7705e0fb9d3d8ee20dd6a5122b2b50cb))

## [0.4.23](https://github.com/tatolab/streamlib/compare/v0.4.22...v0.4.23) (2026-04-24)


### Features

* **ci:** lockout enforcement — clippy disallowed-macros + xtask lint-logging + CI wiring (closes [#441](https://github.com/tatolab/streamlib/issues/441)) ([#455](https://github.com/tatolab/streamlib/issues/455)) ([52a31e0](https://github.com/tatolab/streamlib/commit/52a31e05f7e5fe43f5041074a48517a6676a0494))
* **logging:** fd-level stdio interceptor (closes [#438](https://github.com/tatolab/streamlib/issues/438)) ([#449](https://github.com/tatolab/streamlib/issues/449)) ([80aa8ae](https://github.com/tatolab/streamlib/commit/80aa8ae6b856635e24cb121850a1fbe00494822c))
* **logging:** release-build trace-strip + opt-in strip_debug_logging (closes [#439](https://github.com/tatolab/streamlib/issues/439)) ([#456](https://github.com/tatolab/streamlib/issues/456)) ([7a0b7d3](https://github.com/tatolab/streamlib/commit/7a0b7d3786f0fd1503a3969661ff93bf053c7f7d))
* **logging:** unified JSONL producer + stdout mirror + schema ([#437](https://github.com/tatolab/streamlib/issues/437)) ([#445](https://github.com/tatolab/streamlib/issues/445)) ([bd0417d](https://github.com/tatolab/streamlib/commit/bd0417dc2f9f667b09b62877dfd1505e0ddc317a))
* **polyglot-deno:** streamlib.log API + interceptors + print audit (closes [#444](https://github.com/tatolab/streamlib/issues/444)) ([#453](https://github.com/tatolab/streamlib/issues/453)) ([24fae9b](https://github.com/tatolab/streamlib/commit/24fae9b78f8964963df4c5dee403de8f65e36151))
* **polyglot-python:** streamlib.log API + interceptors + print audit (closes [#443](https://github.com/tatolab/streamlib/issues/443)) ([#452](https://github.com/tatolab/streamlib/issues/452)) ([2e0c0a3](https://github.com/tatolab/streamlib/commit/2e0c0a3c12f3b6d21f7a55240de59274835f7380))
* **polyglot:** dedicated IPC fd pair — free fd1/fd2 for log capture (closes [#451](https://github.com/tatolab/streamlib/issues/451)) ([#454](https://github.com/tatolab/streamlib/issues/454)) ([0a3d0f2](https://github.com/tatolab/streamlib/commit/0a3d0f2e7a6c46963d73e7fc34b112d3148cc672))
* **polyglot:** escalate-IPC {op:"log"} wire format + host handler (closes [#442](https://github.com/tatolab/streamlib/issues/442)) ([#450](https://github.com/tatolab/streamlib/issues/450)) ([3430142](https://github.com/tatolab/streamlib/commit/34301420d981234e447aeb45b051df83b7c4ca4f))


### Bug Fixes

* **ci:** xtask lint-logging covers Rust (drops glslc requirement) ([#461](https://github.com/tatolab/streamlib/issues/461)) ([4b528c5](https://github.com/tatolab/streamlib/commit/4b528c5085dab03988be2e2f8e30762f16bec00c))


### Performance

* **logging:** criterion bench harness + strace no-syscalls test (closes [#447](https://github.com/tatolab/streamlib/issues/447)) ([#460](https://github.com/tatolab/streamlib/issues/460)) ([39e66a3](https://github.com/tatolab/streamlib/commit/39e66a3de8e9d19973de5242e691556eaf93964e))

## [0.4.22](https://github.com/tatolab/streamlib/compare/v0.4.21...v0.4.22) (2026-04-23)


### Features

* **polyglot:** add acquire_texture op to escalate IPC ([#391](https://github.com/tatolab/streamlib/issues/391)) ([4f23066](https://github.com/tatolab/streamlib/commit/4f230661cdbbb5c74d8b1ab120a3e2e05453fcc5)), closes [#369](https://github.com/tatolab/streamlib/issues/369)
* **polyglot:** Linux polyglot DMA-BUF FD consumer in native libs ([#394](https://github.com/tatolab/streamlib/issues/394)) ([#419](https://github.com/tatolab/streamlib/issues/419)) ([e4452f6](https://github.com/tatolab/streamlib/commit/e4452f676fda22386a36b179c0afab14825806c9))
* **polyglot:** Vulkan DMA-BUF import in native-lib consumers ([#420](https://github.com/tatolab/streamlib/issues/420)) ([#432](https://github.com/tatolab/streamlib/issues/432)) ([c931597](https://github.com/tatolab/streamlib/commit/c931597f651a66eadb8b441e58ebe9056a2742b1))
* **runtime:** own internal surface-sharing broker per runtime ([#428](https://github.com/tatolab/streamlib/issues/428)) ([#434](https://github.com/tatolab/streamlib/issues/434)) ([ef3068a](https://github.com/tatolab/streamlib/commit/ef3068a9d94f8fa1e8f6298f74ab5ae15ee28ff5))
* **xtask:** support JTD discriminator schemas in generate-schemas ([#370](https://github.com/tatolab/streamlib/issues/370)) ([#384](https://github.com/tatolab/streamlib/issues/384)) ([784a5cc](https://github.com/tatolab/streamlib/commit/784a5ccb7696d2b0a340132f59dbe5f64c4146f5))


### Bug Fixes

* **#386:** deno type errors + propagate max_payload_bytes to subprocess read buffers ([#410](https://github.com/tatolab/streamlib/issues/410)) ([15df042](https://github.com/tatolab/streamlib/commit/15df042908a2fee898e6c1ce04ba41aa8cbc5a6c))
* **xtask:** rewrite Python class names in jtd-codegen post-processor ([#396](https://github.com/tatolab/streamlib/issues/396)) ([1f5ad72](https://github.com/tatolab/streamlib/commit/1f5ad72c09e04ab76e6b443f6d7770e697cd179e)), closes [#388](https://github.com/tatolab/streamlib/issues/388)

## [0.4.21](https://github.com/tatolab/streamlib/compare/v0.4.20...v0.4.21) (2026-04-20)


### Features

* **#306:** expose encoder quality_level with real-time default ([#329](https://github.com/tatolab/streamlib/issues/329)) ([17223a8](https://github.com/tatolab/streamlib/commit/17223a855ad0f3079c74384adda7c4e62cca9285))
* **#323:** GpuContextLimitedAccess::escalate() primitive ([#362](https://github.com/tatolab/streamlib/issues/362)) ([08162ba](https://github.com/tatolab/streamlib/commit/08162bafdfb7da04d1c2004bceb12ab8b915d1d5))
* **#324:** restrict GpuContextLimitedAccess API surface to Sandbox ops ([#365](https://github.com/tatolab/streamlib/issues/365)) ([f257e42](https://github.com/tatolab/streamlib/commit/f257e42f5bdbe671fc7bcb485815a31a1bdb3a1a))
* **#325:** polyglot escalate-on-behalf IPC for Python/Deno subprocesses ([#368](https://github.com/tatolab/streamlib/issues/368)) ([14aa37f](https://github.com/tatolab/streamlib/commit/14aa37ff7c5ef0b60f8dae2df4ad918e3111319c))
* **#350:** typed capability ctx for Deno polyglot SDK ([#366](https://github.com/tatolab/streamlib/issues/366)) ([72e4d9f](https://github.com/tatolab/streamlib/commit/72e4d9f1707212f95c5003fa908eb44d9e0fd241))
* **#351:** typed capability ctx for Python polyglot SDK ([#379](https://github.com/tatolab/streamlib/issues/379)) ([84c1779](https://github.com/tatolab/streamlib/commit/84c1779a6d166ab0881fb1336edb00d4f793421f))


### Bug Fixes

* **#291:** request transfer queue family in DeviceQueueCreateInfo ([#313](https://github.com/tatolab/streamlib/issues/313)) ([8505874](https://github.com/tatolab/streamlib/commit/8505874c98b20e4c86bb414043aa0145eefc9ebe))
* **#296:** size render_finished semaphore per swapchain image ([#317](https://github.com/tatolab/streamlib/issues/317)) ([b2aa513](https://github.com/tatolab/streamlib/commit/b2aa5137cb4292a20db854a515bcf336ccdb1cc3))
* **#300:** align rgb_to_nv12 source image profile with encode session ([#318](https://github.com/tatolab/streamlib/issues/318)) ([7918222](https://github.com/tatolab/streamlib/commit/791822261d82db5a7130f429a78018e007c2c401))
* **#304:** serialize processor setup() via GpuContext mutex ([#327](https://github.com/tatolab/streamlib/issues/327)) ([ee4c9af](https://github.com/tatolab/streamlib/commit/ee4c9afd13d325baaddd6c8d44e741a88587c132))
* **#315:** enable samplerYcbcrConversion and split RGB→NV12 into compute + encode images ([#331](https://github.com/tatolab/streamlib/issues/331)) ([7d7c65a](https://github.com/tatolab/streamlib/commit/7d7c65a97efdf640117c5e1efaf4b099e2d837d1))
* **#316:** use UNDEFINED old_layout on swapchain pre-render barrier ([#332](https://github.com/tatolab/streamlib/issues/332)) ([69639bf](https://github.com/tatolab/streamlib/commit/69639bfb0f46ed9c7ab93e1a6804438f0a5ff8df))
* **#358:** make example workspace compile on Linux ([#371](https://github.com/tatolab/streamlib/issues/371)) ([78803ba](https://github.com/tatolab/streamlib/commit/78803ba4af1788fcf50a016db9f57c7a14cfa769))
* **#359:** pin clack and vulkanalia git deps to specific revs ([#367](https://github.com/tatolab/streamlib/issues/367)) ([f191abf](https://github.com/tatolab/streamlib/commit/f191abfa193dcb2ce43fb6e254e2c0b86d709e89))
* **#361:** stabilize pubsub tests against iceoryx2 concurrent-node races ([#378](https://github.com/tatolab/streamlib/issues/378)) ([85c0f26](https://github.com/tatolab/streamlib/commit/85c0f26fa7bbb364ea7530a17a3757b8ad46aa98)), closes [#361](https://github.com/tatolab/streamlib/issues/361)

## [0.4.20](https://github.com/tatolab/streamlib/compare/v0.4.19...v0.4.20) (2026-04-19)


### Bug Fixes

* **#287:** bind vulkanalia builder slice temporaries to locals ([#295](https://github.com/tatolab/streamlib/issues/295)) ([469f3a1](https://github.com/tatolab/streamlib/commit/469f3a193677367ce832aa4d78ea157a138d7569))
* **#288:** add COPY_SRC usage to camera ring textures ([#298](https://github.com/tatolab/streamlib/issues/298)) ([c39a917](https://github.com/tatolab/streamlib/commit/c39a91740e872024e11d3c94e2c176d328b9a834))
* **#289:** chain VkSamplerYcbcrConversionInfo on NV12 image views ([#299](https://github.com/tatolab/streamlib/issues/299)) ([d790f28](https://github.com/tatolab/streamlib/commit/d790f28b17fdbb20cd54f460c9797c16edfe619b))
* **#290:** chain external memory info on DMA-BUF VMA pool probes ([#311](https://github.com/tatolab/streamlib/issues/311)) ([ff898ca](https://github.com/tatolab/streamlib/commit/ff898cace5ce59f897ff6a1770a9d8035dd21271))
* **#292:** pre-allocate encode/decode DMA-BUF resources before swapchain ([#301](https://github.com/tatolab/streamlib/issues/301)) ([d63027b](https://github.com/tatolab/streamlib/commit/d63027b53ab13492447a18427be14e920dc04bb8))
* **#302:** derive decoder probe extent from decoder config ([#309](https://github.com/tatolab/streamlib/issues/309)) ([48206b2](https://github.com/tatolab/streamlib/commit/48206b223ff05c71f0ee1f0246fe2817e9638438))
* **#303:** remove pre-poll in camera MMAP path so stream starts on strict V4L2 drivers ([#307](https://github.com/tatolab/streamlib/issues/307)) ([a5f778d](https://github.com/tatolab/streamlib/commit/a5f778d333a01b8fc7a7943c6a86f071902678de))

## [0.4.19](https://github.com/tatolab/streamlib/compare/v0.4.18...v0.4.19) (2026-04-18)


### Bug Fixes

* **#278:** hold device-level resource lock during video session and DPB/bitstream setup ([#284](https://github.com/tatolab/streamlib/issues/284)) ([f847e90](https://github.com/tatolab/streamlib/commit/f847e903aa1a8d2fbd02c082175cadc0eb5cbd2f))

## [0.4.18](https://github.com/tatolab/streamlib/compare/v0.4.17...v0.4.18) (2026-04-18)


### Features

* **#272:** lazy encoder init — use camera fps for VUI timing ([abeae17](https://github.com/tatolab/streamlib/commit/abeae173b4705295e4ce430d59c6f8bbd5e7cc2e))
* **#272:** propagate FPS through pipeline via Videoframe schema ([3b5bf33](https://github.com/tatolab/streamlib/commit/3b5bf33f0d346f9cff2b4df8ecef77994a16936d))
* **#272:** propagate FPS through pipeline via Videoframe schema ([3b5bf33](https://github.com/tatolab/streamlib/commit/3b5bf33f0d346f9cff2b4df8ecef77994a16936d))
* **#272:** propagate FPS through pipeline via Videoframe schema ([c8085cc](https://github.com/tatolab/streamlib/commit/c8085cc500f7721916357888cca09bea034eff46))


### Bug Fixes

* **#272:** compute queue for Nv12ToRgbConverter + RGBA decoder output ([3bdc8da](https://github.com/tatolab/streamlib/commit/3bdc8da5d8fa5ecb0863481e4d28ffb680d53a4a))
* **#272:** integrate GPU NV12→RGBA shader into SimpleDecoder ([d511afd](https://github.com/tatolab/streamlib/commit/d511afdad55a73758a3363fc7481eb3e6f55c8e7))
* **#272:** NV12 passthrough + direct H.265 mux for roundtrip output ([630ff60](https://github.com/tatolab/streamlib/commit/630ff604e6a9e5ebf382807e7dbd353084e6acb0))
* **#272:** pre-initialize decoder video session before swapchain ([166a95a](https://github.com/tatolab/streamlib/commit/166a95a8c95ff656c86518a8a8a327a31b00a0e2))
* **#272:** revert encoder to eager init (DMA-BUF allocation ordering) ([3772400](https://github.com/tatolab/streamlib/commit/3772400945f01c9dc4d43a5907b7c727c1bb1a3a))
* **#272:** VMA fix for codec examples + PSNR verification ([4efc0c3](https://github.com/tatolab/streamlib/commit/4efc0c3964c4fda6777a94a25c7d71cd91adca24))
* **#273:** add per-queue mutex synchronization to VulkanDevice ([d359749](https://github.com/tatolab/streamlib/commit/d35974909ae97422db376a54aa88594dfaf3c101))
* **#273:** add per-queue mutex synchronization to VulkanDevice ([b76bc0e](https://github.com/tatolab/streamlib/commit/b76bc0e5cf1f4438013523cdc356834155c80605))
* **#277:** route vulkan-video queue submits through VulkanDevice mutexes ([#283](https://github.com/tatolab/streamlib/issues/283)) ([f230a4a](https://github.com/tatolab/streamlib/commit/f230a4aa5b018ebb47203a350967d62b626b0d8d))

## [0.4.17](https://github.com/tatolab/streamlib/compare/v0.4.16...v0.4.17) (2026-04-17)


### Features

* **#270:** add fps to encoder config, create [#272](https://github.com/tatolab/streamlib/issues/272) for pipeline FPS propagation ([440b586](https://github.com/tatolab/streamlib/commit/440b5865c1ef9120fa1f285b8162637cac9c4405))
* **#270:** encoder/decoder processors use shared RHI VulkanDevice ([e74a69e](https://github.com/tatolab/streamlib/commit/e74a69e954c76258bab3733fbc17b95116cdb1c8))
* **#270:** shared device API — VideoContext::from_external + from_device constructors ([02461f5](https://github.com/tatolab/streamlib/commit/02461f5fbfa405540dec323002330afbf1cd25c9))
* add BgraFileSource processor + vulkan-video-roundtrip pipeline example ([0ae2158](https://github.com/tatolab/streamlib/commit/0ae215865f8d06cd74fe4436a5f5030a3a0b4939))
* **example:** live camera encode pipeline via vivid virtual device ([4afc96a](https://github.com/tatolab/streamlib/commit/4afc96a1078fa834f96ec804b96c5a33fc261bce))
* GPU-resident VkImage pipeline ([#253](https://github.com/tatolab/streamlib/issues/253)) ([901358f](https://github.com/tatolab/streamlib/commit/901358f65204a82d1b3b697ee7b7d57b9972ab1e))
* integrate nvpro-vulkan-video crate as libs/vulkan-video ([9da4ded](https://github.com/tatolab/streamlib/commit/9da4ded1d3fac0172f635063a49dab1fab27c84c))
* **processors:** add H.264/H.265 encoder/decoder processors via vulkan-video ([e46bde6](https://github.com/tatolab/streamlib/commit/e46bde6e6d121e659a428ebf524a08ad22513c87)), closes [#254](https://github.com/tatolab/streamlib/issues/254)
* **processors:** add LinuxMp4WriterProcessor for encoded video → MP4 ([b99c90c](https://github.com/tatolab/streamlib/commit/b99c90ca97e297171164e5ff9bf3e45f0d635448))
* **vulkan-video:** integrate nvpro-vulkan-video as libs/vulkan-video ([51b4d32](https://github.com/tatolab/streamlib/commit/51b4d325bc82e9f137819f1efe86b57ba3de3ea9))


### Bug Fixes

* **#270:** correct FPS — encoder now uses 60fps, MP4 writer adds -r flag ([3a08365](https://github.com/tatolab/streamlib/commit/3a0836563f29af0b79621352d9665b04ae3d1d1e))
* **#270:** use dedicated transfer + compute queues for encoder ([1e25a6e](https://github.com/tatolab/streamlib/commit/1e25a6e05b4dc6be73bc40edee5286868e8b87a4))
* restructure verify-video as proper skill folder with SKILL.md ([a87d3e6](https://github.com/tatolab/streamlib/commit/a87d3e6edddd7e9b2fd5661e37d2914304a5f3b5))
* share a single device ([#270](https://github.com/tatolab/streamlib/issues/270)). ([71891a0](https://github.com/tatolab/streamlib/commit/71891a047691d52f512c504bc96182c8b204b385))
* throttle file source to real-time FPS + use read_next_in_order for encoders ([17bea2d](https://github.com/tatolab/streamlib/commit/17bea2d5c4fce977df026a2403d333bf9bbf4a13))

## [0.4.16](https://github.com/tatolab/streamlib/compare/v0.4.15...v0.4.16) (2026-04-17)


### Bug Fixes

* **camera:** add poll() guard to MMAP capture path for clean shutdown ([ba1cbab](https://github.com/tatolab/streamlib/commit/ba1cbabb9817333b137b332e4dc55b0887c8e183))
* **display:** clean exit via EventLoopProxy wake-up on shutdown ([c55e3b2](https://github.com/tatolab/streamlib/commit/c55e3b2551bb9fbf8bb2ec8c284f64d180d90f24))
* **display:** clean exit via EventLoopProxy wake-up on shutdown ([f702948](https://github.com/tatolab/streamlib/commit/f70294835d3a73ef2b785f9d45708a11fb98526b)), closes [#236](https://github.com/tatolab/streamlib/issues/236)
* **pubsub:** debug_assert on temporary Arc passed to subscribe() ([f201a31](https://github.com/tatolab/streamlib/commit/f201a311bbba6c9bfca3ec65d9a31d2e0c48dfd9))
* **runtime:** keep ShutdownListener alive + display publishes RuntimeShutdown on internal exit ([d1bfb1e](https://github.com/tatolab/streamlib/commit/d1bfb1e06c5ade73505b4aa86da8f0bca86ca85c))

## [0.4.15](https://github.com/tatolab/streamlib/compare/v0.4.14...v0.4.15) (2026-04-16)


### Bug Fixes

* **vulkan:** VMA pool isolation for DMA-BUF allocations ([cab6a00](https://github.com/tatolab/streamlib/commit/cab6a000865918324e175854947f0fbe1fd8fef7))

## [0.4.14](https://github.com/tatolab/streamlib/compare/v0.4.13...v0.4.14) (2026-04-16)


### Features

* **ipc:** migrate iceoryx2 to slice API — eliminate stack overflow ([#258](https://github.com/tatolab/streamlib/issues/258)) ([a4d7d5e](https://github.com/tatolab/streamlib/commit/a4d7d5eadaa77338767b40484eb999cc5d12a6e8))


### Bug Fixes

* **moq:** MoQ subgroup keyframe fix — pin 0.14.1 + keyframe_interval_seconds ([#256](https://github.com/tatolab/streamlib/issues/256)) ([aee51ca](https://github.com/tatolab/streamlib/commit/aee51ca9dc73e876d4137a90a035f6b25136e0a4))

## [0.4.13](https://github.com/tatolab/streamlib/compare/v0.4.12...v0.4.13) (2026-04-15)


### Bug Fixes

* quote description in 217 plan to fix amos YAML parse error. ([40cc887](https://github.com/tatolab/streamlib/commit/40cc8872207f427bdac79219ee2348cb8ca0f963))

## [0.4.12](https://github.com/tatolab/streamlib/compare/v0.4.11...v0.4.12) (2026-04-05)


### Bug Fixes

* **moq:** configure QUIC keep-alive to prevent Cloudflare relay idle timeout ([#238](https://github.com/tatolab/streamlib/issues/238)) ([ab5fd60](https://github.com/tatolab/streamlib/commit/ab5fd60914bd98ddac9f1c9693a355077d43e588))
* **moq:** configure QUIC keep-alive to prevent relay idle timeout ([2349c30](https://github.com/tatolab/streamlib/commit/2349c30d17848ec980112baa4d1c1607529a9555))

## [0.4.11](https://github.com/tatolab/streamlib/compare/v0.4.10...v0.4.11) (2026-04-05)


### Features

* add composable codec processors and full MoQ roundtrip example ([5b95416](https://github.com/tatolab/streamlib/commit/5b954165a96ec1d93343b5b7da2992a82aba6d5e))
* **encoder:** configurable H264 profile, IPC buffer fix, broadcast defaults ([df4c34c](https://github.com/tatolab/streamlib/commit/df4c34c05cc79bb815deb45fb2549d48cfee2a58))
* **moq:** add MoQ feature flag and dependencies ([#218](https://github.com/tatolab/streamlib/issues/218)) ([0a9a412](https://github.com/tatolab/streamlib/commit/0a9a41207406cf8bcedb39aa5134fb1f876dd6b8))
* **moq:** add MoQ publish and subscribe examples ([#229](https://github.com/tatolab/streamlib/issues/229)) ([c657fff](https://github.com/tatolab/streamlib/commit/c657fff76e1ba39d5eea73b9096936bf98077002))
* **moq:** add MoQ roundtrip latency example ([#228](https://github.com/tatolab/streamlib/issues/228)) ([927eb52](https://github.com/tatolab/streamlib/commit/927eb529f32cf8b8e01fc6f3ccdba7e01b467003))
* **moq:** add MoQ session, publish, and subscribe primitives ([#219](https://github.com/tatolab/streamlib/issues/219), [#220](https://github.com/tatolab/streamlib/issues/220), [#221](https://github.com/tatolab/streamlib/issues/221)) ([408d7a1](https://github.com/tatolab/streamlib/commit/408d7a10927acf7fd18ef94bc5354bfee402a0bb))
* **moq:** add MoQ subscribe processor for ingesting data into graph ([#227](https://github.com/tatolab/streamlib/issues/227)) ([771be3e](https://github.com/tatolab/streamlib/commit/771be3e795fb9dd4419322d9ccff181feab52ef0))
* **moq:** add MoQ transport layer alongside iceoryx2 ([#217](https://github.com/tatolab/streamlib/issues/217)) ([1f713b3](https://github.com/tatolab/streamlib/commit/1f713b357db36b50acfc2ea2a504d51938311e9d))
* **moq:** add moq_fanout flag to PortDescriptor ([#224](https://github.com/tatolab/streamlib/issues/224)) ([e2a0ad8](https://github.com/tatolab/streamlib/commit/e2a0ad8adc02d9cbd4b743e85011fa756d5baa74))
* **moq:** add moq-av-publish example with dual video+audio tracks ([ca6738d](https://github.com/tatolab/streamlib/commit/ca6738d55f7394d7e80166445586eb7345767e31))
* **moq:** add moq-av-subscribe example with dual video+audio tracks ([51f6167](https://github.com/tatolab/streamlib/commit/51f6167f1c1a4d155a5101de33c25b6718f6d8a6))
* **moq:** add MoqDecodeSubscribeProcessor and real A/V subscriber example ([271af76](https://github.com/tatolab/streamlib/commit/271af768e89a8c76470ad3b558e674649e660e3d))
* **moq:** add MoqLinkTransportConfig to Link ([#225](https://github.com/tatolab/streamlib/issues/225)) ([e047a95](https://github.com/tatolab/streamlib/commit/e047a954ac623f2c58ac974f9bb2c32c8aa79438))
* **moq:** add schema-agnostic data example ([#230](https://github.com/tatolab/streamlib/issues/230)) ([5fb8b6c](https://github.com/tatolab/streamlib/commit/5fb8b6c891b37ee6e124af6c78f20ea1cf9abedc))
* **moq:** compiler wiring for MoQ-annotated links ([#223](https://github.com/tatolab/streamlib/issues/223)) ([44a3bb7](https://github.com/tatolab/streamlib/commit/44a3bb72cbcd7fa2b1fdc7b30e89636adaff34a4))
* **moq:** extend OutputWriter with MoQ remote destinations ([#222](https://github.com/tatolab/streamlib/issues/222)) ([fca3609](https://github.com/tatolab/streamlib/commit/fca36099027097877cbf4b842fc4ef3cc0c698cc))
* **moq:** moq-subscribe saves H.264 to file, verified end-to-end pipeline ([5f2bc83](https://github.com/tatolab/streamlib/commit/5f2bc83b3bea614b79e39d3637a34b7990d503b5))
* **moq:** replace moq-lite with cloudflare/moq-rs (moq-transport) ([10d7c04](https://github.com/tatolab/streamlib/commit/10d7c04d62ad5b941c83352100185257f340971e))
* **moq:** replace synthetic moq-av-publish with real camera+mic capture ([03524f2](https://github.com/tatolab/streamlib/commit/03524f2c64aea9bb11eec627321ae8d0f7cb93c0))
* **moq:** schema-to-track mapping and MoQ catalog generation ([#226](https://github.com/tatolab/streamlib/issues/226)) ([6c1f08d](https://github.com/tatolab/streamlib/commit/6c1f08d738a8ad6d47cea118daf3fc06b40ca3e8))
* **moq:** shared sessions, auto-track names, populated catalog, graceful unconnected ports ([63223b5](https://github.com/tatolab/streamlib/commit/63223b56ca0e649cf3fd6ad8cdcc750f270c6f44))


### Bug Fixes

* **codegen:** wire schema read_mode and buffer_size into macro-generated port config ([2816037](https://github.com/tatolab/streamlib/commit/28160376c379e6e4b3db6e3b054a57e77f1b621d)), closes [#237](https://github.com/tatolab/streamlib/issues/237)
* **codegen:** wire schema read_mode and buffer_size into port config ([caaed9d](https://github.com/tatolab/streamlib/commit/caaed9df923645267ee06ae08a557bced31e4e41))
* **decoder:** FFmpeg LOW_DELAY, receive_frame loop, monotonic PTS ([eb20758](https://github.com/tatolab/streamlib/commit/eb2075878d93d589e648f65e271784968aa2bbe6))
* **display:** add 8MB stack for render thread ([ad17cec](https://github.com/tatolab/streamlib/commit/ad17cece55749a8af3ca92d02ec29ad7bfc835ab))
* **encoder:** DPB slot count, P-frame reference type, payload sizing ([3f6d6f5](https://github.com/tatolab/streamlib/commit/3f6d6f55219bd99d110d156f971d999eb9e7874f))
* **encoder:** lower default keyframe interval from 60 to 15 frames ([ef7e1c6](https://github.com/tatolab/streamlib/commit/ef7e1c6dca6a156527c7ffbee47d1003f8f73ce0))
* **example:** add resampler + rechunker on subscribe audio path ([bafdefc](https://github.com/tatolab/streamlib/commit/bafdefc958b18b84e3d509f5be34a3d826f53381))
* **h264:** defer encoder creation to first frame, use actual frame dimensions ([aaf35ad](https://github.com/tatolab/streamlib/commit/aaf35ad20d18e3fae4a7900dbcd54075e994fe99))
* **h264:** use Baseline profile matching old working encoder ([3aaf19c](https://github.com/tatolab/streamlib/commit/3aaf19cb8076482a5915a70cd952929fa3aa96d7))
* **ipc:** ReadNextInOrder for all wired ports, fix keyframe detection ([527d1ae](https://github.com/tatolab/streamlib/commit/527d1aef981530bec1c5b03741fb48418a6cc42c))
* **ipc:** respect macro-generated port config, add has_port guard ([01b8b69](https://github.com/tatolab/streamlib/commit/01b8b6921d8b5daf569253cc68e9632544615677))
* **moq:** align MoqDecodeSubscribeProcessor video decode path with WHEP ([5dce5bc](https://github.com/tatolab/streamlib/commit/5dce5bcb6ef6349f3e78e36abbaa842571d07de6))
* **moq:** capture tokio handle for subscribe_track on processor threads ([7872e23](https://github.com/tatolab/streamlib/commit/7872e23603300f4568d6445c23cbbaa414f994ea))
* **moq:** defer relay connect to first frame to avoid FD safety crash ([2a76211](https://github.com/tatolab/streamlib/commit/2a76211ad09f746d6c84a603185226221ec6bd22))
* **moq:** eager relay connect and subscribe retry for A/V processors ([fb5863a](https://github.com/tatolab/streamlib/commit/fb5863af9f8eb6666acebaa6dae5207604703614))
* **moq:** fix broadcast path and subscribe flow for relay compatibility ([b0d1864](https://github.com/tatolab/streamlib/commit/b0d1864dfecc85c43ac27c41f24d7ef8b733c939))
* **moq:** namespace routing and session_started for A/V pipeline ([9136074](https://github.com/tatolab/streamlib/commit/91360748431f3c17ba7d7dbc3eb18775934b503e))
* **moq:** per-GOP subgroup grouping eliminates subscribe drops ([a8bacc9](https://github.com/tatolab/streamlib/commit/a8bacc9dcf958ffc0507c4c03da33646540e3483))
* **moq:** prepend SPS+PPS to IDR frames for decoder and restore IDR gate ([3c69618](https://github.com/tatolab/streamlib/commit/3c69618fdbceef4fc6bb93655e581f960a30ba40))
* **moq:** remove moq_fanout remnant, default to Cloudflare relay, auto-generate broadcast path ([f9c188b](https://github.com/tatolab/streamlib/commit/f9c188bb988df4527ee7432494a72c4c28d74c9a))
* **moq:** remove url config, hardcode Cloudflare relay, add multi-track example ([0aa893d](https://github.com/tatolab/streamlib/commit/0aa893d64826b70cc2ebbf290766d1483a1e1a18))
* **moq:** restore IDR gate for mid-stream join, investigating SPS/PPS size mismatch ([dc48080](https://github.com/tatolab/streamlib/commit/dc48080b3eb15c9b72663f40c68beb76ee1f7f80))
* **moq:** simplify MoqPublishTrack/MoqSubscribeTrack config and rewrite example ([df30a69](https://github.com/tatolab/streamlib/commit/df30a698b56e0721af994f0d94b21c97ad1093df))
* **moq:** skip P-frames until first IDR and prepend SPS/PPS to keyframes ([2d3950c](https://github.com/tatolab/streamlib/commit/2d3950cf398fe932183b81dae14d066ab24c86cb))
* **moq:** subscribe resilience, keyframe detection, bitrate tuning ([7df7754](https://github.com/tatolab/streamlib/commit/7df775454578220dff9c1f58bd505e2c564f4df9))
* **moq:** subscribe retry with exponential backoff, graceful GPU device lost ([c1b9c12](https://github.com/tatolab/streamlib/commit/c1b9c128f2987cb061a16950faca7ffc0dd415dc))
* **moq:** use processor_id/port_name for MoQ track names to avoid collisions ([807b662](https://github.com/tatolab/streamlib/commit/807b662718436c5a49ac7dbe6edc04a3bee18576))
* **moq:** use with_consume only for subscriber session ([75d20f6](https://github.com/tatolab/streamlib/commit/75d20f6006076db1b85c8eb8565f9e3a128d5173))
* **moq:** wait for broadcast announcement before subscribing ([8f511f8](https://github.com/tatolab/streamlib/commit/8f511f84d4a7e5cb2f4ff548d0a5135c180cb0c3))
* **vulkan:** correct swapchain image layout in DisplayProcessor barrier ([6a52708](https://github.com/tatolab/streamlib/commit/6a52708257ffa33600eab182de1a1c94b4c6e306))
* **vulkan:** remove gpu-allocator, centralize allocation in VulkanDevice RHI ([777beac](https://github.com/tatolab/streamlib/commit/777beac2b7882f247feaea851a4c973f0d48e1de))

## [0.4.10](https://github.com/tatolab/streamlib/compare/v0.4.9...v0.4.10) (2026-04-05)


### Features

* add composable codec processors and full MoQ roundtrip example ([5b95416](https://github.com/tatolab/streamlib/commit/5b954165a96ec1d93343b5b7da2992a82aba6d5e))
* **encoder:** configurable H264 profile, IPC buffer fix, broadcast defaults ([df4c34c](https://github.com/tatolab/streamlib/commit/df4c34c05cc79bb815deb45fb2549d48cfee2a58))
* **moq:** add MoQ feature flag and dependencies ([#218](https://github.com/tatolab/streamlib/issues/218)) ([0a9a412](https://github.com/tatolab/streamlib/commit/0a9a41207406cf8bcedb39aa5134fb1f876dd6b8))
* **moq:** add MoQ publish and subscribe examples ([#229](https://github.com/tatolab/streamlib/issues/229)) ([c657fff](https://github.com/tatolab/streamlib/commit/c657fff76e1ba39d5eea73b9096936bf98077002))
* **moq:** add MoQ roundtrip latency example ([#228](https://github.com/tatolab/streamlib/issues/228)) ([927eb52](https://github.com/tatolab/streamlib/commit/927eb529f32cf8b8e01fc6f3ccdba7e01b467003))
* **moq:** add MoQ session, publish, and subscribe primitives ([#219](https://github.com/tatolab/streamlib/issues/219), [#220](https://github.com/tatolab/streamlib/issues/220), [#221](https://github.com/tatolab/streamlib/issues/221)) ([408d7a1](https://github.com/tatolab/streamlib/commit/408d7a10927acf7fd18ef94bc5354bfee402a0bb))
* **moq:** add MoQ subscribe processor for ingesting data into graph ([#227](https://github.com/tatolab/streamlib/issues/227)) ([771be3e](https://github.com/tatolab/streamlib/commit/771be3e795fb9dd4419322d9ccff181feab52ef0))
* **moq:** add MoQ transport layer alongside iceoryx2 ([#217](https://github.com/tatolab/streamlib/issues/217)) ([1f713b3](https://github.com/tatolab/streamlib/commit/1f713b357db36b50acfc2ea2a504d51938311e9d))
* **moq:** add moq_fanout flag to PortDescriptor ([#224](https://github.com/tatolab/streamlib/issues/224)) ([e2a0ad8](https://github.com/tatolab/streamlib/commit/e2a0ad8adc02d9cbd4b743e85011fa756d5baa74))
* **moq:** add moq-av-publish example with dual video+audio tracks ([ca6738d](https://github.com/tatolab/streamlib/commit/ca6738d55f7394d7e80166445586eb7345767e31))
* **moq:** add moq-av-subscribe example with dual video+audio tracks ([51f6167](https://github.com/tatolab/streamlib/commit/51f6167f1c1a4d155a5101de33c25b6718f6d8a6))
* **moq:** add MoqDecodeSubscribeProcessor and real A/V subscriber example ([271af76](https://github.com/tatolab/streamlib/commit/271af768e89a8c76470ad3b558e674649e660e3d))
* **moq:** add MoqLinkTransportConfig to Link ([#225](https://github.com/tatolab/streamlib/issues/225)) ([e047a95](https://github.com/tatolab/streamlib/commit/e047a954ac623f2c58ac974f9bb2c32c8aa79438))
* **moq:** add schema-agnostic data example ([#230](https://github.com/tatolab/streamlib/issues/230)) ([5fb8b6c](https://github.com/tatolab/streamlib/commit/5fb8b6c891b37ee6e124af6c78f20ea1cf9abedc))
* **moq:** compiler wiring for MoQ-annotated links ([#223](https://github.com/tatolab/streamlib/issues/223)) ([44a3bb7](https://github.com/tatolab/streamlib/commit/44a3bb72cbcd7fa2b1fdc7b30e89636adaff34a4))
* **moq:** extend OutputWriter with MoQ remote destinations ([#222](https://github.com/tatolab/streamlib/issues/222)) ([fca3609](https://github.com/tatolab/streamlib/commit/fca36099027097877cbf4b842fc4ef3cc0c698cc))
* **moq:** moq-subscribe saves H.264 to file, verified end-to-end pipeline ([5f2bc83](https://github.com/tatolab/streamlib/commit/5f2bc83b3bea614b79e39d3637a34b7990d503b5))
* **moq:** replace moq-lite with cloudflare/moq-rs (moq-transport) ([10d7c04](https://github.com/tatolab/streamlib/commit/10d7c04d62ad5b941c83352100185257f340971e))
* **moq:** replace synthetic moq-av-publish with real camera+mic capture ([03524f2](https://github.com/tatolab/streamlib/commit/03524f2c64aea9bb11eec627321ae8d0f7cb93c0))
* **moq:** schema-to-track mapping and MoQ catalog generation ([#226](https://github.com/tatolab/streamlib/issues/226)) ([6c1f08d](https://github.com/tatolab/streamlib/commit/6c1f08d738a8ad6d47cea118daf3fc06b40ca3e8))
* **moq:** shared sessions, auto-track names, populated catalog, graceful unconnected ports ([63223b5](https://github.com/tatolab/streamlib/commit/63223b56ca0e649cf3fd6ad8cdcc750f270c6f44))


### Bug Fixes

* **decoder:** FFmpeg LOW_DELAY, receive_frame loop, monotonic PTS ([eb20758](https://github.com/tatolab/streamlib/commit/eb2075878d93d589e648f65e271784968aa2bbe6))
* **display:** add 8MB stack for render thread ([ad17cec](https://github.com/tatolab/streamlib/commit/ad17cece55749a8af3ca92d02ec29ad7bfc835ab))
* **encoder:** DPB slot count, P-frame reference type, payload sizing ([3f6d6f5](https://github.com/tatolab/streamlib/commit/3f6d6f55219bd99d110d156f971d999eb9e7874f))
* **encoder:** lower default keyframe interval from 60 to 15 frames ([ef7e1c6](https://github.com/tatolab/streamlib/commit/ef7e1c6dca6a156527c7ffbee47d1003f8f73ce0))
* **example:** add resampler + rechunker on subscribe audio path ([bafdefc](https://github.com/tatolab/streamlib/commit/bafdefc958b18b84e3d509f5be34a3d826f53381))
* **h264:** defer encoder creation to first frame, use actual frame dimensions ([aaf35ad](https://github.com/tatolab/streamlib/commit/aaf35ad20d18e3fae4a7900dbcd54075e994fe99))
* **h264:** use Baseline profile matching old working encoder ([3aaf19c](https://github.com/tatolab/streamlib/commit/3aaf19cb8076482a5915a70cd952929fa3aa96d7))
* **ipc:** ReadNextInOrder for all wired ports, fix keyframe detection ([527d1ae](https://github.com/tatolab/streamlib/commit/527d1aef981530bec1c5b03741fb48418a6cc42c))
* **ipc:** respect macro-generated port config, add has_port guard ([01b8b69](https://github.com/tatolab/streamlib/commit/01b8b6921d8b5daf569253cc68e9632544615677))
* **moq:** align MoqDecodeSubscribeProcessor video decode path with WHEP ([5dce5bc](https://github.com/tatolab/streamlib/commit/5dce5bcb6ef6349f3e78e36abbaa842571d07de6))
* **moq:** capture tokio handle for subscribe_track on processor threads ([7872e23](https://github.com/tatolab/streamlib/commit/7872e23603300f4568d6445c23cbbaa414f994ea))
* **moq:** defer relay connect to first frame to avoid FD safety crash ([2a76211](https://github.com/tatolab/streamlib/commit/2a76211ad09f746d6c84a603185226221ec6bd22))
* **moq:** eager relay connect and subscribe retry for A/V processors ([fb5863a](https://github.com/tatolab/streamlib/commit/fb5863af9f8eb6666acebaa6dae5207604703614))
* **moq:** fix broadcast path and subscribe flow for relay compatibility ([b0d1864](https://github.com/tatolab/streamlib/commit/b0d1864dfecc85c43ac27c41f24d7ef8b733c939))
* **moq:** namespace routing and session_started for A/V pipeline ([9136074](https://github.com/tatolab/streamlib/commit/91360748431f3c17ba7d7dbc3eb18775934b503e))
* **moq:** per-GOP subgroup grouping eliminates subscribe drops ([a8bacc9](https://github.com/tatolab/streamlib/commit/a8bacc9dcf958ffc0507c4c03da33646540e3483))
* **moq:** prepend SPS+PPS to IDR frames for decoder and restore IDR gate ([3c69618](https://github.com/tatolab/streamlib/commit/3c69618fdbceef4fc6bb93655e581f960a30ba40))
* **moq:** remove moq_fanout remnant, default to Cloudflare relay, auto-generate broadcast path ([f9c188b](https://github.com/tatolab/streamlib/commit/f9c188bb988df4527ee7432494a72c4c28d74c9a))
* **moq:** remove url config, hardcode Cloudflare relay, add multi-track example ([0aa893d](https://github.com/tatolab/streamlib/commit/0aa893d64826b70cc2ebbf290766d1483a1e1a18))
* **moq:** restore IDR gate for mid-stream join, investigating SPS/PPS size mismatch ([dc48080](https://github.com/tatolab/streamlib/commit/dc48080b3eb15c9b72663f40c68beb76ee1f7f80))
* **moq:** simplify MoqPublishTrack/MoqSubscribeTrack config and rewrite example ([df30a69](https://github.com/tatolab/streamlib/commit/df30a698b56e0721af994f0d94b21c97ad1093df))
* **moq:** skip P-frames until first IDR and prepend SPS/PPS to keyframes ([2d3950c](https://github.com/tatolab/streamlib/commit/2d3950cf398fe932183b81dae14d066ab24c86cb))
* **moq:** subscribe resilience, keyframe detection, bitrate tuning ([7df7754](https://github.com/tatolab/streamlib/commit/7df775454578220dff9c1f58bd505e2c564f4df9))
* **moq:** subscribe retry with exponential backoff, graceful GPU device lost ([c1b9c12](https://github.com/tatolab/streamlib/commit/c1b9c128f2987cb061a16950faca7ffc0dd415dc))
* **moq:** use processor_id/port_name for MoQ track names to avoid collisions ([807b662](https://github.com/tatolab/streamlib/commit/807b662718436c5a49ac7dbe6edc04a3bee18576))
* **moq:** use with_consume only for subscriber session ([75d20f6](https://github.com/tatolab/streamlib/commit/75d20f6006076db1b85c8eb8565f9e3a128d5173))
* **moq:** wait for broadcast announcement before subscribing ([8f511f8](https://github.com/tatolab/streamlib/commit/8f511f84d4a7e5cb2f4ff548d0a5135c180cb0c3))
* STAP-A RTP packetization for reliable SPS/PPS delivery ([#214](https://github.com/tatolab/streamlib/issues/214)) ([8e138b8](https://github.com/tatolab/streamlib/commit/8e138b8b3aaaf4a641f455d7f7095b828d055a55))
* STAP-A RTP packetization for reliable SPS/PPS delivery ([#214](https://github.com/tatolab/streamlib/issues/214)) ([2256485](https://github.com/tatolab/streamlib/commit/22564856ef66f8648b3940c782b2c887c8639b2a))
* **vulkan:** correct swapchain image layout in DisplayProcessor barrier ([6a52708](https://github.com/tatolab/streamlib/commit/6a52708257ffa33600eab182de1a1c94b4c6e306))
* **vulkan:** remove gpu-allocator, centralize allocation in VulkanDevice RHI ([777beac](https://github.com/tatolab/streamlib/commit/777beac2b7882f247feaea851a4c973f0d48e1de))

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
