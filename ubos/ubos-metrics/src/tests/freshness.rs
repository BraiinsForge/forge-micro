// Copyright (C) 2026  Braiins Systems s.r.o.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Braiins Systems s.r.o. reserves the right to grant any party a license
// to this program, or any part thereof, under any terms, and such a grant
// shall be considered distinct from the grant above.

//! Coverage for the TTL of a measured metric and for the two states the
//! builder methods leave a metric in.

use core::num::NonZeroU32;

use embassy_time::Duration;

use crate::consts::Metrics;
use crate::freshness::Expiry;
use crate::metrics::Metric;
use crate::name::Name;
use crate::value::Count;

fn metric() -> Metric {
    Metric::new(Name::new("test_metric"), Count(0))
}

fn ttl_secs(metric: &Metric) -> Option<NonZeroU32> {
    metric.expiry().map(Expiry::ttl_secs)
}

/// A TTL below one second would store zero, so it rounds up to one.
#[test]
fn a_sub_second_ttl_rounds_up_to_one_second() {
    let metric = metric().ttl(Duration::from_millis(1));
    assert_eq!(ttl_secs(&metric), Some(NonZeroU32::MIN));
}

#[test]
fn a_ttl_rounds_up_to_whole_seconds() {
    let metric = metric().ttl(Duration::from_millis(1_001));
    assert_eq!(ttl_secs(&metric), NonZeroU32::new(2));
}

#[test]
fn a_huge_ttl_is_held_at_the_largest_count() {
    let metric = metric().ttl(Duration::from_secs(u64::from(u32::MAX) + 10));
    assert_eq!(ttl_secs(&metric), Some(NonZeroU32::MAX));
}

#[test]
fn a_new_metric_takes_the_default_ttl() {
    let secs = u32::try_from(Metrics::DEFAULT_TTL.as_secs()).expect("the default fits");
    assert_eq!(ttl_secs(&metric()), NonZeroU32::new(secs));
}

#[test]
fn ttl_after_constant_gives_a_measured_metric() {
    let metric = metric().constant().ttl(Duration::from_secs(5));
    assert_eq!(ttl_secs(&metric), NonZeroU32::new(5));
}

#[test]
fn constant_after_ttl_removes_the_expiry() {
    let metric = metric().ttl(Duration::from_secs(5)).constant();
    assert!(metric.expiry().is_none());
}
