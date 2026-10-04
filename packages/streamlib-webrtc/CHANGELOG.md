# Changelog

## [0.5.0](https://github.com/tatolab/streamlib/compare/streamlib-webrtc-v0.4.0...streamlib-webrtc-v0.5.0) (2026-10-04)


### ⚠ BREAKING CHANGES

* **engine:** the graph is one shape — the snapshot a runtime loads is what graph renders ([#2614](https://github.com/tatolab/streamlib/issues/2614))

### Features

* **engine:** the graph is one shape — the snapshot a runtime loads is what graph renders ([#2614](https://github.com/tatolab/streamlib/issues/2614)) ([66a0675](https://github.com/tatolab/streamlib/commit/66a0675e34376c23afe1322a4f61bc22256ef553))

## [0.4.0](https://github.com/tatolab/streamlib/compare/streamlib-webrtc-v0.3.3...streamlib-webrtc-v0.4.0) (2026-10-03)


### ⚠ BREAKING CHANGES

* **sdk:** `streamlib.processor` is gone; decorate node classes with `streamlib.node`. A scaffolded app's node modules live under `nodes/`.

### Features

* **sdk:** `[@node](https://github.com/node)` declares a node where `[@processor](https://github.com/processor)` did ([#2612](https://github.com/tatolab/streamlib/issues/2612)) ([8f411f2](https://github.com/tatolab/streamlib/commit/8f411f2f3705f1678c01a8facd7b6ce298d1749d))

## [0.3.3](https://github.com/tatolab/streamlib/compare/streamlib-webrtc-v0.3.2...streamlib-webrtc-v0.3.3) (2026-10-01)


### Features

* **packages:** the MoQ and WebRTC extension wheels build, test and release on macOS, on the engine's clock ([#2555](https://github.com/tatolab/streamlib/issues/2555)) ([417d368](https://github.com/tatolab/streamlib/commit/417d368b54568aca782a8995aa6700664babb0d1))

## [0.3.2](https://github.com/tatolab/streamlib/compare/streamlib-webrtc-v0.3.1...streamlib-webrtc-v0.3.2) (2026-09-20)


### Features

* **engine:** show each remote link's stamp clock, and stop an Mp4Sink track from another clock ([#2382](https://github.com/tatolab/streamlib/issues/2382)) ([872d8e4](https://github.com/tatolab/streamlib/commit/872d8e49f8996fc8af21f7450cad88630b48fea2))

## [0.3.1](https://github.com/tatolab/streamlib/compare/streamlib-webrtc-v0.3.0...streamlib-webrtc-v0.3.1) (2026-09-19)


### Features

* **wheel:** wire a remote output from Python and MCP, into helper-placed processors too ([#2342](https://github.com/tatolab/streamlib/issues/2342)) ([a8cf4ad](https://github.com/tatolab/streamlib/commit/a8cf4adb6877f513bffaef2012774e03bc7e8e45))

## [0.3.0](https://github.com/tatolab/streamlib/compare/streamlib-webrtc-v0.2.2...streamlib-webrtc-v0.3.0) (2026-09-18)


### ⚠ BREAKING CHANGES

* **engine:** every runtime carries a stable name it owns — hashed default, set by constructor, environment or CLI ([#2329](https://github.com/tatolab/streamlib/issues/2329))

### Features

* **engine:** every runtime carries a stable name it owns — hashed default, set by constructor, environment or CLI ([#2329](https://github.com/tatolab/streamlib/issues/2329)) ([d8bd91e](https://github.com/tatolab/streamlib/commit/d8bd91e7414e5f1abc855827d059151693077091))

## [0.2.2](https://github.com/tatolab/streamlib/compare/streamlib-webrtc-v0.2.1...streamlib-webrtc-v0.2.2) (2026-09-15)


### Features

* **engine:** size every channel for a late consumer of any delivery profile and raise the link caps to 32 + tap and 256 ([#2301](https://github.com/tatolab/streamlib/issues/2301)) ([bf2b1e0](https://github.com/tatolab/streamlib/commit/bf2b1e018412eccdfea312c5be2be10645b4eb94))

## [0.2.1](https://github.com/tatolab/streamlib/compare/streamlib-webrtc-v0.2.0...streamlib-webrtc-v0.2.1) (2026-09-15)


### Features

* **engine:** resolve one runtime directory and run iceoryx2 in an engine-owned domain per OS user ([#2294](https://github.com/tatolab/streamlib/issues/2294)) ([bc87317](https://github.com/tatolab/streamlib/commit/bc87317b4c7c79a495c4b6c598d9a04795c95785))

## [0.2.0](https://github.com/tatolab/streamlib/compare/streamlib-webrtc-v0.1.1...streamlib-webrtc-v0.2.0) (2026-09-11)


### ⚠ BREAKING CHANGES

* **wheel:** keyword-argument configuration is deleted. A processor's `__init__` takes one `config` parameter annotated with its config class, or nothing beyond `self`; any other signature is refused at decoration.

### Features

* **wheel:** a Python processor's config is one class named by its __init__ annotation ([#2226](https://github.com/tatolab/streamlib/issues/2226)) ([9033ca9](https://github.com/tatolab/streamlib/commit/9033ca9b06fb292ae63f8a7d8d4a26a57d04e351))

## [0.1.1](https://github.com/tatolab/streamlib/compare/streamlib-webrtc-v0.1.0...streamlib-webrtc-v0.1.1) (2026-09-05)


### Features

* **extension:** streamlib-webrtc — a standalone maturin wheel with WhipPublisher and WhepPlayer on the mined clients and h264_rtp.rs ([#2156](https://github.com/tatolab/streamlib/issues/2156)) ([80e652d](https://github.com/tatolab/streamlib/commit/80e652da9f1dc48fe50f79709740c7fe2a734340))
