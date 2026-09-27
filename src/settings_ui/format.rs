// Text the panes show that is worked out rather than written: how long ago a
// transcript was made, and how far the model download has got. Pure, so it
// is asserted without a window.

use crate::transcribe::parakeet_download::Progress;

/// Coarse "x ago" rendering of a Unix timestamp relative to `now` — enough to
/// orient a recovery, without pulling in a date library.
pub(super) fn relative_time(now: i64, ts: i64) -> String {
    let secs = (now - ts).max(0);
    if secs < 60 {
        "just now".into()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// The whole download's progress, 0..=1: each file an equal share, whatever
/// its size, since the sizes aren't known until each one starts.
pub(super) fn progress_fraction(p: &Progress) -> f32 {
    let per_file = 1.0 / p.file_count.max(1) as f32;
    let within = match p.bytes_total {
        Some(total) if total > 0 => (p.bytes_done as f32 / total as f32).clamp(0.0, 1.0),
        _ => 0.0,
    };
    (p.file_index as f32 * per_file + within * per_file).clamp(0.0, 1.0)
}

/// "file/files  done / total MB", or just the bytes done when the server
/// didn't say how many there are.
pub(super) fn progress_label(p: &Progress) -> String {
    let done_mb = p.bytes_done as f64 / 1_048_576.0;
    match p.bytes_total {
        Some(total) if total > 0 => {
            let total_mb = total as f64 / 1_048_576.0;
            format!(
                "{}/{}  {:>6.1} / {:>6.1} MB",
                p.file_index + 1,
                p.file_count,
                done_mb,
                total_mb
            )
        }
        _ => format!("{}/{}  {:>6.1} MB", p.file_index + 1, p.file_count, done_mb),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(file_index: usize, bytes_done: u64, bytes_total: Option<u64>) -> Progress {
        Progress {
            file_index,
            file_count: 2,
            bytes_done,
            bytes_total,
        }
    }

    #[test]
    fn relative_time_rounds_down_to_the_largest_whole_unit() {
        let now = 1_000_000;
        assert_eq!(relative_time(now, now), "just now");
        assert_eq!(relative_time(now, now - 59), "just now");
        assert_eq!(relative_time(now, now - 60), "1m ago");
        assert_eq!(relative_time(now, now - 3599), "59m ago");
        assert_eq!(relative_time(now, now - 3600), "1h ago");
        assert_eq!(relative_time(now, now - 86_399), "23h ago");
        assert_eq!(relative_time(now, now - 3 * 86_400), "3d ago");
    }

    /// A clock that stepped back since the entry was written is not "in the
    /// future".
    #[test]
    fn a_timestamp_ahead_of_now_is_just_now() {
        assert_eq!(relative_time(1_000, 5_000), "just now");
    }

    #[test]
    fn each_file_is_an_equal_share_of_the_download() {
        assert_eq!(progress_fraction(&progress(0, 0, Some(100))), 0.0);
        assert_eq!(progress_fraction(&progress(0, 50, Some(100))), 0.25);
        assert_eq!(progress_fraction(&progress(1, 0, Some(10))), 0.5);
        assert_eq!(progress_fraction(&progress(1, 10, Some(10))), 1.0);
    }

    #[test]
    fn a_file_of_unknown_size_counts_as_not_started() {
        assert_eq!(progress_fraction(&progress(1, 500, None)), 0.5);
        assert_eq!(progress_fraction(&progress(1, 500, Some(0))), 0.5);
    }

    #[test]
    fn progress_never_leaves_zero_to_one() {
        assert_eq!(progress_fraction(&progress(1, 20, Some(10))), 1.0);
        let none = Progress {
            file_index: 0,
            file_count: 0,
            bytes_done: 0,
            bytes_total: None,
        };
        assert_eq!(progress_fraction(&none), 0.0);
    }

    #[test]
    fn the_progress_label_counts_files_from_one_and_bytes_in_mb() {
        assert_eq!(
            progress_label(&progress(0, 1_048_576, Some(10 * 1_048_576))),
            "1/2     1.0 /   10.0 MB"
        );
        assert_eq!(
            progress_label(&progress(1, 3 * 524_288, None)),
            "2/2     1.5 MB"
        );
    }
}
