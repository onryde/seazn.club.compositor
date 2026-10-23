# syntax=docker/dockerfile:1.7
# The relay image (C12): GStreamer, gstcefsrc with CEF (F-001's pins), Xvfb, tini and the compositor.
# Plain OCI, nothing host-specific inside (ADR 0007). Build context = the repo root:
#   docker build --build-arg GSTCEF_COMMIT="$(cat docker/GSTCEF_COMMIT)" -f docker/Dockerfile .
# Stages: gst-base (the runtime libraries), cef-build (gstcefsrc + CEF), rust-build (the binary),
# cef-test (rust-build plus the plugin: image.yml runs C11's graphics tests there), harness-build
# and harness (C13's fakes and checker, `--target harness`; test tooling the runtime never
# carries), and the runtime, last and unnamed, so a build with no target is the relay image.
#
# Never set GST_CEF_LOG_SEVERITY or GST_DEBUG here (Owed 52, F-013): the first overrides the
# element's log-severity=disable and CEF then writes every console line, the job token included, to
# chrome_debug.log; the second prints the page URL with its token. image.yml refuses either, in the
# image's environment and in the running compositor's (scripts/image_probe.py).

FROM ubuntu:24.04 AS gst-base
ENV DEBIAN_FRONTEND=noninteractive
# The runtime list is F-001's (the C00 spike's runtime stage): the brief's was short. libcups2t64,
# libpango-1.0-0 and libcairo2 are libcef.so's (F-001); glib-networking is GStreamer's TLS (rtmps).
RUN apt-get update && apt-get install -y --no-install-recommends \
      gstreamer1.0-tools gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
      gstreamer1.0-plugins-ugly gstreamer1.0-libav glib-networking xvfb xauth tini ca-certificates \
      fonts-dejavu-core libnss3 libatk-bridge2.0-0 libatk1.0-0 libcups2t64 libgbm1 libasound2t64 \
      libxkbcommon0 libdrm2 libxcomposite1 libxdamage1 libxrandr2 libxfixes3 libpango-1.0-0 libcairo2 \
      libxshmfence1 libdbus-1-3 \
    && rm -rf /var/lib/apt/lists/*

FROM gst-base AS cef-build
# F-001: linking gstcefsubprocess against libcef.so needs libcups, libpango and libcairo present at
# link time (gst-base has them).
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential cmake git curl pkg-config libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
      libx11-dev libxcomposite-dev libxdamage-dev libxrandr-dev \
    && rm -rf /var/lib/apt/lists/*
# F-001's pin, read from docker/GSTCEF_COMMIT by image.yml. No default: the build fails without it.
ARG GSTCEF_COMMIT
WORKDIR /src
RUN test -n "$GSTCEF_COMMIT" \
 && git clone https://github.com/centricular/gstcefsrc && cd gstcefsrc && git checkout "$GSTCEF_COMMIT" \
 && test "$(git rev-parse HEAD)" = "$GSTCEF_COMMIT" \
 && mkdir build && cd build && cmake -G "Unix Makefiles" -DCMAKE_BUILD_TYPE=Release .. && make -j"$(nproc)" \
 && strip --strip-unneeded Release/libcef.so \
 && { echo "gstcefsrc_commit=$(git -C /src/gstcefsrc rev-parse HEAD)"; \
      echo "cef_dir=$(find /src/gstcefsrc/third_party/cef -mindepth 1 -maxdepth 1 -type d -name 'cef_binary*' -printf '%f\n' | head -n 1)"; } > Release/BUILDINFO \
 && cat Release/BUILDINFO && du -sh Release

FROM gst-base AS rust-build
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential curl pkg-config libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
    && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
RUN curl -sSf https://sh.rustup.rs -o /tmp/rustup-init.sh \
 && sh /tmp/rustup-init.sh -y --no-modify-path --profile minimal --default-toolchain 1.91.0 \
 && rm /tmp/rustup-init.sh && rustc --version
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p compositor \
 && install -m 0755 target/release/compositor /usr/local/bin/compositor \
 && strip /usr/local/bin/compositor

# C11's graphics tests beside a real cefsrc, as a non-root user (image.yml runs them).
FROM rust-build AS cef-test
COPY --from=cef-build /src/gstcefsrc/build/Release /opt/gstcef
ENV GST_PLUGIN_PATH=/opt/gstcef DISPLAY=:99
RUN useradd --uid 10001 --create-home tester && chown -R 10001:10001 /src /opt/cargo
USER tester

# The harness (C13, harness/compose.yml): fake-camera, fake-control and the checker on the same
# GStreamer as the runtime, plus tc (netem.sh's sidecars) and curl (the cases' controls). No CEF:
# the harness only feeds and reads the relay image.
FROM rust-build AS harness-build
RUN cargo build --release --locked -p harness --bins \
 && install -m 0755 target/release/fake-camera target/release/fake-control target/release/checker /usr/local/bin/

FROM gst-base AS harness
RUN apt-get update && apt-get install -y --no-install-recommends iproute2 curl \
    && rm -rf /var/lib/apt/lists/*
COPY --from=harness-build /usr/local/bin/fake-camera /usr/local/bin/fake-control /usr/local/bin/checker /usr/local/bin/
RUN for e in appsrc x264enc h264parse mpegtsmux srtsink audioconvert voaacenc aacparse queue filesrc \
      decodebin qtdemux avdec_h264 avdec_aac videoconvert audioresample appsink fakesink; do \
      gst-inspect-1.0 "$e" >/dev/null 2>&1 || { echo "missing element $e"; exit 1; }; done

FROM gst-base
COPY --from=cef-build /src/gstcefsrc/build/Release /opt/gstcef
COPY --from=rust-build /usr/local/bin/compositor /usr/local/bin/compositor
COPY docker/entrypoint.sh /entrypoint.sh
COPY docker/supervise.sh /usr/local/lib/seazn/supervise.sh
ENV GST_PLUGIN_PATH=/opt/gstcef COMPOSITOR_DRIVERS=live DISPLAY=:99
# Owed 54: the CEF user's HOME exists and is writable (GStreamer's registry, NSS's database), and
# only this one process's CEF uses it.
RUN chmod 0755 /entrypoint.sh && chmod 0644 /usr/local/lib/seazn/supervise.sh \
 && command -v prlimit >/dev/null \
 && useradd --system --uid 10001 --home /home/relay --create-home --shell /usr/sbin/nologin relay \
 && missing=$(ldd /opt/gstcef/libcef.so /opt/gstcef/libgstcef.so /opt/gstcef/gstcefsubprocess /usr/local/bin/compositor \
      | grep "not found" || true) \
 && if [ -n "$missing" ]; then echo "$missing"; exit 1; fi
USER relay
WORKDIR /home/relay
# Fail the build on a missing element (every factory the adapters make), and leave the relay
# user's plugin registry built, so the first start does not scan.
RUN for e in cefsrc srtsrc tsdemux queue h264parse avdec_h264 videoconvertscale capsfilter appsink \
      aacparse avdec_aac audioconvert audioresample appsrc compositor videoconvert x264enc voaacenc \
      flvmux rtmp2sink filesink videotestsrc pngdec; do \
      gst-inspect-1.0 "$e" >/dev/null 2>&1 || { echo "missing element $e"; exit 1; }; done
# `-s`: tini is PID 1 under Docker but not on every host; as a subreaper it reaps the orphans CEF
# leaves (a GPU process whose browser is gone) and prints no "not running as PID 1" warning, three
# unformatted lines on every boot (the PR #36 review's m5). No `-g`: tini signals the entrypoint
# alone, which forwards the stops (F-016).
ENTRYPOINT ["/usr/bin/tini", "-s", "--", "/entrypoint.sh"]
