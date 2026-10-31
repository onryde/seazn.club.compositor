//! Proof of play (R4 sponsors on stream, task SC1; spec 2026-10-27 §3.6.2): the overlay page's
//! sponsor lines, as the heartbeat forwards them, and the bounded buffer that holds them until web has
//! them. Plain data and one pure buffer: the adapters read a page line into an `AiringLine` and write
//! a beat's lines on the wire; the control worker snapshots the buffer for each beat and acknowledges
//! what a 2xx carried.
//!
//! - **An `AiringLine` is always one web accepts.** Its constructors refuse every value outside web's
//!   bounds (S8's `RelayAiring`: ids of at most 80 UTF-16 code units, a break kind of at most 40, an
//!   ISO 8601 UTC instant). Web's heartbeat is strict: one item it refuses is a 400 for the whole beat,
//!   and because an item stays until a 2xx, every later beat would be lost with it.
//! - **The buffer holds at most `AIRING_BUFFER_CAP` lines.** A push past it drops the oldest and counts
//!   it (`dropped`, a total for the process), which every beat that carries airings sends as
//!   `airingsDropped`.
//! - **A line leaves only on a 2xx.** `take_for_beat` is a snapshot: every line held, oldest first, and
//!   the newest one's sequence number. `ack_2xx` removes the lines that snapshot carried and none pushed
//!   since; a beat that failed removes nothing, so the next beat sends its lines again (web upserts a
//!   start on its moment id, so a line resent after a lost 2xx is no second airing).
//! - **The final beat flushes**: it is a beat like any other, so it carries every line still held, and
//!   none is left behind for a beat that will never come. Its 2xx clears them as any beat's does; a
//!   second final beat (the drain's, then a fatal error's) sends again what the first did not land.
use std::collections::VecDeque;

/// Spec 2026-10-27 §3.6.2, "buffers ≤ 64"; web's `airings` is `.max(64)` (S8's `RelayHeartbeat`), so a
/// beat never carries more than web takes.
pub const AIRING_BUFFER_CAP: usize = 64;

/// S8's `RelayAiring`: `momentId` and `cardId` are `z.string().max(80)`, which counts UTF-16 code
/// units (zod's `.max` is the JavaScript string's `length`).
pub const AIRING_ID_MAX_UNITS: usize = 80;

/// S8's `RelayAiring`: `breakKind` is `z.string().max(40).nullable()` (UTF-16 code units, as above).
pub const AIRING_BREAK_KIND_MAX_UNITS: usize = 40;

/// The card's look (S8: `z.enum(["lower_third", "banner"])`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiringStyle {
    LowerThird,
    Banner,
}

/// What put the card on air (S8: `z.enum(["rotation", "scene", "operator"])`). The page sends
/// `rotation` for a break card and `scene` for a carousel showing (web's ruling R5); web takes the
/// real source of a non-scene start from its own assignments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiringSource {
    Rotation,
    Scene,
    Operator,
}

/// Why a sponsor line was refused. The text it came in is never kept: a reader logs the line itself,
/// clipped and scrubbed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineRefusal {
    /// The body is longer than the reader's bound (`SPONSOR_LINE_MAX_BYTES`, spec §3.6.2).
    Oversize,
    /// The body is not JSON.
    NotJson,
    /// The body is JSON but not an object.
    NotAnObject,
    /// A key outside the spec's set, or one of its keys missing (spec §3.6.1, exact key sets).
    KeySet,
    /// This key's value has the wrong JSON type.
    WrongType(&'static str),
    /// This key's word is not one web declares (`style`, `source`).
    UnknownWord(&'static str),
    /// This key's text is longer than web's bound.
    TooLong(&'static str),
    /// This key's text is not the ISO 8601 UTC instant web takes (`at`).
    NotAnInstant(&'static str),
}

/// A `seazn:sponsor:start` line: the card a moment put on air.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiringStart {
    moment_id: String,
    card_id: String,
    style: AiringStyle,
    source: AiringSource,
    break_kind: Option<String>,
    at: String,
}

impl AiringStart {
    pub fn moment_id(&self) -> &str {
        &self.moment_id
    }
    pub fn card_id(&self) -> &str {
        &self.card_id
    }
    pub fn style(&self) -> AiringStyle {
        self.style
    }
    pub fn source(&self) -> AiringSource {
        self.source
    }
    /// `None` for a carousel showing: a scene is not a break.
    pub fn break_kind(&self) -> Option<&str> {
        self.break_kind.as_deref()
    }
    pub fn at(&self) -> &str {
        &self.at
    }
}

/// A `seazn:sponsor:end` line: the moment left the air, cut short or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiringEnd {
    moment_id: String,
    at: String,
    cut_short: bool,
}

impl AiringEnd {
    pub fn moment_id(&self) -> &str {
        &self.moment_id
    }
    pub fn at(&self) -> &str {
        &self.at
    }
    pub fn cut_short(&self) -> bool {
        self.cut_short
    }
}

/// One proof-of-play line, within web's bounds (the constructors refuse anything else).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiringLine {
    Start(AiringStart),
    End(AiringEnd),
}

