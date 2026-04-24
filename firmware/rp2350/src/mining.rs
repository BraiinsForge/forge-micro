// Stratum V1 load-test task. Connects to a hardcoded pool, completes the
// handshake, saves notify snapshots, and sends one fake submit per notify.

use embedded_io_async::Write;

use defmt::{info, warn};
use embassy_net::tcp::TcpSocket;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::Timer;
use static_cell::ConstStaticCell;
use stratum_v1_core::{
    encode_authorize, encode_configure, encode_submit, encode_subscribe, parse_configure_result,
    parse_incoming, parse_subscribe_result, Authorize, Configure, LineBuffer, Notification, Rpc,
    Submit, Subscribe,
};

const LINE_BUFFER_BYTES: usize = 4096;
const TCP_BUFFER_SIZE: usize = 2048;

const CONFIGURE_ID: u32 = 1;
const SUBSCRIBE_ID: u32 = 2;
const AUTHORIZE_ID: u32 = 3;
const SUBMIT_ID_START: u32 = 4;

const CONFIGURE_MAP: &[u8] = br#"{"version-rolling":{"mask":"1fffe000","min-bit-count":2}}"#;

const MAX_EXTRANONCE2_SIZE: usize = 16;
const MAX_SNAPSHOTS: usize = 32;
const MAX_JOB_ID_CHARS: usize = 32;

// Up to 8 extranonce1 bytes → 16 hex chars; stored verbatim from the pool
// response so we can hand it straight back to the Submit encoder.
const MAX_EXTRANONCE1_HEX_CHARS: usize = 16;

// One encode-buffer shared with all outgoing messages; 512 bytes covers the
// largest stratum request by a comfortable margin.
const ENCODE_BUFFER_SIZE: usize = 512;

// Temporary buffer for copying result bytes before consuming a line.
const RESULT_COPY_SIZE: usize = 256;

// Socket timeout covers both the connect and per-read waits. Generous because
// a quiet pool with no fresh work may legitimately go 60s+ between notifies.
const SOCKET_TIMEOUT_SECS: u64 = 120;

// Fields are written by the load-test and kept for future read-back via HTTP or UART.
#[allow(dead_code)]
pub struct JobSnapshot {
    pub arrived_ms: u64,
    pub job_id: heapless::String<MAX_JOB_ID_CHARS>,
    pub clean_jobs: bool,
    pub coinbase_1_len: u16,
    pub coinbase_2_len: u16,
    pub merkle_depth: u8,
}

struct LoadTestState {
    // Hex string passed through verbatim; no decode because Submit encodes it
    // directly without a work-assembly step.
    extranonce1_hex: heapless::String<MAX_EXTRANONCE1_HEX_CHARS>,
    extranonce2_size: u8,
    version_mask: u32,
    share_difficulty: f64,
    snapshots: heapless::Deque<JobSnapshot, MAX_SNAPSHOTS>,
}

impl LoadTestState {
    const fn new() -> Self {
        Self {
            extranonce1_hex: heapless::String::new(),
            extranonce2_size: 0,
            version_mask: 0,
            share_difficulty: 0.0,
            snapshots: heapless::Deque::new(),
        }
    }
}

static STATE: Mutex<CriticalSectionRawMutex, LoadTestState> = Mutex::new(LoadTestState::new());

static TCP_RX: ConstStaticCell<[u8; TCP_BUFFER_SIZE]> = ConstStaticCell::new([0; TCP_BUFFER_SIZE]);
static TCP_TX: ConstStaticCell<[u8; TCP_BUFFER_SIZE]> = ConstStaticCell::new([0; TCP_BUFFER_SIZE]);

#[derive(defmt::Format, Debug)]
enum SessionError {
    Connect,
    SocketRead,
    SocketWrite,
    EncodeOverflow,
    LineTooLong,
    HandshakeMissing,
    ExtranonceTooLong,
}

