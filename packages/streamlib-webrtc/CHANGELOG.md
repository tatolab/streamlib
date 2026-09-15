# Changelog

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
