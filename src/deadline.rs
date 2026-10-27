/*
 * KYB (Know Your Business) — git-backed knowledge base and incident tracker for AI agent fleets
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/kyb
 * SPDX-License-Identifier: MIT
 */

//! Due dates on tasks, and what the server says about them on its own.
//!
//! A date written into a task title as prose cannot be sorted, filtered or
//! noticed: the base knows the certificate expires on the 14th, but nobody hears
//! it unless they open the right entry. So a task carries its deadline as data,
//! and every read response carries a short block of the deadlines that are
//! already late or inside their reminder window - computed here, once, for every
//! client alike. Nobody has to remember to ask.
//!
//! "Today" is a calendar date, and whose calendar decides it. A deadline written
//! as a bare date is due on that day wherever the caller is; one written with a
//! time and an offset counts by its own day the same way, and is also late once
//! that instant has passed.

use anyhow::{bail, Result};
use chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};

/// How far ahead a soft deadline starts being surfaced when the task says nothing.
pub const REMIND_DAYS: u32 = 14;
/// Hard deadlines are surfaced earlier: missing one costs something real.
pub const REMIND_DAYS_HARD: u32 = 30;
/// A reminder a year and more ahead is a calendar, not a reminder.
pub const REMIND_DAYS_MAX: u32 = 366;
/// How many deadlines a response block lists before it says "+N more".
pub const BLOCK_CAP: usize = 5;
/// The listing's middle group: due within this many days.
pub const THIS_WEEK_DAYS: i64 = 7;

pub(crate) fn is_false(b: &bool) -> bool {
    !*b
}

/// The deadline of one task. Every field is optional and an empty one is absent
/// from the frontmatter, so a task without a deadline is stored exactly as before.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Deadline {
    /// `YYYY-MM-DD`, or an RFC3339 date-time with its offset.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub due: String,
    /// Missing it has consequences outside the team: an expiry, a contractual or
    /// regulatory date, a third party's cut-off. Hard deadlines come first.
    #[serde(default, skip_serializing_if = "is_false")]
    pub hard: bool,
    /// Days ahead the deadline starts being surfaced; absent = the server default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remind_days: Option<u32>,
    /// The event the real date depends on ("the vendor's quote arrives").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub waits_on: String,
    /// A provisional date, reminded on until the real `due` is known.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub expected: String,
}

/// A date as written: a calendar day, or an instant with its offset.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum When {
    Day(NaiveDate),
    At(DateTime<FixedOffset>),
}

/// `2026-03-14`, `2026-03-14T17:00:00-07:00`, or the same without seconds. A time
/// without a zone is refused: it is a different instant on every machine.
pub fn parse_when(raw: &str) -> Option<When> {
    let s = raw.trim();
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some(When::Day(d));
    }
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(When::At(t));
    }
    DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M%:z").ok().map(When::At)
}

impl Deadline {
    pub fn is_empty(&self) -> bool {
        *self == Deadline::default()
    }

    /// The date reminders run on, as written, and whether it is provisional:
    /// `due` once it is known, `expected` until then.
    pub fn effective(&self) -> Option<(When, &str, bool)> {
        let due = self.due.trim();
        if !due.is_empty() {
            return parse_when(due).map(|w| (w, due, false));
        }
        let expected = self.expected.trim();
        if !expected.is_empty() {
            return parse_when(expected).map(|w| (w, expected, true));
        }
        None
    }

    /// Days ahead this deadline is surfaced.
    pub fn window(&self) -> u32 {
        self.remind_days.unwrap_or(if self.hard { REMIND_DAYS_HARD } else { REMIND_DAYS })
    }

