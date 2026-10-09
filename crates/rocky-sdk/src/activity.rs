//! What a node did in the background, newest last, for an application to
//! show: sync transfers, checkpoints, horizons, enrollment, renewal, author
//! and group key changes. Bounded and in memory only; see
//! `docs/runtime/activity.md`.
use serde_json::{Value, json};
use std::{collections::VecDeque, time::SystemTime};
use tokio::sync::watch;

/// Events kept; an observer further behind misses the oldest.
const KEPT: usize = 512;
/// Events one call returns.
const PAGE: usize = 128;

#[derive(Default)]
struct Log {
    /// The sequence of the next event, from 1.
    next: u64,
    events: VecDeque<Value>,
}

pub(crate) struct Activity(watch::Sender<Log>);
impl Activity {
    pub fn new() -> Self {
        Self(
            watch::channel(Log {
                next: 1,
                events: VecDeque::new(),
            })
            .0,
        )
    }
    /// Appends `kind` with `detail`'s fields, a sequence and a time.
    pub fn record(&self, kind: &str, detail: Value) {
        let at = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as u64)
            .unwrap_or_default();
        self.0.send_modify(|log| {
            let mut event =
                json!({"sequence": log.next.to_string(), "at": at.to_string(), "kind": kind});
            if let (Some(event), Value::Object(fields)) = (event.as_object_mut(), detail) {
                event.extend(fields);
            }
            log.next += 1;
            log.events.push_back(event);
            if log.events.len() > KEPT {
                log.events.pop_front();
            }
        });
    }
    /// Records like [`Self::record`] unless the latest event of `kind` had
    /// the same detail, as a retried pass would repeat it.
    pub fn record_changed(&self, kind: &str, detail: Value) {
        let repeated = self
            .0
            .borrow()
            .events
            .iter()
            .rev()
            .find(|event| event["kind"] == kind)
            .is_some_and(|event| {
                detail
                    .as_object()
                    .is_some_and(|fields| fields.iter().all(|(name, value)| &event[name] == value))
            });
        if !repeated {
            self.record(kind, detail);
        }
    }
    /// The events after sequence `after`, at most a page, and the token to
    /// pass next.
    pub fn after(&self, after: u64) -> Value {
        let log = self.0.borrow();
        let events: Vec<&Value> = log
            .events
            .iter()
            .filter(|event| sequence(event) > after)
            .take(PAGE)
            .collect();
        let token = events
            .last()
            .map_or(after.max(log.next - 1), |event| sequence(event));
        json!({"events": events, "token": token.to_string()})
    }
    /// Waits until an event after `after` exists, or `closing` turns true.
    pub async fn next(&self, after: u64, closing: &mut watch::Receiver<bool>) -> Option<Value> {
        let mut log = self.0.subscribe();
        loop {
            if log.borrow_and_update().next - 1 > after {
                return Some(self.after(after));
            }
            tokio::select! {
                changed = log.changed() => changed.ok()?,
                _ = closing.wait_for(|closing| *closing) => return None,
            }
        }
    }
}

fn sequence(event: &Value) -> u64 {
    event["sequence"]
        .as_str()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pages_are_ordered_bounded_and_resume_after_the_token() {
        let activity = Activity::new();
        for index in 0..(KEPT + 10) {
            activity.record("synced", json!({"index": index}));
        }
        let first = activity.after(0);
        let events = first["events"].as_array().unwrap();
        assert_eq!(events.len(), PAGE);
        // The ten oldest were dropped.
        assert_eq!(events[0]["index"], 10);
        assert_eq!(events[0]["kind"], "synced");
        let token: u64 = first["token"].as_str().unwrap().parse().unwrap();
        assert_eq!(activity.after(token)["events"][0]["index"], 10 + PAGE);
        let (_, mut closing) = watch::channel(false);
        let last = (KEPT + 10) as u64;
        let waiting = activity.next(last, &mut closing);
        activity.record("online", json!({}));
        let next = waiting.await.unwrap();
        assert_eq!(next["events"][0]["kind"], "online");
        assert_eq!(next["token"], (last + 1).to_string());
    }

    #[test]
    fn a_repeated_event_is_kept_once() {
        let activity = Activity::new();
        activity.record_changed("bootstrap_started", json!({"peer": "a", "scopes": 2}));
        activity.record_changed("bootstrap_started", json!({"peer": "a", "scopes": 2}));
        activity.record_changed("bootstrap_started", json!({"peer": "a", "scopes": 1}));
        assert_eq!(activity.after(0)["events"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn an_empty_log_answers_its_current_token() {
        let activity = Activity::new();
        assert_eq!(activity.after(0), json!({"events": [], "token": "0"}));
        activity.record("online", json!({}));
        assert_eq!(activity.after(5)["token"], "5");
    }
}