#[embassy_executor::task]
pub async fn stratum_task(stack: embassy_net::Stack<'static>) -> ! {
    // ConstStaticCell::take() is one-shot; take the TCP buffers once and
    // reuse them across reconnects by re-borrowing on each session.
    let rx_buffer = TCP_RX.take();
    let tx_buffer = TCP_TX.take();

    loop {
        if let Err(error) = run_session(stack, rx_buffer, tx_buffer).await {
            warn!("stratum session: {}", error);
        }
        STATE.lock().await.snapshots.clear();
        Timer::after(crate::config::Pool::RECONNECT_DELAY).await;
    }
}

async fn run_session(
    stack: embassy_net::Stack<'static>,
    rx_buffer: &mut [u8; TCP_BUFFER_SIZE],
    tx_buffer: &mut [u8; TCP_BUFFER_SIZE],
) -> Result<(), SessionError> {
    let [a, b, c, d] = crate::config::Pool::HOST_IP;
    let address = embassy_net::IpAddress::v4(a, b, c, d);

    let mut socket = TcpSocket::new(stack, rx_buffer, tx_buffer);
    socket.set_timeout(Some(embassy_time::Duration::from_secs(SOCKET_TIMEOUT_SECS)));

    socket
        .connect((address, crate::config::Pool::PORT))
        .await
        .map_err(|_| SessionError::Connect)?;

    info!(
        "stratum: connected to {}.{}.{}.{}:{}",
        a,
        b,
        c,
        d,
        crate::config::Pool::PORT
    );

    let mut line_buffer = LineBuffer::<LINE_BUFFER_BYTES>::new();
    let mut encode_buffer = [0u8; ENCODE_BUFFER_SIZE];

    do_configure(&mut socket, &mut line_buffer, &mut encode_buffer).await?;
    do_subscribe(&mut socket, &mut line_buffer, &mut encode_buffer).await?;
    do_authorize(&mut socket, &mut line_buffer, &mut encode_buffer).await?;

    info!("stratum: handshake complete, entering notify loop");

    let mut submit_id: u32 = SUBMIT_ID_START;

    loop {
        fill_line_buffer(&mut socket, &mut line_buffer).await?;
        loop {
            let next = line_buffer
                .next_line()
                .map_err(|_| SessionError::LineTooLong)?;
            let Some(line) = next else { break };
            // Classify before consuming; we cannot hold `line` across consume().
            let action = classify_line(line);
            line_buffer.consume();
            dispatch_action(action, &mut socket, &mut encode_buffer, &mut submit_id).await?;
        }
    }
}

async fn fill_line_buffer(
    socket: &mut TcpSocket<'_>,
    line_buffer: &mut LineBuffer<LINE_BUFFER_BYTES>,
) -> Result<(), SessionError> {
    let writable = line_buffer.writable();
    if writable.is_empty() {
        return Err(SessionError::LineTooLong);
    }
    let read_count = socket
        .read(writable)
        .await
        .map_err(|_| SessionError::SocketRead)?;
    if read_count == 0 {
        return Err(SessionError::SocketRead);
    }
    info!("stratum: read {} bytes", read_count);
    line_buffer.advance(read_count);
    Ok(())
}

async fn dispatch_action(
    action: LineAction,
    socket: &mut TcpSocket<'_>,
    encode_buffer: &mut [u8; ENCODE_BUFFER_SIZE],
    submit_id: &mut u32,
) -> Result<(), SessionError> {
    match action {
        LineAction::Notify {
            job_id,
            job_id_len,
            clean_jobs,
            coinbase_1_len,
            coinbase_2_len,
            merkle_depth,
            version,
            version_len,
            time,
            time_len,
        } => {
            info!(
                "stratum: notify job_id={=[u8]:a} clean={} cb1={} cb2={} merkle={}",
                &job_id[..job_id_len],
                clean_jobs,
                coinbase_1_len,
                coinbase_2_len,
                merkle_depth
            );
            let snapshot = make_snapshot(
                job_id,
                job_id_len,
                clean_jobs,
                coinbase_1_len,
                coinbase_2_len,
                merkle_depth,
            );
            {
                let mut state = STATE.lock().await;
                if state.snapshots.is_full() {
                    state.snapshots.pop_front();
                }
                // push_back cannot fail: we just ensured space above.
                let _ = state.snapshots.push_back(snapshot);
            }
            send_fake_submit(
                socket,
                encode_buffer,
                &job_id[..job_id_len],
                &version[..version_len],
                &time[..time_len],
                *submit_id,
            )
            .await?;
            info!("stratum: submit sent id={}", *submit_id);
            // Wrap submit_id so it never collides with handshake ids.
            *submit_id = submit_id.wrapping_add(1).max(SUBMIT_ID_START);
        }
        LineAction::SetDifficulty(difficulty) => {
            STATE.lock().await.share_difficulty = difficulty;
            info!("stratum: set_difficulty {}", difficulty);
        }
        LineAction::SetVersionMask(version_mask) => {
            STATE.lock().await.version_mask = version_mask;
            info!("stratum: set_version_mask {:#010x}", version_mask);
        }
        LineAction::OtherNotification => {
            info!("stratum: other notification");
        }
        LineAction::Response { is_error, stratum_error } => {
            if is_error {
                warn!("stratum: pool returned an error response: {}", stratum_error.as_str());
            }
        }
        LineAction::ParseError => {
            warn!("stratum: parse error, skipping line");
        }
    }
    Ok(())
}