impl AiringLine {
    /// A start line, or the first bound it breaks (in the spec's key order).
    pub fn start(
        moment_id: String,
        card_id: String,
        style: AiringStyle,
        source: AiringSource,
        break_kind: Option<String>,
        at: String,
    ) -> Result<Self, LineRefusal> {
        within("momentId", &moment_id, AIRING_ID_MAX_UNITS)?;
        within("cardId", &card_id, AIRING_ID_MAX_UNITS)?;
        if let Some(k) = &break_kind {
            within("breakKind", k, AIRING_BREAK_KIND_MAX_UNITS)?;
        }
        instant("at", &at)?;
        Ok(Self::Start(AiringStart {
            moment_id,
            card_id,
            style,
            source,
            break_kind,
            at,
        }))
    }

    /// An end line, or the first bound it breaks (in the spec's key order).
    pub fn end(moment_id: String, at: String, cut_short: bool) -> Result<Self, LineRefusal> {
        within("momentId", &moment_id, AIRING_ID_MAX_UNITS)?;
        instant("at", &at)?;
        Ok(Self::End(AiringEnd {
            moment_id,
            at,
            cut_short,
        }))
    }

    pub fn moment_id(&self) -> &str {
        match self {
            Self::Start(s) => &s.moment_id,
            Self::End(e) => &e.moment_id,
        }
    }
}

/// Refuses `text` when it is longer than `max` UTF-16 code units, the unit zod's `.max` counts.
fn within(key: &'static str, text: &str, max: usize) -> Result<(), LineRefusal> {
    if text.encode_utf16().count() > max {
        return Err(LineRefusal::TooLong(key));
    }
    Ok(())
}

fn instant(key: &'static str, text: &str) -> Result<(), LineRefusal> {
    if !is_utc_instant(text) {
        return Err(LineRefusal::NotAnInstant(key));
    }
    Ok(())
}

/// True for the instant web's `z.string().datetime()` takes (zod 4.4.3's `datetime` regex at its
/// defaults: no offset, no local form, any precision): `YYYY-MM-DDTHH:MM`, then optionally `:SS` and
/// then optionally a `.` and one or more digits, then `Z`; ASCII digits only, hours 00 to 23, minutes
/// and seconds 00 to 59, and a real calendar date (zod's `dateSource`: each month's length, 29
/// February in a leap year only, any four-digit year). The page sends `Date.toISOString()`.
pub fn is_utc_instant(s: &str) -> bool {
    let Some(b) = s.as_bytes().strip_suffix(b"Z") else {
        return false;
    };
    if b.len() < 16 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' {
        return false;
    }
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute)) = (
        number(&b[0..4]),
        number(&b[5..7]),
        number(&b[8..10]),
        number(&b[11..13]),
        number(&b[14..16]),
    ) else {
        return false;
    };
    if hour > 23 || minute > 59 || day == 0 || day > days_in(year, month) {
        return false;
    }
    let rest = &b[16..];
    if rest.is_empty() {
        return true;
    }
    let Some(rest) = rest.strip_prefix(b":") else {
        return false;
    };
    if rest.len() < 2 || number(&rest[..2]).is_none_or(|sec| sec > 59) {
        return false;
    }
    let fraction = &rest[2..];
    fraction.is_empty() || fraction.strip_prefix(b".").is_some_and(all_digits)
}