    /// The fields as `kyb diff` shows them: a cleared value is an empty string,
    /// like every other field, so moving a due date reads as `from` -> `to`.
    pub fn diff_fields(&self) -> [(&'static str, String); 5] {
        [
            ("due", self.due.clone()),
            ("hard", if self.hard { "true".into() } else { String::new() }),
            ("remind_days", self.remind_days.map(|d| d.to_string()).unwrap_or_default()),
            ("waits_on", self.waits_on.clone()),
            ("expected", self.expected.clone()),
        ]
    }

    pub fn validate(&self, kind: &str, is_task: bool) -> Result<()> {
        if self.is_empty() {
            return Ok(());
        }
        if !is_task {
            bail!("due, hard, remind_days, waits_on and expected are task-only fields (kind '{kind}')");
        }
        for (name, value) in [("due", &self.due), ("expected", &self.expected)] {
            if !value.trim().is_empty() && parse_when(value).is_none() {
                bail!(
                    "{name} must be YYYY-MM-DD or a date-time with its zone, e.g. 2026-03-14T17:00:00-07:00 (got '{value}')"
                );
            }
        }
        let dated = !self.due.trim().is_empty() || !self.expected.trim().is_empty();
        if !dated && (self.hard || self.remind_days.is_some() || !self.waits_on.trim().is_empty()) {
            bail!(
                "hard, remind_days and waits_on describe a deadline: set due, or expected while the real date is not known yet"
            );
        }
        if let Some(days) = self.remind_days {
            if days > REMIND_DAYS_MAX {
                bail!("remind_days must be at most {REMIND_DAYS_MAX} (got {days})");
            }
        }
        if !self.waits_on.is_empty() && self.waits_on.trim().is_empty() {
            bail!("waits_on is blank — leave it out if the date does not depend on an event");
        }
        if let Some(hit) = crate::model::find_secret(&self.waits_on) {
            bail!("waits_on looks like a secret ({hit}…) — store secrets as pointers in refs");
        }
        Ok(())
    }
}

/// Present in the request = set it (`""`, `false` or `null` clears it); absent =
/// keep what is stored. A task rewritten to fix a typo in its body must not lose
/// the deadline nobody mentioned.
///
/// Each field is `Some(None)` for an explicit `null`, so `null` clears like the
/// empty value does instead of reading as "not mentioned".
#[derive(Debug, Default, Deserialize)]
pub struct DeadlinePatch {
    #[serde(default, deserialize_with = "present")]
    pub due: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    pub hard: Option<Option<bool>>,
    #[serde(default, deserialize_with = "present")]
    pub remind_days: Option<Option<u32>>,
    #[serde(default, deserialize_with = "present")]
    pub waits_on: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    pub expected: Option<Option<String>>,
}

/// Tells an explicit `null` (Some(None): clear) from an absent field (None: keep).
fn present<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(d).map(Some)
}

impl DeadlinePatch {
    /// Names no field at all.
    pub fn is_empty(&self) -> bool {
        self.due.is_none()
            && self.hard.is_none()
            && self.remind_days.is_none()
            && self.waits_on.is_none()
            && self.expected.is_none()
    }

    pub fn apply(self, mut d: Deadline) -> Deadline {
        let (names_hard, names_days, names_event) =
            (self.hard.is_some(), self.remind_days.is_some(), self.waits_on.is_some());
        if let Some(due) = self.due {
            d.due = due.unwrap_or_default().trim().to_string();
        }
        if let Some(hard) = self.hard {
            d.hard = hard.unwrap_or(false);
        }
        if let Some(days) = self.remind_days {
            d.remind_days = days;
        }
        if let Some(waits_on) = self.waits_on {
            d.waits_on = waits_on.unwrap_or_default();
        }
        if let Some(expected) = self.expected {
            d.expected = expected.unwrap_or_default().trim().to_string();
        }
        // A deadline left without a date is gone as a whole: what described it -
        // how hard it was, how early to remind, the event it waited on - goes too,
        // unless this very request sets it again (and is then refused for having
        // no date, which is the honest answer).
        if d.due.trim().is_empty() && d.expected.trim().is_empty() {
            if !names_hard {
                d.hard = false;
            }
            if !names_days {
                d.remind_days = None;
            }
            if !names_event {
                d.waits_on.clear();
            }
        }
        d
    }
}

/// The moment a question is asked, and the caller's calendar date at it.
#[derive(Debug, Clone, Copy)]
pub struct Clock {
    pub now: DateTime<Utc>,
    pub today: NaiveDate,
}

impl Clock {
    /// `today` as the caller sent it, or the server's own date in its time zone
    /// (`TZ`) when the caller did not say.
    pub fn new(now: DateTime<Utc>, today: Option<NaiveDate>) -> Clock {
        let today = today.unwrap_or_else(|| now.with_timezone(&chrono::Local).date_naive());
        Clock { now, today }
    }
}

/// What a deadline computation needs to know about a task.
pub struct Task<'a> {
    pub key: &'a str,
    pub title: &'a str,
    pub status: &'a str,
    pub assignee: &'a str,
    pub tags: &'a [String],
    pub deadline: &'a Deadline,
}

