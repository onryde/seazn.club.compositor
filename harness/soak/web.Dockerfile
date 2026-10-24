# The soak's harness Machine for page=real (C15): the soak harness image (harness/soak/Dockerfile)
# plus the page source, a standalone seazn.club web build and its own Postgres. soak.yml's web job
# builds the web app at a pinned web commit, runs db:apply and sync:sports against a scratch
# Postgres, dumps it, and proves the build carries no PostHog key, Sentry DSN or Stripe key
# (web/bundle-check.sh). Its artifact lands in harness/soak/web/build/:
#
#   app.tgz      the standalone tree (.next/standalone, with .next/static and public staged)
#   node         the runner's node binary (the web build's own; this image is the same Ubuntu)
#   db/roles.sql db/seazn.sql     the roles and the migrated, sport-synced database (plain SQL)
#   web.json     the web commit, the node version, the bundle check's file count
#
#   docker buildx build --builder default --load -f harness/soak/web.Dockerfile \
#     --build-arg SOAK_IMAGE=<the soak harness image> -t <tag> .
#
# The database is restored here, at build time, so a dump that does not restore fails the build,
# not the Machine. Nothing here is a secret: the signing key is made per run on the Machine.
ARG SOAK_IMAGE
FROM ${SOAK_IMAGE}
USER root
# Ubuntu 24.04's Postgres 16, the version the web repo's CI migrates on. No default cluster: the
# soak's own is made below. libatomic1: the runner's node binary links it (rehearsal 40130557751).
RUN mkdir -p /etc/postgresql-common/createcluster.d \
    && echo 'create_main_cluster = false' >/etc/postgresql-common/createcluster.d/soak.conf \
    && apt-get update && apt-get install -y --no-install-recommends postgresql-16 postgresql-client-16 libatomic1 \
    && rm -rf /var/lib/apt/lists/*
COPY harness/soak/web/build/node /usr/local/bin/node
ADD harness/soak/web/build/app.tgz /web/app/
COPY harness/soak/web/build/db /web/db
COPY harness/soak/web/build/web.json /web/web.json
COPY harness/soak/web/lib.mjs harness/soak/web/seed.mjs harness/soak/web/score.mjs harness/soak/web/overlay-url.mjs \
     harness/soak/web/serve.sh harness/soak/web/up.sh harness/soak/web/smoke.sh harness/soak/web/session.sql /soak/web/
# Every shared library the copied node links must resolve here: a missing one fails the build by
# name, not the Machine with a crash.
RUN chmod 0755 /usr/local/bin/node /soak/web/serve.sh /soak/web/up.sh /soak/web/smoke.sh \
    && ldd /usr/local/bin/node \
    && if ldd /usr/local/bin/node | grep 'not found'; then echo "node: a shared library is missing (above)"; exit 1; fi \
    && node --version && test -f /web/app/apps/web/server.js \
    && install -d -o postgres -g postgres -m 700 /var/lib/soak-pg \
    && runuser -u postgres -- /usr/lib/postgresql/16/bin/initdb -D /var/lib/soak-pg -U postgres --auth=trust \
         --no-locale -E UTF8 >/dev/null \
    && runuser -u postgres -- /usr/lib/postgresql/16/bin/pg_ctl -D /var/lib/soak-pg -w \
         -o "-c listen_addresses='' -k /tmp" start >/dev/null \
    && psql -h /tmp -U postgres -X -q -v ON_ERROR_STOP=1 -f /web/db/roles.sql \
    && createdb -h /tmp -U postgres seazn \
    && psql -h /tmp -U postgres -d seazn -X -q -v ON_ERROR_STOP=1 -f /web/db/seazn.sql >/dev/null \
    && test "$(psql -h /tmp -U postgres -d seazn -XAtc 'select count(*) from seazn_club.sports')" -gt 0 \
    && runuser -u postgres -- /usr/lib/postgresql/16/bin/pg_ctl -D /var/lib/soak-pg -m fast -w stop >/dev/null
