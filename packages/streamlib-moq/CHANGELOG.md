# Changelog

## [0.3.2](https://github.com/tatolab/streamlib/compare/streamlib-moq-v0.3.1...streamlib-moq-v0.3.2) (2026-09-20)


### Features

* **engine:** show each remote link's stamp clock, and stop an Mp4Sink track from another clock ([#2382](https://github.com/tatolab/streamlib/issues/2382)) ([872d8e4](https://github.com/tatolab/streamlib/commit/872d8e49f8996fc8af21f7450cad88630b48fea2))

## [0.3.1](https://github.com/tatolab/streamlib/compare/streamlib-moq-v0.3.0...streamlib-moq-v0.3.1) (2026-09-19)


### Features

* **wheel:** wire a remote output from Python and MCP, into helper-placed processors too ([#2342](https://github.com/tatolab/streamlib/issues/2342)) ([a8cf4ad](https://github.com/tatolab/streamlib/commit/a8cf4adb6877f513bffaef2012774e03bc7e8e45))

## [0.3.0](https://github.com/tatolab/streamlib/compare/streamlib-moq-v0.2.2...streamlib-moq-v0.3.0) (2026-09-18)


### ⚠ BREAKING CHANGES

* **engine:** every runtime carries a stable name it owns — hashed default, set by constructor, environment or CLI ([#2329](https://github.com/tatolab/streamlib/issues/2329))

### Features

* **engine:** every runtime carries a stable name it owns — hashed default, set by constructor, environment or CLI ([#2329](https://github.com/tatolab/streamlib/issues/2329)) ([d8bd91e](https://github.com/tatolab/streamlib/commit/d8bd91e7414e5f1abc855827d059151693077091))

## [0.2.2](https://github.com/tatolab/streamlib/compare/streamlib-moq-v0.2.1...streamlib-moq-v0.2.2) (2026-09-15)


### Features

* **engine:** size every channel for a late consumer of any delivery profile and raise the link caps to 32 + tap and 256 ([#2301](https://github.com/tatolab/streamlib/issues/2301)) ([bf2b1e0](https://github.com/tatolab/streamlib/commit/bf2b1e018412eccdfea312c5be2be10645b4eb94))

## [0.2.1](https://github.com/tatolab/streamlib/compare/streamlib-moq-v0.2.0...streamlib-moq-v0.2.1) (2026-09-15)


### Features

* **engine:** resolve one runtime directory and run iceoryx2 in an engine-owned domain per OS user ([#2294](https://github.com/tatolab/streamlib/issues/2294)) ([bc87317](https://github.com/tatolab/streamlib/commit/bc87317b4c7c79a495c4b6c598d9a04795c95785))

## [0.2.0](https://github.com/tatolab/streamlib/compare/streamlib-moq-v0.1.6...streamlib-moq-v0.2.0) (2026-09-11)


### ⚠ BREAKING CHANGES

* **wheel:** keyword-argument configuration is deleted. A processor's `__init__` takes one `config` parameter annotated with its config class, or nothing beyond `self`; any other signature is refused at decoration.

### Features

* **wheel:** a Python processor's config is one class named by its __init__ annotation ([#2226](https://github.com/tatolab/streamlib/issues/2226)) ([9033ca9](https://github.com/tatolab/streamlib/commit/9033ca9b06fb292ae63f8a7d8d4a26a57d04e351))

## [0.1.6](https://github.com/tatolab/streamlib/compare/streamlib-moq-v0.1.5...streamlib-moq-v0.1.6) (2026-09-06)


### Features

* **extension:** the MoQ publisher sheds an uplink backlog — vendored moq-transport, abandon with a draft-16 reset, the unforwarded count and the QUIC path ([#2191](https://github.com/tatolab/streamlib/issues/2191)) ([facbf7b](https://github.com/tatolab/streamlib/commit/facbf7bdc48777e0941c1675b11fca317d9773ec))

## [0.1.5](https://github.com/tatolab/streamlib/compare/streamlib-moq-v0.1.4...streamlib-moq-v0.1.5) (2026-09-06)


### Features

* **extension:** streamlib-moq subscribes to a data track — data_track config, the data_bags output, envelope decode and the gap count ([#2185](https://github.com/tatolab/streamlib/issues/2185)) ([9fc66f6](https://github.com/tatolab/streamlib/commit/9fc66f6be946ba0d6d88a4aa2454acf012bed038))

## [0.1.4](https://github.com/tatolab/streamlib/compare/streamlib-moq-v0.1.3...streamlib-moq-v0.1.4) (2026-09-06)


### Features

* **extension:** streamlib-moq publishes data tracks — classification by bitstream, the envelope, track_names, the cmaf refusal and the time backstop ([#2183](https://github.com/tatolab/streamlib/issues/2183)) ([2d80f11](https://github.com/tatolab/streamlib/commit/2d80f115eb1b3de165096f1e4f1f81ed29dd5fd6))

## [0.1.3](https://github.com/tatolab/streamlib/compare/streamlib-moq-v0.1.2...streamlib-moq-v0.1.3) (2026-09-05)


### Features

* **extension:** the MoQ drop policy — a delivery deadline that sheds a stale group, audio ahead of video on the priority ladder ([#2181](https://github.com/tatolab/streamlib/issues/2181)) ([8f306e9](https://github.com/tatolab/streamlib/commit/8f306e9aa80794b1037f274fbbc479012fd15ee7))

## [0.1.2](https://github.com/tatolab/streamlib/compare/streamlib-moq-v0.1.1...streamlib-moq-v0.1.2) (2026-09-05)


### Bug Fixes

* **media:** the ISOBMFF conformance sweep — fabricated durations, an elided capture gap, and four places the two writers disagree ([#2176](https://github.com/tatolab/streamlib/issues/2176)) ([55263a2](https://github.com/tatolab/streamlib/commit/55263a2d783aa4794cf54cae2d261ce86e7ca8dd))

## [0.1.1](https://github.com/tatolab/streamlib/compare/streamlib-moq-v0.1.0...streamlib-moq-v0.1.1) (2026-09-05)


### Features

* **extension:** streamlib-moq — a standalone maturin wheel publishing and subscribing MoQ over draft-16, in CMAF and the bag envelope ([#2158](https://github.com/tatolab/streamlib/issues/2158)) ([00a5e9e](https://github.com/tatolab/streamlib/commit/00a5e9e158b4b2ae343bb4384ef3f8583e206f0a))
