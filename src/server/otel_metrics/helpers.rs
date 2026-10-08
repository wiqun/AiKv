//! OTel counter / updown delta.

pub(super) fn counter_delta(current: u64, last: &mut u64) -> u64 {
    let delta = current.saturating_sub(*last);
    *last = current;
    delta
}

pub(super) fn updown_delta(current: i64, last: &mut i64) -> i64 {
    let delta = current - *last;
    *last = current;
    delta
}
