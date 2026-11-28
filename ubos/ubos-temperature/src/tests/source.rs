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

use embassy_futures::block_on;
use rusty_fork::rusty_fork_test;
use ubos_core::units::Temperature;

use super::*;
use crate::TempSource;

/// A source that submits one reading of `source` on each sample.
struct One {
    readings: Handle,
    source: TempSource,
}

impl ReadingSource for One {
    async fn sample(&mut self) {
        let value = Temperature::from_celsius(40);
        let source = self.source;
        self.readings.submit(Reading { source, value }).await;
    }
}

// The test takes the one handle of the readings, so it runs in a process
// of its own.
rusty_fork_test! {
    #[test]
    fn a_pair_submits_the_first_source_then_the_second_before_the_end() {
        let (mut runner, mut receiver, readings) = Handle::take();
        let first = One { readings, source: TempSource::Mcu };
        let second = One { readings, source: TempSource::Ambient };
        let mut pair = (first, second);
        block_on(pair.sample());
        block_on(runner.end_sample());
        let sources = [(); 2].map(|()| match block_on(receiver.receive()) {
            Sampled::Reading(reading) => reading.source,
            Sampled::End => panic!("the sample ended before its readings"),
        });
        assert_eq!(sources, [TempSource::Mcu, TempSource::Ambient]);
        assert_eq!(block_on(receiver.receive()), Sampled::End);
    }
}
