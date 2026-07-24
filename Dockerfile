# syntax=docker/dockerfile:1.7
# =============================================================================
# StreamLib — self-contained, headless, GPU-capable runtime image.
#
# Multi-stage:
#   * builder : full toolchain (GPU-free). Emits an image-local STATIC
#               package-source tree (`.slpkg` generic store + catalog + release
#               manifest) from THIS checkout, and builds the runtime binaries.
#               (docker/build-stage1.sh does all of it.)
#   * final   : nvidia/cuda runtime base + GPU/Vulkan/GLVND + V4L2 + userspace
#               audio + the build toolchain (build-capable). Carries the static
#               package-source tree, the app dir, and the cargo caches. Runs the
#               service via docker/entrypoint.sh — no .deb, no systemd.
#
# The image is build-capable on purpose: the image-local static package-source
# tree + toolchain let `runtime.add_module` resolve and build packages
# (docs/architecture/package-source.md). There is no daemon and no cargo
# registry — a package's engine / SDK crate deps resolve to the local checkout
# via `streamlib link` ([patch.crates-io] path overrides). The Deno SDK npm face
# resolves over a dumb `python3 -m http.server` mount the entrypoint serves (npm
# is HTTP-only by spec); pypi + `.slpkg` read the same tree straight off
# `file://`. The api-server core module builds from source on first boot (warm
# cargo cache -> tens of seconds).
#
# Host prerequisites (driver + nvidia-container-toolkit + virtual devices) are
# NOT bakeable into an image — see scripts/docker/host-prereqs.sh and docker/README.md.
# =============================================================================

ARG CUDA_BASE=nvidia/cuda:13.2.1-runtime-ubuntu24.04
ARG RUST_CHANNEL=stable
ARG JTD_CODEGEN_VERSION=0.4.1

# -----------------------------------------------------------------------------
FROM ubuntu:24.04 AS builder
ARG RUST_CHANNEL
ARG JTD_CODEGEN_VERSION
ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin

# Toolchain + dev libs (authoritative set mirrors .github/workflows/test.yml +
# check-pack-load.yml; glslc compiles the engine's shaders, jtd-codegen runs in
# build.rs, the av*/opus/asound/v4l -dev libs back the codec/audio/camera
# cdylibs). `python3 -m http.server` serves the daemon-free npm mount at runtime.
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential clang libclang-dev cmake pkg-config glslc protobuf-compiler \
        libssl-dev \
        libvulkan-dev libopus-dev libasound2-dev libv4l-dev \
        libavcodec-dev libavformat-dev libavutil-dev libswscale-dev \
        libswresample-dev libavfilter-dev libavdevice-dev \
        ca-certificates curl jq git rsync unzip xz-utils \
        python3 python3-pip nodejs npm \
    && rm -rf /var/lib/apt/lists/*

# Rust (pinned channel), uv, deno (>= 2.8 for `deno pack`), jtd-codegen v0.4.1.
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --default-toolchain "${RUST_CHANNEL}" --profile minimal \
 && curl -LsSf https://astral.sh/uv/install.sh | env UV_INSTALL_DIR=/usr/local/bin sh \
 && curl -fsSL https://deno.land/install.sh | env DENO_INSTALL=/usr/local sh \
 && curl -sSL "https://github.com/jsontypedef/json-typedef-codegen/releases/download/v${JTD_CODEGEN_VERSION}/x86_64-unknown-linux-gnu.zip" -o /tmp/jtd.zip \
 && unzip -q /tmp/jtd.zip -d /tmp/jtd && install -m0755 /tmp/jtd/jtd-codegen /usr/local/bin/jtd-codegen \
 && rm -rf /tmp/jtd.zip /tmp/jtd

WORKDIR /src
COPY . /src

# Emit the image-local static package-source tree (`.slpkg` store + catalog +
# release manifest) and build the binaries.
ENV SRC=/src APP_DIR=/opt/streamlib \
    PACKAGE_SOURCE_DIR=/opt/streamlib/package-source PACKAGE_SOURCE_PORT=8799
RUN chmod +x docker/build-stage1.sh \
 && docker/build-stage1.sh

# -----------------------------------------------------------------------------
FROM ${CUDA_BASE} AS final
ENV DEBIAN_FRONTEND=noninteractive

# Runtime + build-capable apt set:
#   * GPU/Vulkan headless: the loader + the GLVND/EGL dispatch layer that
#     libGLX_nvidia sits behind (the non-obvious requirement — see
#     docs/learnings/headless-nvidia-vulkan-container.md) + X11 client libs.
#   * V4L2 + codec/audio runtime libs arrive transitively via the -dev libs
#     (the image is build-capable, so it carries the build toolchain too).
#   * Userspace audio: PipeWire + WirePlumber + the pipewire-alsa bridge.
#   * Build toolchain (glslc, build-essential, dev libs) so runtime add_module
#     can rebuild modules in-container; Rust/uv/deno/jtd-codegen are COPY'd below.
RUN apt-get update && apt-get install -y --no-install-recommends \
        libvulkan1 libglvnd0 libgl1 libglx0 libegl1 libgles2 libx11-6 libxext6 \
        vulkan-tools \
        pipewire pipewire-bin pipewire-alsa wireplumber libspa-0.2-modules dbus \
        build-essential clang libclang-dev cmake pkg-config glslc libssl-dev \
        libvulkan-dev libopus-dev libasound2-dev libv4l-dev \
        libavcodec-dev libavformat-dev libavutil-dev libswscale-dev \
        libswresample-dev libavfilter-dev libavdevice-dev \
        ca-certificates curl jq git rsync unzip xz-utils python3 \
    && rm -rf /var/lib/apt/lists/*

# Language toolchains (Rust + cargo caches, uv, deno, jtd-codegen) and the
# runtime npm package-source config, lifted from the builder.
COPY --from=builder /usr/local/rustup /usr/local/rustup
COPY --from=builder /usr/local/cargo  /usr/local/cargo
COPY --from=builder /usr/local/bin/uv /usr/local/bin/uv
COPY --from=builder /usr/local/bin/deno /usr/local/bin/deno
COPY --from=builder /usr/local/bin/jtd-codegen /usr/local/bin/jtd-codegen
COPY --from=builder /root/.npmrc /root/.npmrc

# The app dir: binaries + package source + the image-local static package-source
# tree (under /opt/streamlib/package-source) the entrypoint serves / reads for add_module.
COPY --from=builder /opt/streamlib /opt/streamlib

COPY docker/entrypoint.sh /usr/local/bin/streamlib-entrypoint
COPY docker/pipewire/10-virtual.conf /etc/pipewire/pipewire.conf.d/10-virtual.conf
RUN chmod +x /usr/local/bin/streamlib-entrypoint

# Package-source resolution against the image-local static tree: pypi + `.slpkg`
# read `file://` with no server; the Deno SDK npm face resolves over the
# localhost static mount docker/entrypoint.sh serves on
# ${STREAMLIB_PACKAGE_SOURCE_HTTP_PORT} (npm is HTTP-only by spec). There is no
# cargo registry — a package's engine / SDK crate deps resolve to the local
# checkout via `streamlib link` ([patch.crates-io] path overrides).
ENV PATH=/opt/streamlib/bin:/usr/local/cargo/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    STREAMLIB_HOME=/opt/streamlib \
    STREAMLIB_PACKAGE_SOURCE_DIR=/opt/streamlib/package-source \
    STREAMLIB_PACKAGE_SOURCE_HTTP_PORT=8799 \
    STREAMLIB_PACKAGE_SOURCE=file:///opt/streamlib/package-source \
    UV_INDEX=file:///opt/streamlib/package-source/pypi/simple \
    XDG_RUNTIME_DIR=/run/user/0 \
    NVIDIA_DRIVER_CAPABILITIES=all \
    NVIDIA_VISIBLE_DEVICES=all

# API server control plane.
EXPOSE 9000

ENTRYPOINT ["/usr/local/bin/streamlib-entrypoint"]
CMD ["streamlib-runtime", "--host", "0.0.0.0", "--port", "9000"]
