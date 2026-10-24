-- The composed stream session the relay page's door checks (C15, page=real), in the standalone
-- app's own database: a stream target (a placeholder destination; nothing here ever streams to it)
-- and a `fixture_stream_sessions` row with the run's SESSION_ID, mode composed, state live, on the
-- seeded fixture. The door (web apps/web/src/server/overlay/relay-access.ts) needs exactly this:
-- the row with id = sid, its fixture, mode composed, a state that is not completed or failed.
--
--   psql -v ON_ERROR_STOP=1 -v sid=<SESSION_ID> -v fixture=<id> -v sport=cricket -v delay=3100 \
--     -v email=<the seed account> -f session.sql
\set QUIET on
set search_path = seazn_club;
with f as (
  select fx.id as fixture_id, d.id as division_id, d.competition_id, c.org_id
  from fixtures fx
  join divisions d on d.id = fx.division_id
  join competitions c on c.id = d.competition_id
  where fx.id = :'fixture'
), t as (
  insert into org_stream_targets (org_id, kind, label, rtmp_enc)
  select org_id, 'custom_rtmp', 'soak (never used)', '\x00'::bytea from f
  returning id, org_id
)
insert into fixture_stream_sessions (id, fixture_id, org_id, mode, state, target_id, sport_key, competition_id,
  division_id, entitlement_via_override, created_by, overlay_delay_ms)
select :'sid', f.fixture_id, f.org_id, 'composed', 'live', t.id, :'sport', f.competition_id, f.division_id, false,
  (select id from users where email = :'email'), :'delay'::int
from f join t on t.org_id = f.org_id
returning id, fixture_id, mode, state;