// Carries just enough copied data to act on a line after line_buffer.consume().
enum LineAction {
    Notify {
        job_id: [u8; MAX_JOB_ID_CHARS],
        job_id_len: usize,
        clean_jobs: bool,
        coinbase_1_len: u16,
        coinbase_2_len: u16,
        merkle_depth: u8,
        version: [u8; 8],
        version_len: usize,
        time: [u8; 8],
        time_len: usize,
    },
    SetDifficulty(f64),
    SetVersionMask(u32),
    OtherNotification,
    Response {
        is_error: bool,
        stratum_error: heapless::String<128>,
    },
    ParseError,
}

fn classify_line(line: &[u8]) -> LineAction {
    let rpc = match parse_incoming(line) {
        Ok(rpc) => rpc,
        Err(error) => {
            // Log first ~128 bytes of the offending line so we can see what
            // method/shape the pool sent that we don't yet handle.
            let snippet = &line[..line.len().min(128)];
            warn!(
                "stratum: parse error {} on line ({=[u8]:a}) len={}",
                defmt::Debug2Format(&error),
                snippet,
                line.len()
            );
            return LineAction::ParseError;
        }
    };
    match rpc {
        Rpc::Notification(Notification::Notify(notify)) => {
            let job_id_src = notify.job_id.as_bytes();
            let job_id_len = job_id_src.len().min(MAX_JOB_ID_CHARS);
            let mut job_id = [0u8; MAX_JOB_ID_CHARS];
            job_id[..job_id_len].copy_from_slice(&job_id_src[..job_id_len]);

            let version_src = notify.version.as_bytes();
            let version_len = version_src.len().min(8);
            let mut version = [0u8; 8];
            version[..version_len].copy_from_slice(&version_src[..version_len]);

            let time_src = notify.time.as_bytes();
            let time_len = time_src.len().min(8);
            let mut time = [0u8; 8];
            time[..time_len].copy_from_slice(&time_src[..time_len]);

            #[allow(clippy::cast_possible_truncation)]
            LineAction::Notify {
                job_id,
                job_id_len,
                clean_jobs: notify.clean_jobs,
                coinbase_1_len: notify.coin_base_1.len() as u16,
                coinbase_2_len: notify.coin_base_2.len() as u16,
                merkle_depth: notify.merkle_branch.len() as u8,
                version,
                version_len,
                time,
                time_len,
            }
        }
        Rpc::Notification(Notification::SetDifficulty(difficulty)) => {
            LineAction::SetDifficulty(difficulty.0)
        }
        Rpc::Notification(Notification::SetVersionMask(mask)) => {
            LineAction::SetVersionMask(mask.mask)
        }
        Rpc::Notification(_) => LineAction::OtherNotification,
        Rpc::Response(response) => {
            let mut error_str = heapless::String::<128>::new();
            if let Some(err) = &response.stratum_error {
                use core::fmt::Write;
                let _ = write!(error_str, "{:#?}", err);
            }

            LineAction::Response {
                is_error: response.stratum_error.is_some(),
                stratum_error: error_str,
            }
        }
    }
}

