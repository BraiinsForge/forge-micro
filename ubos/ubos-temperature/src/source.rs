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

//! The sources of temperature readings and the task that samples them.
//!
//! A source implements [`ReadingSource`]: one [`sample`](ReadingSource::sample)
//! reads once and submits every reading it took through a [`Handle`], and a
//! read that fails submits nothing. The task of [`run`] samples the source
//! at its start and on each sample interval of the config, and ends each
//! sample with [`Sampled::End`]. `ubos-thermal` takes both from the one
//! [`Receiver`]. This crate decides nothing about a reading, so a board
//! with nothing to measure submits nothing.

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant, Timer};
use ubos_config::{Config, Handle as ConfigHandle};
use ubos_core::take_once::TakeOnce;
use ubos_logging::{info, warn};

use crate::Reading;

#[cfg(test)]
#[path = "tests/source.rs"]
mod tests;

/// Something that takes temperature readings. The implementor bounds its
/// own wait, because nothing else ends a read that never answers, and the
/// sources after it would never be sampled.
#[allow(async_fn_in_trait)]
pub trait ReadingSource {
    async fn sample(&mut self);
}

/// A pair of sources is a source. It samples the first and then the
/// second, so a board names first the source that the halt needs.
impl<A: ReadingSource, B: ReadingSource> ReadingSource for (A, B) {
    async fn sample(&mut self) {
        self.0.sample().await;
        self.1.sample().await;
    }
}

static TAKEN: TakeOnce = TakeOnce::new("temperature");

/// Depth of the channel of readings. It holds what the sources submit
/// between two turns of the consumer.
const READINGS_DEPTH: usize = 64;

/// What the consumer takes from the channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sampled {
    /// One reading that a source submitted.
    Reading(Reading),
    /// The sources were sampled once. The readings of that sample came
    /// before it, because the task that samples sends both in order.
    End,
}

static READINGS: Channel<CriticalSectionRawMutex, Sampled, READINGS_DEPTH> = Channel::new();

/// The way for a source to submit its readings. A firmware takes it once
/// and gives a copy to each source.
#[derive(Clone, Copy)]
pub struct Handle(());

/// The right to take what the sources submitted.
///
/// It is not `Copy`, so the channel has one consumer.
pub struct Receiver(());

impl Receiver {
    /// The next reading, or the end of the sample that took the readings
    /// before it.
    pub async fn receive(&mut self) -> Sampled {
        READINGS.receive().await
    }
}

/// The right to run the task that samples the sources.
///
/// It is not `Copy`, so a firmware that gives it to one task cannot give
/// it to a second.
pub struct Runner(());

impl Runner {
    /// Tell the consumer that the sources were sampled once. Waits while
    /// the channel is full.
    pub async fn end_sample(&mut self) {
        READINGS.send(Sampled::End).await;
    }
}

impl Handle {
    /// The firmware's one handle, the runner of the sampling task and the
    /// receiver of the consumer. A second call panics.
    #[must_use]
    #[track_caller]
    pub fn take() -> (Runner, Receiver, Self) {
        TAKEN.take();
        (Runner(()), Receiver(()), Self(()))
    }

    /// Hand one reading to the consumer. Waits while the channel is full.
    pub async fn submit(&self, reading: Reading) {
        READINGS.send(Sampled::Reading(reading)).await;
    }
}

fn sample_interval(config: &Config) -> Duration {
    Duration::from_millis(u64::from(config.thermal.sample_interval_ms))
}

/// Sample `source` at the start, on each sample interval of the config
/// and after each change of the interval, end each sample for the
/// consumer, and never return. `runner` is borrowed mutably, so one runner
/// runs one task at a time.
pub async fn run<S: ReadingSource>(
    mut source: S,
    runner: &mut Runner,
    config_handle: ConfigHandle,
) -> ! {
    let mut subscription = config_handle.subscribe();
    let mut interval = sample_interval(&config_handle.current().await);
    let mut deadline = Instant::now() + interval;
    loop {
        source.sample().await;
        runner.end_sample().await;
        // A slow sample or a late wake leaves the deadline behind. Samples
        // with no wait between would rotate the window of the consumer,
        // which counts samples, in less time than the samples stand for.
        let now = Instant::now();
        if deadline <= now {
            warn!(
                "temperature: sample ended {} ms after the next was due",
                (now - deadline).as_millis()
            );
            deadline = now + interval;
        }
        loop {
            match select(Timer::at(deadline), subscription.changed()).await {
                Either::First(()) => {
                    deadline += interval;
                    break;
                }
                Either::Second(config) => {
                    let changed = sample_interval(&config);
                    if changed != interval {
                        interval = changed;
                        info!(
                            "temperature: sample interval updated to {} ms",
                            interval.as_millis()
                        );
                        deadline = Instant::now() + interval;
                        break;
                    }
                }
            }
        }
    }
}

/// Task shell around [`run`], over the one source type that holds every
/// sensor of the board.
#[macro_export]
macro_rules! temperature_task {
    ($source:ty) => {
        #[embassy_executor::task]
        async fn temperature_task(
            source: $source,
            mut runner: ubos_temperature::Runner,
            config_handle: ubos_config::Handle,
        ) -> ! {
            ubos_temperature::run(source, &mut runner, config_handle).await
        }
    };
}
