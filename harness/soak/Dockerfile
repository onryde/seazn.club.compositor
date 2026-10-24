# The soak's harness Machine (C15): the C13 harness image (docker/Dockerfile's `harness` target, at
# the sha under test: fake-camera, fake-control, checker) plus MediaMTX, iptables and the driver, as
# plain processes under tini. Built by soak.yml with the docker driver, on top of the harness image
# it built and loaded a step before:
#
#   docker buildx build --builder default --load -f harness/soak/Dockerfile \
#     --build-arg HARNESS_IMAGE=seazn-harness:<sha> -t <tag> .
#
# It runs as root only because the dest disconnect uses iptables and ip6tables (the brief). It holds
# no secret: the job token arrives as the Machine's environment, the SRT passphrase is made here.
ARG HARNESS_IMAGE
# compose.yml's MediaMTX, by the same digest (F-021): one binary in a scratch image.
FROM bluenviron/mediamtx:1.21.2@sha256:c7540465f868a02652118bea5a0daab12a29ac0f6cb8485abd535939ecddf886 AS mediamtx

FROM ${HARNESS_IMAGE}
USER root
RUN apt-get update && apt-get install -y --no-install-recommends iptables jq procps \
    && rm -rf /var/lib/apt/lists/* \
    && command -v iptables-legacy >/dev/null && command -v ip6tables-legacy >/dev/null \
    && command -v iptables-nft >/dev/null && command -v ip6tables-nft >/dev/null
COPY --from=mediamtx /mediamtx /usr/local/bin/mediamtx
# What fake-control serves and rewrites: the C14 test page and its score timeline (the plumbing
# page; the production page is an overlayUrl, driver.sh), and the vendored composed session.
COPY harness/pages /soak/pages
COPY contracts/fixtures/relay-job.v1/session-composed-720p.json /soak/session-composed-720p.json
COPY harness/thresholds.json /soak/thresholds.json
COPY harness/soak/driver.sh harness/soak/schedule.jq harness/soak/soak.json harness/soak/mediamtx.yml /soak/
RUN chmod 0755 /soak/driver.sh \
    && for b in mediamtx fake-camera fake-control checker jq curl iptables ip6tables tini; do \
         command -v "$b" >/dev/null || { echo "missing $b"; exit 1; }; done
WORKDIR /out
# `-s`: tini is not PID 1 on a Machine (the platform's init is), so it reaps as a subreaper, as the
# relay image's does.
ENTRYPOINT ["/usr/bin/tini", "-s", "--", "/soak/driver.sh"]
