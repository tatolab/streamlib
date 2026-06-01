# syntax=docker/dockerfile:1.7
# =============================================================================
# StreamLib — self-contained, headless, GPU-capable runtime image.
#
# Multi-stage:
#   * gitea-src : pull the static Gitea binary out of the official image.
#   * builder   : full toolchain (GPU-free). Stands up an ephemeral Gitea,
#                 publishes the whole closure from THIS checkout, builds the
#                 runtime, and pre-materializes the api-server core module.
#                 (docker/build-stage1.sh does all of it.)
#   * final     : nvidia/cuda runtime base + GPU/Vulkan/GLVND + V4L2 + userspace
#                 audio + the build toolchain (build-capable). Carries the
#                 filled Gitea, the app dir, and the cargo caches. Runs the
#                 service via docker/entrypoint.sh — no .deb, no systemd.
#
# The image is build-capable on purpose: the in-container Gitea + toolchain let
# `runtime.add_module` resolve and build *new* packages against the same
# registry-by-version model used locally (docs/architecture/gitea-registry-distribution.md).
# Core boot needs neither (api-server is pre-materialized).
#
# Host prerequisites (driver + nvidia-container-toolkit + virtual devices) are
# NOT bakeable into an image — see scripts/docker/host-prereqs.sh and docker/README.md.
# =============================================================================

ARG CUDA_BASE=nvidia/cuda:13.2.1-runtime-ubuntu24.04
# Upstream Gitea linux-amd64 is a glibc static binary (the gitea/gitea Docker
# image ships a musl build that won't run on the Ubuntu/CUDA glibc base).
ARG GITEA_VERSION=1.22.6
ARG RUST_CHANNEL=stable
ARG JTD_CODEGEN_VERSION=0.4.1

# -----------------------------------------------------------------------------
FROM ubuntu:24.04 AS builder
ARG GITEA_VERSION
ARG RUST_CHANNEL
ARG JTD_CODEGEN_VERSION
ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin

