# C00 spike image (THROWAWAY, branch spike/t00, never merged). Plain docker: nothing Fly-specific inside.
# Build context = repo root:  docker build -f spike/Dockerfile .
FROM ubuntu:24.04 AS build
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential cmake git ca-certificates curl pkg-config \
      libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
      libx11-dev libxcomposite-dev libxdamage-dev libxrandr-dev libnss3 libatk-bridge2.0-0 \
      libgbm1 libasound2t64 libxkbcommon0 libdrm2 \
      libcups2t64 libpango-1.0-0 libcairo2 libatk1.0-0 libatspi2.0-0 libxfixes3 libxext6 libxshmfence1 libdbus-1-3 libexpat1 \
    && rm -rf /var/lib/apt/lists/*
# F-001: the brief's list was short. Linking gstcefsubprocess against libcef.so (CEF 139) needs
# libcups, libpango and libcairo present at link time (undefined ipp*/cups*/pango*/cairo* refs).
WORKDIR /src
# F-001 pin: gstcefsrc HEAD of main on 2026-10-15 ("cefsrc: discard data in the PAUSED state", 2026-08-24).
ARG GSTCEF_COMMIT=204b01b02df23d143316b11e51516171ab7fae0a
RUN git clone https://github.com/centricular/gstcefsrc && cd gstcefsrc && git checkout "${GSTCEF_COMMIT}" \
 && mkdir build && cd build && cmake -G "Unix Makefiles" -DCMAKE_BUILD_TYPE=Release .. && make -j"$(nproc)" \
 && { echo "gstcefsrc_commit=$(git -C /src/gstcefsrc rev-parse HEAD)"; \
      echo "cef_dir=$(ls /src/gstcefsrc/third_party/cef | grep -m1 cef_binary)"; \
      echo "cmake_cef_version=$(grep -m1 -o 'CEF_VERSION \"[^\"]*\"' /src/gstcefsrc/CMakeLists.txt)"; } > /src/gstcefsrc/build/Release/BUILDINFO \
 && cat /src/gstcefsrc/build/Release/BUILDINFO && ls /src/gstcefsrc/build/Release

FROM ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      gstreamer1.0-tools gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
      gstreamer1.0-plugins-ugly gstreamer1.0-libav glib-networking xvfb xauth tini ca-certificates \
      libnss3 libatk-bridge2.0-0 libatk1.0-0 libcups2t64 libgbm1 libasound2t64 libxkbcommon0 libdrm2 \
      libxcomposite1 libxdamage1 libxrandr2 libxfixes3 libpango-1.0-0 libcairo2 libxshmfence1 libdbus-1-3 \
      python3 python3-gi gir1.2-gstreamer-1.0 gir1.2-gst-plugins-base-1.0 \
      sysstat procps ffmpeg \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/gstcefsrc/build/Release /opt/gstcefsrc
ENV GST_PLUGIN_PATH=/opt/gstcefsrc
# Fail the build (not the spike) on a missing shared library or element.
RUN missing=$(ldd /opt/gstcefsrc/libcef.so /opt/gstcefsrc/libgstcef.so /opt/gstcefsrc/gstcefsubprocess | grep "not found" || true); \
    if [ -n "$missing" ]; then echo "$missing"; exit 1; fi; \
    for e in cefsrc srtsrc srtsink rtmp2sink flvmux tsdemux mpegtsmux x264enc avdec_h264 voaacenc compositor fpsdisplaysink rawvideoparse pngenc; do \
      gst-inspect-1.0 "$e" >/dev/null 2>&1 || { echo "missing element $e"; exit 1; }; done
RUN useradd -m -u 10001 spike
COPY spike/ /spike/
RUN chmod +x /spike/*.sh
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["sleep", "infinity"]
