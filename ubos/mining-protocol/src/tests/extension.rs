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

//! Tests of the extension-frame defaults on the two seam traits.

use super::*;

use crate::{
    Error, MiningClient, MiningClientVisitor, MiningJob, SessionUpdate, Share, SubmitResult,
};
use embedded_io_async::{Read, Write};

/// An adapter that implements only what the trait requires.
struct Stub;

impl MiningClient for Stub {
    async fn connect<T>(&mut self, _transport: &mut T) -> Result<(), Error>
    where
        T: Read + Write,
    {
        Ok(())
    }

    fn submit(&mut self, _share: &Share<'_>) -> Result<(), Error> {
        Ok(())
    }

    async fn drive<T>(&mut self, _transport: &mut T) -> Result<(), Error>
    where
        T: Read + Write,
    {
        Ok(())
    }

    fn accept<V: MiningClientVisitor>(&mut self, _visitor: &mut V) {}
}

/// A visitor that implements only the three mining methods and counts
/// nothing else.
struct Count(usize);

impl MiningClientVisitor for Count {
    fn visit_new_job(&mut self, _job: MiningJob<'_>) {}

    fn visit_submit_result(&mut self, _result: SubmitResult<'_>) {}

    fn visit_session_update(&mut self, _update: SessionUpdate<'_>) {}
}

#[test]
fn an_adapter_carries_no_extension_frames_unless_it_says_so() {
    let frame = ExtensionFrame {
        is_channel_message: false,
        extension_type: 0x1234,
        msg_type: 7,
        payload: b"hello",
    };
    assert_eq!(Stub.send_extension(&frame), Err(Error::NotImplemented));
}

#[test]
fn a_visitor_ignores_extension_frames_unless_it_says_so() {
    let mut count = Count(0);
    count.visit_extension_frame(ExtensionFrame {
        is_channel_message: false,
        extension_type: 0x1234,
        msg_type: 7,
        payload: b"",
    });
    assert_eq!(count.0, 0);
}