# Toolchain + dev libs (authoritative set mirrors .github/workflows/test.yml +
# check-pack-load.yml; glslc compiles the engine's shaders, jtd-codegen runs in
# build.rs, the av*/opus/asound/v4l -dev libs back the codec/audio/camera cdylibs).
# npm publishes the packed Deno SDK; util-linux provides setpriv (Gitea-as-git).
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential clang libclang-dev cmake pkg-config glslc protobuf-compiler \
        libssl-dev \
        libvulkan-dev libopus-dev libasound2-dev libv4l-dev \
        libavcodec-dev libavformat-dev libavutil-dev libswscale-dev \
        libswresample-dev libavfilter-dev libavdevice-dev \
        ca-certificates curl jq git rsync unzip xz-utils \
        python3 python3-pip nodejs npm util-linux \
    && rm -rf /var/lib/apt/lists/* \
    && pip3 install --no-cache-dir --break-system-packages tomlkit

# Rust (pinned channel), uv, deno (>= 2.8 for `deno pack`), jtd-codegen v0.4.1.
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --default-toolchain "${RUST_CHANNEL}" --profile minimal \
 && curl -LsSf https://astral.sh/uv/install.sh | env UV_INSTALL_DIR=/usr/local/bin sh \
 && curl -fsSL https://deno.land/install.sh | env DENO_INSTALL=/usr/local sh \
 && curl -sSL "https://github.com/jsontypedef/json-typedef-codegen/releases/download/v${JTD_CODEGEN_VERSION}/x86_64-unknown-linux-gnu.zip" -o /tmp/jtd.zip \
 && unzip -q /tmp/jtd.zip -d /tmp/jtd && install -m0755 /tmp/jtd/jtd-codegen /usr/local/bin/jtd-codegen \
 && rm -rf /tmp/jtd.zip /tmp/jtd

# Gitea must run as a non-root user (it refuses root). uid 1000 collides with
# Ubuntu 24.04's default `ubuntu` user — reclaim it for `git`.
RUN userdel -r ubuntu 2>/dev/null || true \
 && groupadd -g 1000 git && useradd -u 1000 -g 1000 -m -s /bin/bash git

RUN curl -fsSL "https://dl.gitea.com/gitea/${GITEA_VERSION}/gitea-${GITEA_VERSION}-linux-amd64" \
      -o /usr/local/bin/gitea \
 && chmod 0755 /usr/local/bin/gitea \
 && /usr/local/bin/gitea --version

WORKDIR /src
COPY . /src

# Fill the registry, build the binaries, pre-materialize api-server.
ARG SKIP_PYTHON_SDK=0
ARG SKIP_DENO_SDK=0
ARG SKIP_PACKAGES=0
ARG PREBUILD_API_SERVER=1
ENV SRC=/src APP_DIR=/opt/streamlib GITEA_WORK_DIR=/var/lib/gitea \
    GITEA_URL=http://localhost:3300 GITEA_ORG=tatolab GITEA_ADMIN_USER=tatolab-admin
RUN chmod +x docker/build-stage1.sh \
 && SKIP_PYTHON_SDK=${SKIP_PYTHON_SDK} SKIP_DENO_SDK=${SKIP_DENO_SDK} \
    SKIP_PACKAGES=${SKIP_PACKAGES} PREBUILD_API_SERVER=${PREBUILD_API_SERVER} \
    docker/build-stage1.sh

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
        ca-certificates curl jq git rsync unzip xz-utils python3 util-linux \
    && rm -rf /var/lib/apt/lists/*

RUN userdel -r ubuntu 2>/dev/null || true \
 && groupadd -g 1000 git && useradd -u 1000 -g 1000 -m -s /bin/bash git

# Language toolchains (Rust + cargo caches, uv, deno, jtd-codegen) and the
# Gitea binary, lifted from the builder.
COPY --from=builder /usr/local/rustup /usr/local/rustup
COPY --from=builder /usr/local/cargo  /usr/local/cargo
COPY --from=builder /usr/local/bin/uv /usr/local/bin/uv
COPY --from=builder /usr/local/bin/deno /usr/local/bin/deno
COPY --from=builder /usr/local/bin/jtd-codegen /usr/local/bin/jtd-codegen
COPY --from=builder /usr/local/bin/gitea /usr/local/bin/gitea
COPY --from=builder /root/.npmrc /root/.npmrc

# The app dir (binaries + package source + pre-materialized cache) and the
# filled Gitea data (owned by the git user that serves it).
COPY --from=builder /opt/streamlib /opt/streamlib
COPY --from=builder --chown=git:git /var/lib/gitea /var/lib/gitea

COPY docker/entrypoint.sh /usr/local/bin/streamlib-entrypoint
COPY docker/pipewire/10-virtual.conf /etc/pipewire/pipewire.conf.d/10-virtual.conf
RUN chmod +x /usr/local/bin/streamlib-entrypoint

ENV PATH=/opt/streamlib/bin:/usr/local/cargo/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    STREAMLIB_HOME=/opt/streamlib \
    GITEA_WORK_DIR=/var/lib/gitea \
    GITEA_URL=http://localhost:3300 \
    STREAMLIB_REGISTRY_URL=http://localhost:3300 \
    UV_INDEX=http://localhost:3300/api/packages/tatolab/pypi/simple \
    CARGO_REGISTRIES_GITEA_INDEX=sparse+http://localhost:3300/api/packages/tatolab/cargo/ \
    XDG_RUNTIME_DIR=/run/user/0 \
    NVIDIA_DRIVER_CAPABILITIES=all \
    NVIDIA_VISIBLE_DEVICES=all

# API server (9000), in-container Gitea registry (3300).
EXPOSE 9000 3300

ENTRYPOINT ["/usr/local/bin/streamlib-entrypoint"]
CMD ["streamlib-runtime", "--host", "0.0.0.0", "--port", "9000"]