fn make_snapshot(
    job_id: [u8; MAX_JOB_ID_CHARS],
    job_id_len: usize,
    clean_jobs: bool,
    coinbase_1_len: u16,
    coinbase_2_len: u16,
    merkle_depth: u8,
) -> JobSnapshot {
    let mut job_id_str: heapless::String<MAX_JOB_ID_CHARS> = heapless::String::new();
    let ascii = core::str::from_utf8(&job_id[..job_id_len]).unwrap_or("");
    let _ = job_id_str.push_str(ascii);
    JobSnapshot {
        arrived_ms: embassy_time::Instant::now().as_millis(),
        job_id: job_id_str,
        clean_jobs,
        coinbase_1_len,
        coinbase_2_len,
        merkle_depth,
    }
}

async fn send_fake_submit(
    socket: &mut TcpSocket<'_>,
    encode_buffer: &mut [u8; ENCODE_BUFFER_SIZE],
    job_id_bytes: &[u8],
    version_bytes: &[u8],
    time_bytes: &[u8],
    submit_id: u32,
) -> Result<(), SessionError> {
    let extranonce2_size = STATE.lock().await.extranonce2_size;
    let hex_len = usize::from(extranonce2_size) * 2;
    if hex_len > MAX_EXTRANONCE2_SIZE * 2 {
        return Err(SessionError::ExtranonceTooLong);
    }

    // Build extranonce2 as a run of ASCII '0' digits on the stack.
    let extranonce2_buffer = [b'0'; MAX_EXTRANONCE2_SIZE * 2];
    let extranonce2 = core::str::from_utf8(&extranonce2_buffer[..hex_len])
        .expect("BUG: ASCII '0' is valid UTF-8");

    let job_id = core::str::from_utf8(job_id_bytes).unwrap_or("");
    let version = core::str::from_utf8(version_bytes).unwrap_or("00000000");
    let time = core::str::from_utf8(time_bytes).unwrap_or("00000000");

    let submit = Submit {
        user_name: crate::config::Pool::USER,
        job_id,
        extra_nonce_2: extranonce2,
        time,
        nonce: "00000000",
        version,
    };

    let written = encode_submit(&mut encode_buffer[..], submit_id, &submit)
        .map_err(|_| SessionError::EncodeOverflow)?;

    socket
        .write_all(&encode_buffer[..written])
        .await
        .map_err(|_| SessionError::SocketWrite)?;

    info!("stratum: submitted fake share for job {}", job_id);

    Ok(())
}

// Reads from the socket until we see a response with the expected id.
// Returns the result bytes copied into a stack buffer so the line can be consumed.
// Returns None when the matched response carried no result field.
async fn read_response(
    socket: &mut TcpSocket<'_>,
    line_buffer: &mut LineBuffer<LINE_BUFFER_BYTES>,
    expected_id: u32,
) -> Result<Option<[u8; RESULT_COPY_SIZE]>, SessionError> {
    let mut result_buffer = [0u8; RESULT_COPY_SIZE];
    loop {
        fill_line_buffer(socket, line_buffer).await?;
        loop {
            let next = line_buffer
                .next_line()
                .map_err(|_| SessionError::LineTooLong)?;
            let Some(line) = next else { break };

            // inspect_handshake_line copies result bytes into result_buffer
            // before we call consume(), so no borrow of `line` escapes.
            let found = inspect_handshake_line(line, expected_id, &mut result_buffer);
            line_buffer.consume();

            match found {
                HandshakeLine::Skip => {}
                HandshakeLine::FoundWithResult(len) => {
                    let mut out = [0u8; RESULT_COPY_SIZE];
                    out[..len].copy_from_slice(&result_buffer[..len]);
                    return Ok(Some(out));
                }
                HandshakeLine::FoundNoResult => return Ok(None),
            }
        }
    }
}

enum HandshakeLine {
    Skip,
    FoundWithResult(usize),
    FoundNoResult,
}

