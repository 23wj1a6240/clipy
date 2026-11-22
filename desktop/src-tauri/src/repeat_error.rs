//! Keep a persistent per-frame or per-chunk failure from creating a Sentry error
//! event for every frame or audio chunk.
//! Every distinct message is reported once per recording. Repeats are counted
//! and produce logarithmically spaced logs; new messages always fail open.

use std::collections::HashMap;

const MAX_TRACKED_ERRORS: usize = 64;
const MAX_MESSAGE_BYTES: usize = 4_096;

#[derive(Debug, PartialEq, Eq)]
pub enum ErrorReport {
    First,
    Repeat(u64),
    Suppress,
}

#[derive(Default)]
pub struct RepeatErrorTracker {
    counts: HashMap<String, u64>,
}

impl RepeatErrorTracker {
    pub fn observe(&mut self, message: &str) -> ErrorReport {
        if let Some(count) = self.counts.get_mut(message) {
            *count = count.saturating_add(1);
            return if count.is_power_of_two() {
                ErrorReport::Repeat(*count)
            } else {
                ErrorReport::Suppress
            };
        }
        // Bound memory without collapsing different failures into one bucket.
        // An untracked or oversized message is always reported as an error.
        if self.counts.len() < MAX_TRACKED_ERRORS && message.len() <= MAX_MESSAGE_BYTES {
            self.counts.insert(message.to_owned(), 1);
        }
        ErrorReport::First
    }

    pub fn repeated(&self) -> impl Iterator<Item = (&str, u64)> {
        self.counts
            .iter()
            .filter(|(_, count)| **count > 1)
            .map(|(message, count)| (message.as_str(), *count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ten_thousand_failed_frames_keep_one_error_report_and_repeat_counts() {
        let mut tracker = RepeatErrorTracker::default();
        let mut first_reports = 0;
        let mut repeat_reports = Vec::new();
        for _ in 0..10_000 {
            match tracker.observe("queue_video: video write_interleaved: Invalid argument") {
                ErrorReport::First => first_reports += 1,
                ErrorReport::Repeat(count) => repeat_reports.push(count),
                ErrorReport::Suppress => {}
            }
        }
        assert_eq!(first_reports, 1);
        assert_eq!(
            repeat_reports,
            vec![2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096, 8192]
        );
        assert_eq!(tracker.repeated().next().unwrap().1, 10_000);
    }

    #[test]
    fn alternating_failures_and_different_operations_each_keep_their_first_report() {
        let mut tracker = RepeatErrorTracker::default();
        for message in [
            "queue_video: invalid argument",
            "queue_video: disk full",
            "queue_video (drain): invalid argument",
        ] {
            assert_eq!(tracker.observe(message), ErrorReport::First);
        }
        assert_eq!(
            tracker.observe("queue_video: invalid argument"),
            ErrorReport::Repeat(2)
        );
        assert_eq!(
            tracker.observe("queue_video: disk full"),
            ErrorReport::Repeat(2)
        );
        assert_eq!(tracker.repeated().count(), 2);
    }

    #[test]
    fn each_recording_reports_the_same_failure_again() {
        let mut first_recording = RepeatErrorTracker::default();
        assert_eq!(first_recording.observe("failure"), ErrorReport::First);
        assert_eq!(first_recording.observe("failure"), ErrorReport::Repeat(2));
        let mut second_recording = RepeatErrorTracker::default();
        assert_eq!(second_recording.observe("failure"), ErrorReport::First);
        assert_eq!(second_recording.repeated().count(), 0);
    }

    #[test]
    fn full_tracker_never_hides_new_errors() {
        let mut tracker = RepeatErrorTracker::default();
        for index in 0..MAX_TRACKED_ERRORS {
            assert_eq!(
                tracker.observe(&format!("error-{index}")),
                ErrorReport::First
            );
        }
        assert_eq!(tracker.observe("untracked failure"), ErrorReport::First);
        assert_eq!(tracker.observe("untracked failure"), ErrorReport::First);
        assert_eq!(tracker.observe("error-0"), ErrorReport::Repeat(2));
        assert_eq!(tracker.counts.len(), MAX_TRACKED_ERRORS);
    }

    #[test]
    fn oversized_messages_are_reported_without_retaining_them() {
        let mut tracker = RepeatErrorTracker::default();
        let message = "x".repeat(MAX_MESSAGE_BYTES + 1);
        assert_eq!(tracker.observe(&message), ErrorReport::First);
        assert_eq!(tracker.observe(&message), ErrorReport::First);
        assert!(tracker.counts.is_empty());
    }
}
