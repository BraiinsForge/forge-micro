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

//! Tests for the wait of [`crate::Handle::current`] on the first publish.

extern crate std;

use core::future::Future;
use core::pin::pin;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::{Context, Poll, Waker};
use std::sync::Arc;
use std::task::Wake;

use rusty_fork::rusty_fork_test;

use crate::Handle;
use crate::tests_fixture::default_config;

/// A waker that records that it was woken.
struct Woken(AtomicBool);

impl Wake for Woken {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::Relaxed);
    }
}

rusty_fork_test! {
    /// A reader that starts before the config task waits, is woken by the
    /// first publish, and then gets the config that the task published.
    #[test]
    fn current_waits_for_the_first_publish() {
        let (runner, handle) = Handle::take();
        let woken = Arc::new(Woken(AtomicBool::new(false)));
        let waker = Waker::from(woken.clone());
        let mut context = Context::from_waker(&waker);
        let mut current = pin!(handle.current());
        assert!(current.as_mut().poll(&mut context).is_pending());
        assert!(current.as_mut().poll(&mut context).is_pending());
        assert!(!woken.0.load(Ordering::Relaxed));

        let mut published = default_config();
        published.thermal.sample_interval_ms += 1;
        runner.publish(published.clone());
        assert!(woken.0.load(Ordering::Relaxed));
        assert_eq!(current.as_mut().poll(&mut context), Poll::Ready(published));
    }
}