// Copies result bytes into result_buffer before returning so no borrow of the
// line survives. Returns the number of bytes copied on success.
fn inspect_handshake_line(
    line: &[u8],
    expected_id: u32,
    result_buffer: &mut [u8; RESULT_COPY_SIZE],
) -> HandshakeLine {
    let Ok(rpc) = parse_incoming(line) else {
        return HandshakeLine::Skip;
    };
    let Rpc::Response(response) = rpc else {
        return HandshakeLine::Skip;
    };
    if response.id != expected_id {
        return HandshakeLine::Skip;
    }
    match response.stratum_result {
        None => HandshakeLine::FoundNoResult,
        Some(result_bytes) => {
            let copy_len = result_bytes.len().min(RESULT_COPY_SIZE);
            result_buffer[..copy_len].copy_from_slice(&result_bytes[..copy_len]);
            HandshakeLine::FoundWithResult(copy_len)
        }
    }
}

async fn do_configure(
    socket: &mut TcpSocket<'_>,
    line_buffer: &mut LineBuffer<LINE_BUFFER_BYTES>,
    encode_buffer: &mut [u8; ENCODE_BUFFER_SIZE],
) -> Result<(), SessionError> {
    let configure = Configure {
        features: &["version-rolling"],
        configure_map: CONFIGURE_MAP,
    };
    let written = encode_configure(&mut encode_buffer[..], CONFIGURE_ID, &configure)
        .map_err(|_| SessionError::EncodeOverflow)?;
    socket
        .write_all(&encode_buffer[..written])
        .await
        .map_err(|_| SessionError::SocketWrite)?;

    let result_opt = read_response(socket, line_buffer, CONFIGURE_ID).await?;
    if let Some(result_buffer) = result_opt {
        if let Ok(configure_result) = parse_configure_result(&result_buffer) {
            info!(
                "stratum: configure result received (len {})",
                configure_result.0.len()
            );
        }
    }

    Ok(())
}

async fn do_subscribe(
    socket: &mut TcpSocket<'_>,
    line_buffer: &mut LineBuffer<LINE_BUFFER_BYTES>,
    encode_buffer: &mut [u8; ENCODE_BUFFER_SIZE],
) -> Result<(), SessionError> {
    let subscribe = Subscribe {
        agent_signature: Some("rusty-boards-load-test/0.1"),
        extra_nonce_1: None,
    };
    let written = encode_subscribe(&mut encode_buffer[..], SUBSCRIBE_ID, &subscribe)
        .map_err(|_| SessionError::EncodeOverflow)?;
    socket
        .write_all(&encode_buffer[..written])
        .await
        .map_err(|_| SessionError::SocketWrite)?;

    let result_opt = read_response(socket, line_buffer, SUBSCRIBE_ID).await?;
    let result_buffer = result_opt.ok_or(SessionError::HandshakeMissing)?;

    let subscribe_result =
        parse_subscribe_result(&result_buffer).map_err(|_| SessionError::HandshakeMissing)?;

    {
        let mut state = STATE.lock().await;
        state.extranonce1_hex.clear();
        let truncated_len = subscribe_result.1.len().min(MAX_EXTRANONCE1_HEX_CHARS);
        // Truncate silently if extranonce1 exceeds the storage capacity.
        let _ = state
            .extranonce1_hex
            .push_str(&subscribe_result.1[..truncated_len]);
        state.extranonce2_size = subscribe_result.2;
    }

    info!(
        "stratum: subscribe ok extranonce2_size={}",
        subscribe_result.2
    );

    Ok(())
}

async fn do_authorize(
    socket: &mut TcpSocket<'_>,
    line_buffer: &mut LineBuffer<LINE_BUFFER_BYTES>,
    encode_buffer: &mut [u8; ENCODE_BUFFER_SIZE],
) -> Result<(), SessionError> {
    let authorize = Authorize {
        name: crate::config::Pool::USER,
        password: crate::config::Pool::PASS,
    };
    let written = encode_authorize(&mut encode_buffer[..], AUTHORIZE_ID, &authorize)
        .map_err(|_| SessionError::EncodeOverflow)?;
    socket
        .write_all(&encode_buffer[..written])
        .await
        .map_err(|_| SessionError::SocketWrite)?;

    let _response = read_response(socket, line_buffer, AUTHORIZE_ID).await?;

    info!("stratum: authorize ok");

    Ok(())
}