/// One deadline as every client sees it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Item {
    pub key: String,
    pub title: String,
    pub status: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub assignee: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// The date reminders run on, as written: `due`, or `expected` while provisional.
    pub due: String,
    /// 0 = due today; negative = that many days late.
    pub days_left: i64,
    pub overdue: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub hard: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub provisional: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub waits_on: String,
    /// Days ahead this one is surfaced.
    pub remind_days: u32,
}

impl Item {
    /// Late, or close enough that its reminder window has opened.
    pub fn surfaced(&self) -> bool {
        self.overdue || self.days_left <= i64::from(self.remind_days)
    }

    /// The listing group: overdue, this_week or later.
    pub fn bucket(&self) -> &'static str {
        if self.overdue {
            "overdue"
        } else if self.days_left <= THIS_WEEK_DAYS {
            "this_week"
        } else {
            "later"
        }
    }

    /// Carries every tag of `scope` (tags compare case-insensitively, as the
    /// index stores them).
    pub fn in_scope(&self, scope: &[String]) -> bool {
        scope.iter().all(|s| self.tags.iter().any(|t| t.eq_ignore_ascii_case(s)))
    }
}

/// The deadline of a task, as of `clock`. None when the task has no date.
pub fn item(task: &Task, clock: &Clock) -> Option<Item> {
    let (when, written, provisional) = task.deadline.effective()?;
    let (days_left, overdue) = match when {
        When::Day(day) => ((day - clock.today).num_days(), clock.today > day),
        When::At(at) => {
            // The calendar first, exactly as for a bare date: the deadline's day
            // in its own zone against the caller's today, so the two kinds agree
            // about what day it is - and a today sent ahead (a preview, a test)
            // sees it late. The instant only adds what a calendar cannot: once it
            // has passed, the deadline is late even on its own day, or on the day
            // before for a caller whose calendar runs behind the deadline's zone,
            // and then no count of days to go is left standing.
            //
            // The instant is the real clock, so it speaks only for a today that is
            // the present somewhere on Earth - every zone's date is within a day of
            // the UTC one. A today sent for another time, past or future, is a
            // question about that day, and the calendar alone answers it.
            let day = at.date_naive();
            let days = (day - clock.today).num_days();
            let present = (clock.today - clock.now.date_naive()).num_days().abs() <= 1;
            let overdue = clock.today > day || (present && clock.now > at);
            (if overdue { days.min(0) } else { days }, overdue)
        }
    };
    Some(Item {
        key: task.key.to_string(),
        title: task.title.to_string(),
        status: task.status.to_string(),
        assignee: task.assignee.to_string(),
        tags: task.tags.to_vec(),
        due: written.to_string(),
        days_left,
        overdue,
        hard: task.deadline.hard,
        provisional,
        // the event matters while the date hangs on it; once the real date is
        // known it is history, kept in the record, not repeated in reminders
        waits_on: if provisional { task.deadline.waits_on.clone() } else { String::new() },
        remind_days: task.deadline.window(),
    })
}

/// What is already late first - hard before soft, the latest first - then what is
/// coming, hard before soft, the nearest first; by key last, so the order never
/// depends on the index. A late task is the most urgent line there is: a distant
/// hard deadline must never push it down, let alone out of a capped block.
pub fn sort(items: &mut [Item]) {
    items.sort_by(|a, b| {
        b.overdue
            .cmp(&a.overdue)
            .then(b.hard.cmp(&a.hard))
            .then(a.days_left.cmp(&b.days_left))
            .then(a.key.cmp(&b.key))
    });
}

/// The block a read response carries: what is late or inside its window,
/// capped. The late ones come first whatever was asked; inside each part the
/// deadlines of the asked-about area come first. None when there is nothing to
/// say - the block is absent then, not empty.
pub fn block(mut items: Vec<Item>, clock: &Clock, scope: &[String]) -> Option<Value> {
    items.retain(Item::surfaced);
    if items.is_empty() {
        return None;
    }
    sort(&mut items);
    if !scope.is_empty() {
        // stable: the order inside each part stays the one sort() decided, and
        // the area moves up only among its own kind - never above a late task
        items.sort_by_key(|i| (!i.overdue, !i.in_scope(scope)));
    }
    let count = items.len();
    let overdue = items.iter().filter(|i| i.overdue).count();
    items.truncate(BLOCK_CAP);
    Some(json!({
        "today": clock.today.to_string(),
        "count": count,
        "overdue": overdue,
        "more": count - items.len(),
        "items": items,
    }))
}

#[cfg(test)]
#[path = "deadline_tests.rs"]
mod tests;
