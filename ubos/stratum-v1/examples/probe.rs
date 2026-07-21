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

// Test: connect to Braiins Pool, use Stratum V1
// handshake, print incoming RPC,
// submit one fake share to the first job.

use std::io::{Read, Write};
use std::net::TcpStream;

use anyhow::{Result, anyhow};
use stratum_v1::{
    Authorize, LineBuffer, Notification, Notify, Response, Rpc, Submit, Subscribe,
    encode_authorize, encode_submit, encode_subscribe, parse_incoming, parse_subscribe_result,
};

// Use braiins pool
const POOL_ADDR: &str = "stratum.braiins.com:3333";
const USER: &str = "braiinstest.dumb_worker";
const PASS: &str = "x";

const SUBSCRIBE_ID: u32 = 1;
const AUTHORIZE_ID: u32 = 2;
const SUBMIT_ID: u32 = 3;

const MAX_EN2_HEX: usize = 32;

fn main() -> Result<()> {
    let mut stream = TcpStream::connect(POOL_ADDR)?;
    println!("connected {POOL_ADDR}");

    let mut out = [0u8; 512];
    send_subscribe(&mut stream, &mut out)?;
    send_authorize(&mut stream, &mut out)?;

    let mut lb = LineBuffer::<4096>::new();
    let mut extra_nonce_1 = String::new();
    let mut extra_nonce_2_size: u8 = 0;
    let mut submitted = false;

    loop {
        let writable = lb.writable();
        if writable.is_empty() {
            return Err(anyhow!("line too long for 4096-byte buffer"));
        }
        let read = stream.read(writable)?;
        if read == 0 {
            println!("peer closed");
            return Ok(());
        }
        lb.advance(read);

        loop {
            let next = lb
                .next_line()
                .map_err(|error| anyhow!("framing: {error:?}"))?;
            let Some(line) = next else { break };
            println!("< {}", core::str::from_utf8(line).unwrap_or("<non-utf8>"));
            handle_line(
                line,
                &mut stream,
                &mut out,
                &mut extra_nonce_1,
                &mut extra_nonce_2_size,
                &mut submitted,
            )?;
            lb.consume();
        }
    }
}

fn send_line(stream: &mut TcpStream, out: &[u8]) -> Result<()> {
    stream.write_all(out)?;
    // Encoded messages always terminate with '\n'; strip it for display.
    let printable = out.strip_suffix(b"\n").unwrap_or(out);
    println!(
        "> {}",
        core::str::from_utf8(printable).unwrap_or("<non-utf8>")
    );
    Ok(())
}

fn handle_line(
    line: &[u8],
    stream: &mut TcpStream,
    out: &mut [u8],
    extra_nonce_1: &mut String,
    extra_nonce_2_size: &mut u8,
    submitted: &mut bool,
) -> Result<()> {
    let rpc = match parse_incoming(line) {
        Ok(rpc) => rpc,
        Err(error) => {
            println!("! parse error: {error:?}");
            return Ok(());
        }
    };

    match rpc {
        Rpc::Response(response) => handle_response(&response, extra_nonce_1, extra_nonce_2_size),
        Rpc::Notification(Notification::Notify(notify)) => {
            if !*submitted && *extra_nonce_2_size > 0 {
                send_submit(stream, out, &notify, *extra_nonce_2_size)?;
                *submitted = true;
            }
            Ok(())
        }
        Rpc::Notification(
            Notification::SetDifficulty(_)
            | Notification::SetVersionMask(_)
            | Notification::SetExtranonce(_)
            | Notification::Ping { .. }
            | Notification::ClientReconnect(_),
        ) => Ok(()),
    }
}

fn handle_response(
    response: &Response<'_>,
    extra_nonce_1: &mut String,
    extra_nonce_2_size: &mut u8,
) -> Result<()> {
    if response.stratum_error.is_some() {
        return Ok(());
    }
    let Some(result) = response.stratum_result else {
        return Ok(());
    };
    if response.id != SUBSCRIBE_ID {
        return Ok(());
    }
    let parsed = parse_subscribe_result(result)
        .map_err(|error| anyhow!("parse subscribe result: {error:?}"))?;
    extra_nonce_1.clear();
    extra_nonce_1.push_str(parsed.1);
    *extra_nonce_2_size = parsed.2;
    Ok(())
}

fn send_subscribe(stream: &mut TcpStream, out: &mut [u8]) -> Result<()> {
    let message = Subscribe {
        agent_signature: Some("stratum-v1-probe/0.1"),
        extra_nonce_1: None,
    };
    let written = encode_subscribe(out, SUBSCRIBE_ID, &message)
        .map_err(|error| anyhow!("encode subscribe: {error:?}"))?;
    send_line(stream, &out[..written])
}

fn send_authorize(stream: &mut TcpStream, out: &mut [u8]) -> Result<()> {
    let message = Authorize {
        name: USER,
        password: PASS,
    };
    let written = encode_authorize(out, AUTHORIZE_ID, &message)
        .map_err(|error| anyhow!("encode authorize: {error:?}"))?;
    send_line(stream, &out[..written])
}

fn send_submit(
    stream: &mut TcpStream,
    out: &mut [u8],
    notify: &Notify<'_>,
    extra_nonce_2_size: u8,
) -> Result<()> {
    let hex_len = 2 * usize::from(extra_nonce_2_size);
    if hex_len > MAX_EN2_HEX {
        return Err(anyhow!(
            "extra_nonce_2_size {extra_nonce_2_size} exceeds probe limit"
        ));
    }
    let zeros = [b'0'; MAX_EN2_HEX];
    // Every byte is the ASCII digit '0'
    let extra_nonce_2 =
        core::str::from_utf8(&zeros[..hex_len]).expect("BUG: ASCII '0' bytes are valid UTF-8");

    let message = Submit {
        user_name: USER,
        job_id: notify.job_id,
        extra_nonce_2,
        time: notify.time,
        nonce: "00000000",
        version: notify.version,
    };
    let written = encode_submit(out, SUBMIT_ID, &message)
        .map_err(|error| anyhow!("encode submit: {error:?}"))?;
    send_line(stream, &out[..written])
}