/// `epoch_ms` as the instant `is_utc_instant` takes, `Date.toISOString()`'s form
/// (`YYYY-MM-DDTHH:MM:SS.mmmZ`), for a line the compositor writes itself (a retired page's end).
pub fn utc_instant_ms(epoch_ms: u64) -> String {
    let (days, in_day) = (epoch_ms / 86_400_000, epoch_ms % 86_400_000);
    let (year, month, day) = civil(days);
    let (h, mi, sec, ms) = (
        in_day / 3_600_000,
        in_day / 60_000 % 60,
        in_day / 1_000 % 60,
        in_day % 1_000,
    );
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{mi:02}:{sec:02}.{ms:03}Z")
}

/// The Gregorian date `days` after 1970-01-01 (Howard Hinnant's `civil_from_days`, for days ≥ 0).
fn civil(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let (era, doe) = (z / 146_097, z % 146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + u64::from(month <= 2), month, day)
}

fn all_digits(b: &[u8]) -> bool {
    !b.is_empty() && b.iter().all(u8::is_ascii_digit)
}

/// A field of ASCII digits (each caller's slice is two or four long), as a number.
fn number(b: &[u8]) -> Option<u32> {
    all_digits(b).then(|| b.iter().fold(0, |n, d| n * 10 + u32::from(d - b'0')))
}

/// The days in `month` of `year` (Gregorian); 0 for a month that is none.
fn days_in(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => 29,
        2 => 28,
        _ => 0,
    }
}

/// What one beat carries: every line held when it was taken, oldest first, and the buffer's dropped
/// total then. Empty when nothing is held: the beat then carries no `airings` key, and no
/// `airingsDropped` either unless something was ever dropped (`carries_nothing`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AiringBatch {
    lines: Vec<AiringLine>,
    /// The sequence number of the newest line carried; `None` when empty.
    through: Option<u64>,
    dropped: u32,
}

impl AiringBatch {
    pub fn lines(&self) -> &[AiringLine] {
        &self.lines
    }
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
    /// The buffer's dropped total when the batch was taken (the beat's `airingsDropped`): a total for
    /// the process, and an upper bound (`AiringBuffer::dropped`).
    pub fn dropped(&self) -> u32 {
        self.dropped
    }
    /// No line and nothing ever dropped: the beat carries neither key.
    pub fn carries_nothing(&self) -> bool {
        self.is_empty() && self.dropped == 0
    }
}

/// The lines not yet acknowledged, each with its sequence number, at most `AIRING_BUFFER_CAP`.
#[derive(Debug, Clone, Default)]
pub struct AiringBuffer {
    held: VecDeque<(u64, AiringLine)>,
    next: u64,
    dropped: u32,
}

