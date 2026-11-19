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

//! The HTTP statuses that the firmware answers with, alone or with a
//! message for the client.

/// An HTTP status: the code and the reason phrase of a status line.
///
/// The fields are private and every status is a constant here, so a
/// response cannot pair a code with the reason of another status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    code: u16,
    reason: &'static str,
}

impl Status {
    pub const OK: Self = Self::new(200, "OK");
    pub const FOUND: Self = Self::new(302, "Found");
    pub const BAD_REQUEST: Self = Self::new(400, "Bad Request");
    pub const UNAUTHORIZED: Self = Self::new(401, "Unauthorized");
    pub const FORBIDDEN: Self = Self::new(403, "Forbidden");
    pub const NOT_FOUND: Self = Self::new(404, "Not Found");
    pub const METHOD_NOT_ALLOWED: Self = Self::new(405, "Method Not Allowed");
    pub const CONFLICT: Self = Self::new(409, "Conflict");
    pub const LENGTH_REQUIRED: Self = Self::new(411, "Length Required");
    pub const PAYLOAD_TOO_LARGE: Self = Self::new(413, "Payload Too Large");
    pub const INTERNAL_SERVER_ERROR: Self = Self::new(500, "Internal Server Error");
    pub const NOT_IMPLEMENTED: Self = Self::new(501, "Not Implemented");
    pub const SERVICE_UNAVAILABLE: Self = Self::new(503, "Service Unavailable");

    const fn new(code: u16, reason: &'static str) -> Self {
        Self { code, reason }
    }

    /// The numeric code of the status line.
    #[must_use]
    pub const fn code(self) -> u16 {
        self.code
    }

    /// The reason phrase of the status line.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        self.reason
    }

    /// This status with `message`, the text the client reads in the body.
    #[must_use]
    pub const fn with_message(self, message: &'static str) -> StatusMessage {
        StatusMessage {
            status: self,
            message,
        }
    }
}

/// A status and the message that explains it to the client, which
/// [`crate::write_message`] writes as the body `{"message":"..."}`.
///
/// The fields are private and [`Status::with_message`] is the only
/// constructor, so a message never travels without its status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatusMessage {
    status: Status,
    message: &'static str,
}

impl StatusMessage {
    /// The status of the response.
    #[must_use]
    pub const fn status(self) -> Status {
        self.status
    }

    /// The text of the message.
    #[must_use]
    pub const fn message(self) -> &'static str {
        self.message
    }
}