impl AiringBuffer {
    /// Holds `line`; past the cap, the oldest line is dropped and counted.
    pub fn push(&mut self, line: AiringLine) {
        if self.held.len() >= AIRING_BUFFER_CAP {
            self.held.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.held.push_back((self.next, line));
        self.next += 1;
    }

    /// The next beat's lines: a snapshot, nothing removed.
    pub fn take_for_beat(&self) -> AiringBatch {
        AiringBatch {
            lines: self.held.iter().map(|(_, l)| l.clone()).collect(),
            through: self.held.back().map(|(seq, _)| *seq),
            dropped: self.dropped,
        }
    }

    /// A 2xx answered the beat that carried `batch`: its lines leave, and every line pushed after it
    /// was taken stays. Sequence numbers only rise, so they are the held lines at the front.
    pub fn ack_2xx(&mut self, batch: &AiringBatch) {
        let Some(through) = batch.through else {
            return;
        };
        while self.held.front().is_some_and(|(seq, _)| *seq <= through) {
            self.held.pop_front();
        }
    }

    /// Web refused the beat that carried `batch` (a 400 or a 422: the same body is refused again, and a
    /// strict heartbeat that keeps being refused loses the session). Its lines still held leave and
    /// are counted dropped, and every line pushed after it was taken stays. The number that left.
    pub fn drop_refused(&mut self, batch: &AiringBatch) -> usize {
        let Some(through) = batch.through else {
            return 0;
        };
        let mut left = 0;
        while self.held.front().is_some_and(|(seq, _)| *seq <= through) {
            self.held.pop_front();
            left += 1;
        }
        let counted = u32::try_from(left).unwrap_or(u32::MAX);
        self.dropped = self.dropped.saturating_add(counted);
        left
    }

    pub fn len(&self) -> usize {
        self.held.len()
    }
    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }
    /// Lines dropped, over the process: past the cap, or carried by a beat web refused. An upper
    /// bound: a line dropped past the cap while a beat in flight carries it is counted even when that
    /// beat lands.
    pub fn dropped(&self) -> u32 {
        self.dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT: &str = "2026-10-31T12:00:00.000Z";

    fn start(n: u64) -> AiringLine {
        AiringLine::start(
            format!("{n}:sponsor.set_break"),
            "card-1".to_owned(),
            AiringStyle::LowerThird,
            AiringSource::Rotation,
            Some("set_break".to_owned()),
            AT.to_owned(),
        )
        .expect("within web's bounds")
    }

    fn ids(b: &AiringBatch) -> Vec<String> {
        b.lines().iter().map(|l| l.moment_id().to_owned()).collect()
    }

    fn want(lo: u64, hi: u64) -> Vec<String> {
        (lo..=hi).map(|n| format!("{n}:sponsor.set_break")).collect()
    }

    #[test]
    fn an_empty_buffer_gives_an_empty_batch_and_an_ack_of_it_changes_nothing() {
        let mut b = AiringBuffer::default();
        let batch = b.take_for_beat();
        assert!(batch.is_empty());
        assert_eq!(batch.dropped(), 0);
        b.ack_2xx(&batch);
        b.push(start(1));
        b.ack_2xx(&batch);
        assert_eq!(
            ids(&b.take_for_beat()),
            want(1, 1),
            "an empty batch's ack removes nothing"
        );
    }

    #[test]
    fn the_65th_line_drops_the_oldest_and_counts_airings_dropped() {
        let mut b = AiringBuffer::default();
        for n in 1..=AIRING_BUFFER_CAP as u64 {
            b.push(start(n));
        }
        assert_eq!(b.len(), AIRING_BUFFER_CAP);
        assert_eq!(b.dropped(), 0, "64 fit");
        b.push(start(65));
        let batch = b.take_for_beat();
        assert_eq!(ids(&batch), want(2, 65), "the oldest left, the 65th is held");
        assert_eq!(batch.dropped(), 1);
        b.push(start(66));
        b.push(start(67));
        let batch = b.take_for_beat();
        assert_eq!(ids(&batch), want(4, 67));
        assert_eq!(batch.dropped(), 3, "a total, not the last push's");
        assert_eq!(batch.lines().len(), 64, "never more than web's max(64)");
    }

    #[test]
    fn a_beat_answered_5xx_keeps_the_items_and_a_2xx_clears_them() {
        let mut b = AiringBuffer::default();
        b.push(start(1));
        b.push(start(2));
        let first = b.take_for_beat();
        // A 5xx (or any failure): nothing acknowledged, so the next beat resends the same lines.
        let again = b.take_for_beat();
        assert_eq!(
            again, first,
            "a second identical beat after a failure resends the same items"
        );
        assert_eq!(ids(&again), want(1, 2));
        b.ack_2xx(&again);
        assert!(b.is_empty(), "the 2xx cleared what it carried");
        assert!(b.take_for_beat().is_empty(), "and the next beat carries none");
    }

    #[test]
    fn a_2xx_clears_only_what_its_beat_carried() {
        let mut b = AiringBuffer::default();
        b.push(start(1));
        let carried = b.take_for_beat();
        b.push(start(2));
        b.ack_2xx(&carried);
        assert_eq!(
            ids(&b.take_for_beat()),
            want(2, 2),
            "a line pushed after the snapshot stays"
        );
        // A late 2xx of an older beat removes nothing it did not carry.
        b.ack_2xx(&carried);
        assert_eq!(ids(&b.take_for_beat()), want(2, 2));
    }

    /// Fix round 1, I2: a beat web refused (400/422) would be refused again, so what it carried leaves,
    /// counted; a line pushed after its snapshot stays. A second call, and an empty batch, change
    /// nothing.
    #[test]
    fn a_refused_beat_drops_and_counts_what_it_carried_and_keeps_later_lines() {
        let mut b = AiringBuffer::default();
        assert_eq!(
            b.drop_refused(&b.take_for_beat()),
            0,
            "an empty batch drops nothing"
        );
        assert_eq!((b.len(), b.dropped()), (0, 0));
        b.push(start(1));
        b.push(start(2));
        let carried = b.take_for_beat();
        b.push(start(3));
        assert_eq!(b.drop_refused(&carried), 2);
        assert_eq!(b.dropped(), 2);
        let next = b.take_for_beat();
        assert_eq!(
            (ids(&next), next.dropped()),
            (want(3, 3), 2),
            "line 3 stays; 2 counted"
        );
        assert_eq!(b.drop_refused(&carried), 0, "a second call drops nothing more");
        assert_eq!((b.len(), b.dropped()), (1, 2));

        // Past the cap first: two dropped by the cap, then the refused beat's 62 still held.
        let mut b = AiringBuffer::default();
        for n in 1..=AIRING_BUFFER_CAP as u64 {
            b.push(start(n));
        }
        let carried = b.take_for_beat();
        b.push(start(65));
        b.push(start(66));
        assert_eq!(b.dropped(), 2);
        assert_eq!(b.drop_refused(&carried), 62);
        assert_eq!((ids(&b.take_for_beat()), b.dropped()), (want(65, 66), 64));
    }

    /// m2: a beat carries `airingsDropped` whenever the total is above 0, lines or none.
    #[test]
    fn a_batch_carries_nothing_only_with_no_line_and_nothing_dropped() {
        let mut b = AiringBuffer::default();
        assert!(b.take_for_beat().carries_nothing());
        b.push(start(1));
        assert!(!b.take_for_beat().carries_nothing());
        let carried = b.take_for_beat();
        b.drop_refused(&carried);
        let after = b.take_for_beat();
        assert!(after.is_empty() && after.dropped() == 1);
        assert!(!after.carries_nothing(), "no line, but a dropped total to report");
    }

    #[test]
    fn an_ack_after_the_cap_dropped_lines_it_carried_removes_only_those_still_held() {
        let mut b = AiringBuffer::default();
        for n in 1..=AIRING_BUFFER_CAP as u64 {
            b.push(start(n));
        }
        let carried = b.take_for_beat();
        for n in 65..=70 {
            b.push(start(n));
        }
        b.ack_2xx(&carried);
        assert_eq!(ids(&b.take_for_beat()), want(65, 70));
        assert_eq!(b.dropped(), 6);
    }

    #[test]
    fn the_final_beat_flushes_the_buffer() {
        // The final beat is a beat: it carries every line still held, the ones pushed after a failed
        // periodic beat included, and its 2xx leaves nothing.
        let mut b = AiringBuffer::default();
        b.push(start(1));
        let periodic = b.take_for_beat();
        b.push(start(2));
        let last = b.take_for_beat();
        assert_ne!(last, periodic);
        assert_eq!(ids(&last), want(1, 2));
        b.ack_2xx(&last);
        assert!(b.is_empty());
    }

    #[test]
    fn a_scene_start_and_an_end_are_held_like_a_break_start() {
        let mut b = AiringBuffer::default();
        let scene = AiringLine::start(
            "scene:7".to_owned(),
            "card-2".to_owned(),
            AiringStyle::Banner,
            AiringSource::Scene,
            None,
            AT.to_owned(),
        )
        .expect("a scene start: breakKind null");
        let end = AiringLine::end("scene:7".to_owned(), AT.to_owned(), true).expect("an end");
        b.push(scene.clone());
        b.push(end.clone());
        assert_eq!(b.take_for_beat().lines(), &[scene, end]);
    }
}
