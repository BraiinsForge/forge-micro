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

//! Tests for the `/api/info` body and the `/api/info` and `/api/config`
//! body-buffer sizes.

// The crate is `#![no_std]`; explicitly opt back into std for the
// test harness so we can lock a mutex and allocate format strings.
extern crate std;

use core::fmt::Write as _;

use ubos_core::units::{
    Efficiency, FanPwm, FanRpm, Frequency, Hashrate, Power, Temperature, Voltage,
};

use crate::ota::{MAX_BUILD_ID_LEN, OtaPhase, OtaWritePhase, ResetReason};
use crate::rest::{API_INFO_BODY_BUFFER_SIZE, CONFIG_RESPONSE_BUFFER_SIZE};
use crate::schema::info::{ApiInfo, ApiInfoChain, ApiInfoOta};
use crate::schema::state::{ApiChainState, ApiMiningState, ApiPauseReason};
use ubos_manager::PauseReason;
use ubos_metrics::chain::ChainPresence;
use ubos_mining::MiningState;

// --- N6: API_INFO_BODY_BUFFER_SIZE covers the worst-case body ---

/// Pinned exactly, so a field addition that still fits the buffer is
/// noticed here.
const WORST_CASE_INFO_BODY_LEN: usize = 1712;

/// A `&'static str` of `len` copies of `text`. [`ApiInfo`] takes
/// `&'static str`, and a leak in a test costs nothing.
fn leaked(text: &str, len: usize) -> &'static str {
    std::boxed::Box::leak(text.repeat(len).into_boxed_str())
}

#[test]
#[allow(clippy::too_many_lines)]
fn api_info_body_fits_in_buffer() {
    let chain_hottest_c = i32::MIN / 1000;
    let chains = (0..ubos_metrics::chain::MAX_CHAINS)
        .map(|chain| ApiInfoChain {
            chain: u8::try_from(chain).expect("MAX_CHAINS fits a byte"),
            state: ApiChainState::Unknown,
            chips: u16::MAX,
            frequency: Frequency::from_khz(u32::MAX),
            hashrate: Hashrate::from_hps(u64::MAX),
            hottest_c: Some(chain_hottest_c),
            pool: None,
        })
        .collect();

    let info = ApiInfo {
        name: leaked("A", 42),
        hashrate: Hashrate::from_hps(u64::MAX),
        uptime: u64::MAX,
        fan_rpm: FanRpm::from_rpm(u32::MAX),
        fan_pwm: FanPwm::FULL,
        power_in: Power::from_mw(u32::MAX),
        power_out: Power::from_mw(u32::MAX),
        psu_voltage_set: Voltage::from_mv(u32::MAX),
        psu_voltage_measured: Voltage::from_mv(u32::MAX),
        chain_voltage: Voltage::from_mv(u32::MAX),
        psu_fault: true,
        pools: u8::MAX,
        net_confirm_seconds: u64::MAX,
        running: true,
        mining_state: Some(ApiMiningState::WaitingForPool),
        paused: true,
        pause_reason: Some(ApiPauseReason::TemperatureUnreadable),
        chains,
        ip: "255.255.255.255".parse().expect("BUG: an IPv4 fits ip"),
        mac: "ff:ff:ff:ff:ff:ff".parse().expect("BUG: a MAC fits mac"),
        time: "23:59:59 UTC".parse().expect("BUG: a UTC time fits time"),
        build: leaked("\"", MAX_BUILD_ID_LEN),
        product_name: Some(
            leaked("\u{1}", crate::HW_STRING_LEN)
                .parse()
                .expect("BUG: fills HwString exactly"),
        ),
        nominal_hashrate: Some(Hashrate::from_hps(u64::MAX)),
        ota: Some(ApiInfoOta {
            running_slot: "INACTIVE",
            running_version: u32::MAX,
            has_previous_image: true,
            previous_version: Some(u32::MAX),
            max_image_bytes: u64::MAX,
            phase: OtaPhase::Rebooting,
            bytes_written: u32::MAX,
            chunks: u32::MAX,
            last_error: u8::MAX,
            reset_reason: ResetReason::WatchdogTimeout,
            reset_ota_active: true,
            reset_ota_offset: u32::MAX,
            reset_write_phase: OtaWritePhase::FlashDone,
        }),
    };

    // The body the dashboard reads, spelled out by hand so that a renamed,
    // reordered or dropped key fails the comparison below and the contract
    // in `ubos-webui/web/lib/types.ts` holds.
    let board_name = "A".repeat(42);
    let build = "\\\"".repeat(MAX_BUILD_ID_LEN);
    let product_name = "\\u0001".repeat(crate::HW_STRING_LEN);
    let max_u64 = u64::MAX;
    let max_u32 = u32::MAX;
    let max_u8 = u8::MAX;
    let mut expected = std::format!(
        "{{\"name\":\"{board_name}\",\
         \"hashrate\":{max_u64},\
         \"uptime\":{max_u64},\
         \"fan_rpm\":{max_u32},\
         \"fan_pwm\":100,\
         \"power_in_mw\":{max_u32},\
         \"power_out_mw\":{max_u32},\
         \"psu_voltage_set_mv\":{max_u32},\
         \"psu_voltage_measured_mv\":{max_u32},\
         \"chain_voltage_mv\":{max_u32},\
         \"psu_fault\":true,\
         \"pools\":{max_u8},\
         \"net_confirm_seconds\":{max_u64},\
         \"running\":true,\
         \"mining_state\":\"waitingForPool\",\
         \"paused\":true,\
         \"pause_reason\":\"temperatureUnreadable\",\
         \"chains\":["
    );
    for chain in 0..ubos_metrics::chain::MAX_CHAINS {
        if chain > 0 {
            expected.push(',');
        }
        let _ = write!(
            expected,
            "{{\"chain\":{chain},\
             \"state\":\"unknown\",\
             \"chips\":{},\
             \"frequency_khz\":{max_u32},\
             \"hashrate\":{max_u64},\
             \"hottest_c\":{chain_hottest_c},\
             \"pool\":null}}",
            u16::MAX,
        );
    }
    let _ = write!(
        expected,
        "],\"ip\":\"255.255.255.255\",\
         \"mac\":\"ff:ff:ff:ff:ff:ff\",\
         \"time\":\"23:59:59 UTC\",\
         \"build\":\"{build}\",\
         \"product_name\":\"{product_name}\",\
         \"nominal_hashrate\":{max_u64},\
         \"ota\":{{\"runningSlot\":\"INACTIVE\",\
         \"runningVersion\":{max_u32},\
         \"hasPreviousImage\":true,\
         \"previousVersion\":{max_u32},\
         \"maxImageBytes\":{max_u64},\
         \"phase\":\"rebooting\",\
         \"bytesWritten\":{max_u32},\
         \"chunks\":{max_u32},\
         \"lastError\":{max_u8},\
         \"resetReason\":\"watchdogTimeout\",\
         \"resetOtaActive\":true,\
         \"resetOtaOffset\":{max_u32},\
         \"resetWritePhase\":\"flashDone\"}}}}"
    );

    let mut buffer = std::vec![0u8; API_INFO_BODY_BUFFER_SIZE * 2];
    let len = serde_json_core::to_slice(&info, &mut buffer).expect("worst case fits the probe");
    let body = std::str::from_utf8(&buffer[..len]).expect("serde_json_core emits UTF-8");
    assert_eq!(
        body, expected,
        "the /api/info body moved; the dashboard reads it by the keys in \
         ubos-webui/web/lib/types.ts"
    );
    assert_eq!(
        len, WORST_CASE_INFO_BODY_LEN,
        "worst-case /api/info body moved; update WORST_CASE_INFO_BODY_LEN"
    );
    assert!(
        len <= API_INFO_BODY_BUFFER_SIZE,
        "worst-case /api/info body is {len} bytes, which overflows \
         API_INFO_BODY_BUFFER_SIZE ({API_INFO_BODY_BUFFER_SIZE})"
    );
}

#[test]
fn api_info_without_ota_leaves_the_key_out() {
    let info = ApiInfo {
        name: "board",
        hashrate: Hashrate::default(),
        uptime: 0,
        fan_rpm: FanRpm::default(),
        fan_pwm: FanPwm::default(),
        power_in: Power::default(),
        power_out: Power::default(),
        psu_voltage_set: Voltage::default(),
        psu_voltage_measured: Voltage::default(),
        chain_voltage: Voltage::default(),
        psu_fault: false,
        pools: 0,
        net_confirm_seconds: 0,
        running: false,
        mining_state: None,
        paused: false,
        pause_reason: None,
        chains: heapless::Vec::new(),
        ip: heapless::String::new(),
        mac: heapless::String::new(),
        time: heapless::String::new(),
        build: "",
        product_name: None,
        nominal_hashrate: None,
        ota: None,
    };
    let mut buffer = [0u8; API_INFO_BODY_BUFFER_SIZE];
    let len = serde_json_core::to_slice(&info, &mut buffer).expect("an empty body fits");
    let body = std::str::from_utf8(&buffer[..len]).expect("serde_json_core emits UTF-8");
    assert!(
        body.ends_with(",\"build\":\"\",\"product_name\":null,\"nominal_hashrate\":null}"),
        "expected the body to end at nominal_hashrate with no ota key: {body}"
    );
}

// --- CONFIG_RESPONSE_BUFFER_SIZE covers the worst-case body ---

/// A `heapless::String` filled to the capacity of the field it is
/// assigned to, so widening a field in `ubos-config` widens this body.
fn filled<const N: usize>() -> heapless::String<N> {
    let mut string = heapless::String::new();
    for _ in 0..N {
        string.push('a').expect("BUG: fill exceeds capacity");
    }
    string
}

/// Pinned exactly, so a field addition that still fits the buffer is
/// noticed here. The probe carries the `fee` entry whatever the build's
/// switch, so the pin holds in both states.
const WORST_CASE_BODY_LEN: usize = 2617;

#[test]
fn config_response_body_fits_in_buffer() {
    // The two variants' maxima are close enough that a capacity change
    // could invert which is wider.
    let widest_protocol = ubos_config::ProtocolKind::Sv1 {
        password: Some(filled()),
    };
    let mut probe = [0u8; 256];
    let sv1 =
        serde_json_core::to_slice(&widest_protocol, &mut probe).expect("a protocol fits the probe");
    let sv2 = serde_json_core::to_slice(
        &ubos_config::ProtocolKind::Sv2 {
            pool_pubkey: Some([u8::MAX; 32]),
        },
        &mut probe,
    )
    .expect("a protocol fits the probe");
    assert!(
        sv1 > sv2,
        "SV2 now serializes at least as wide as SV1 ({sv2} vs {sv1}); the \
         worst-case body no longer uses the widest protocol variant"
    );

    let pool_entry = ubos_config::PoolEntry {
        host: filled(),
        port: u16::MAX,
        user: filled(),
        protocol: widest_protocol,
        weight: u32::MAX,
    };
    let mut pool = heapless::Vec::new();
    for _ in 0..pool.capacity() {
        pool.push(pool_entry.clone())
            .expect("BUG: pool exceeds capacity");
    }

    let mut dns = heapless::Vec::new();
    for _ in 0..dns.capacity() {
        dns.push([255, 255, 255, 255])
            .expect("BUG: dns exceeds capacity");
    }

    let update = ubos_config::ConfigUpdate {
        pool: Some(pool),
        fee: Some(pool_entry),
        fee_permille: Some(ubos_config::MAX_FEE_PERMILLE),
        thermal: Some(ubos_config::ThermalConfig {
            sample_interval_ms: u32::MAX,
            dangerous: ubos_config::TemperatureDescriptor {
                chip: Some(Temperature::from_millicelsius(i32::MIN)),
                hashboard: Some(Temperature::from_millicelsius(i32::MIN)),
            },
            cooldown_temp: Temperature::from_millicelsius(i32::MIN),
        }),
        cooling: Some(ubos_config::CoolingUpdate {
            mode: Some(ubos_config::FanMode::Fixed),
            target: Some(ubos_config::PidTarget {
                source: ubos_config::Source::Hashboard,
                statistic: ubos_config::Statistic::Mean,
                temp: Temperature::from_millicelsius(i32::MIN),
            }),
            fixed_fan_pwm: Some(u8::MAX),
            min_fan_pwm: Some(u8::MAX),
        }),
        miner: Some(ubos_config::MinerConfig {
            tuning: ubos_config::Tuning::Power(ubos_config::PowerTarget {
                input_power: Power::from_mw(u32::MAX),
                efficiency: Efficiency::from_mj_per_th(core::num::NonZeroU32::MAX),
            }),
        }),
        api: Some(ubos_config::ApiConfig {
            username: filled(),
            password: filled(),
        }),
        network: Some(ubos_config::NetworkConfig {
            mode: ubos_config::NetworkMode::Static {
                address: [255, 255, 255, 255],
                prefix_len: u8::MAX,
                gateway: [255, 255, 255, 255],
                dns,
            },
            hostname: filled(),
        }),
    };

    let mut buffer = std::vec![0u8; CONFIG_RESPONSE_BUFFER_SIZE * 2];
    let len = serde_json_core::to_slice(&update, &mut buffer).expect("worst case fits the probe");
    assert_eq!(
        len, WORST_CASE_BODY_LEN,
        "worst-case GET /api/config body changed; update WORST_CASE_BODY_LEN \
         and the figure quoted at CONFIG_RESPONSE_BUFFER_SIZE"
    );
    assert!(
        len <= CONFIG_RESPONSE_BUFFER_SIZE,
        "worst-case GET /api/config body is {len} bytes, which overflows \
         CONFIG_RESPONSE_BUFFER_SIZE ({CONFIG_RESPONSE_BUFFER_SIZE})"
    );
}

#[test]
fn api_info_chain_renders_each_chain() {
    static CHAINS: [ubos_metrics::chain::ChainStats; 2] = [
        ubos_metrics::chain::ChainStats::new(),
        ubos_metrics::chain::ChainStats::new(),
    ];
    CHAINS[0].set_presence(ubos_metrics::chain::ChainPresence::Present { chips: 105 });
    CHAINS[0].set_frequency(ubos_core::units::Frequency::from_mhz(650));
    CHAINS[0].set_hottest(Some(ubos_core::units::Temperature::from_celsius(72)));
    CHAINS[1].set_presence(ubos_metrics::chain::ChainPresence::Absent);

    let chains: heapless::Vec<ApiInfoChain, 2> = CHAINS
        .iter()
        .enumerate()
        .map(|(chain, stats)| ApiInfoChain::new(chain, stats))
        .collect();
    let mut buffer = [0u8; 512];
    let len = serde_json_core::to_slice(&chains, &mut buffer).expect("two chains fit the probe");
    let json = std::str::from_utf8(&buffer[..len]).expect("serde_json_core emits UTF-8");

    assert!(
        json.contains("{\"chain\":0,\"state\":\"present\",\"chips\":105,\"frequency_khz\":650000"),
        "chain 0 fields missing or wrong: {json}"
    );
    assert!(
        json.contains("\"hottest_c\":72,\"pool\":null}"),
        "chain 0 hottest_c or pool missing or wrong: {json}"
    );
    assert!(
        json.contains("{\"chain\":1,\"state\":\"absent\",\"chips\":0"),
        "chain 1 fields missing or wrong: {json}"
    );
}

// The `/api/info` schema carries the `ota` block and lists the phase
// strings, so a client generated from it reads the live body.
#[cfg(feature = "schema")]
#[test]
fn api_info_schema_names_the_ota_block() {
    let schema = schemars::schema_for!(ApiInfo);
    let value = serde_json::to_value(&schema).expect("schema is JSON");
    let properties = value["properties"].as_object().expect("root is an object");
    assert!(properties.contains_key("ota"));
    assert_eq!(
        enum_values(&value["$defs"]["OtaPhase"]),
        ["idle", "uploading", "error", "rebooting"]
    );
}

/// The string values a schema admits, whether it lists them as `enum`
/// or, when the variants carry descriptions, as `oneOf` constants.
#[cfg(feature = "schema")]
pub(crate) fn enum_values(schema: &serde_json::Value) -> std::vec::Vec<std::string::String> {
    extern crate std;
    use std::string::ToString;
    let values = schema["enum"]
        .as_array()
        .cloned()
        .or_else(|| {
            schema["oneOf"]
                .as_array()
                .map(|variants| variants.iter().map(|v| v["const"].clone()).collect())
        })
        .unwrap_or_else(|| panic!("no enum or oneOf in {schema}"));
    values
        .iter()
        .map(|v| v.as_str().expect("string value").to_string())
        .collect()
}

// The chain state serializes to the strings the dashboard reads,
// one per presence, so the enum changes nothing on the wire.
#[test]
fn chain_state_serializes_as_before() {
    let mut buf = [0u8; 16];
    for (presence, expected) in [
        (ChainPresence::Unknown, "\"unknown\""),
        (ChainPresence::Present { chips: 1 }, "\"present\""),
        (ChainPresence::Absent, "\"absent\""),
        (ChainPresence::Faulted { found: 0 }, "\"faulted\""),
    ] {
        let n = serde_json_core::to_slice(&ApiChainState::from(presence), &mut buf).expect("fits");
        assert_eq!(core::str::from_utf8(&buf[..n]).expect("utf8"), expected);
    }
}

// The mining state serializes to the string `as_api_str` gives, which
// the metrics label still uses, so the wire enum and the label agree.
#[test]
fn mining_state_serializes_as_api_str() {
    let mut buf = [0u8; 24];
    for state in [
        MiningState::WaitingForPool,
        MiningState::Starting,
        MiningState::Running,
        MiningState::Stopped,
    ] {
        let n = serde_json_core::to_slice(&ApiMiningState::from(state), &mut buf).expect("fits");
        let mut expected: heapless::String<24> = heapless::String::new();
        write!(expected, "\"{}\"", state.as_api_str()).expect("fits");
        assert_eq!(
            core::str::from_utf8(&buf[..n]).expect("utf8"),
            expected.as_str()
        );
    }
}

// The pause reason serializes to the strings the dashboard treats as
// an enum, spelled here as literals so the test outlives their source.
#[test]
fn pause_reason_serializes_as_before() {
    let mut buf = [0u8; 32];
    for (reason, expected) in [
        (PauseReason::BrokenFan, "\"brokenFan\""),
        (PauseReason::UserRequest, "\"userRequest\""),
        (PauseReason::OtaUpdate, "\"otaUpdate\""),
        (PauseReason::TunerFault, "\"tunerFault\""),
    ] {
        let n = serde_json_core::to_slice(&ApiPauseReason::from(reason), &mut buf).expect("fits");
        assert_eq!(core::str::from_utf8(&buf[..n]).expect("utf8"), expected);
    }
}

// The chain and pool indexes in `/api/info` carry a fixed width in
// the schema, so a generated client types them as small integers.
#[cfg(feature = "schema")]
#[test]
fn api_info_chain_schema_uses_uint8() {
    let schema = schemars::schema_for!(ApiInfoChain);
    let value = serde_json::to_value(&schema).expect("schema is JSON");
    assert_eq!(value["properties"]["chain"]["format"], "uint8");
    assert_eq!(value["properties"]["pool"]["format"], "uint8");
}

// --- the ack struct is the literal the handlers write ---

#[test]
fn ack_matches_the_literal_body() {
    let mut buffer = [0u8; 16];
    let len =
        serde_json_core::to_slice(&crate::schema::Ack { ok: true }, &mut buffer).expect("ack fits");
    assert_eq!(&buffer[..len], super::CONFIG_ACK_BODY);
}

#[test]
fn ota_chunk_ack_matches_the_literal_body() {
    let mut buffer = [0u8; 40];
    let ack = crate::schema::OtaChunkAck {
        ok: true,
        next: Some(8192),
    };
    let len = serde_json_core::to_slice(&ack, &mut buffer).expect("ack fits");
    assert_eq!(&buffer[..len], b"{\"ok\":true,\"next\":8192}");
}

#[test]
fn ota_chunk_ack_without_next_matches_the_final_literal() {
    let mut buffer = [0u8; 40];
    let ack = crate::schema::OtaChunkAck {
        ok: true,
        next: None,
    };
    let len = serde_json_core::to_slice(&ack, &mut buffer).expect("ack fits");
    assert_eq!(&buffer[..len], super::CONFIG_ACK_BODY);
}

// The chunk ack schema leaves `next` optional, since the final chunk
// answers without it, and types it as the byte offset's width.
#[cfg(feature = "schema")]
#[test]
fn ota_chunk_ack_schema_marks_next_optional() {
    let schema = schemars::schema_for!(crate::schema::OtaChunkAck);
    let value = serde_json::to_value(&schema).expect("schema is JSON");
    assert_eq!(value["required"], serde_json::json!(["ok"]));
    assert_eq!(value["properties"]["next"]["format"], "uint32");
}

#[test]
fn the_temperature_reasons_lead_the_pause_state_of_both_info_bodies() {
    let held = ubos_manager::PauseReasons::default().assert(ubos_manager::PauseReason::UserRequest);
    assert_eq!(
        crate::rest::pause_state(true, true, held),
        (true, Some(ApiPauseReason::DangerousTemperature))
    );
    assert_eq!(
        crate::rest::pause_state(false, true, held),
        (true, Some(ApiPauseReason::TemperatureUnreadable))
    );
}

#[test]
fn no_temperature_reason_leaves_the_pause_state_to_the_manager() {
    let none = ubos_manager::PauseReasons::default();
    assert_eq!(crate::rest::pause_state(false, false, none), (false, None));
    let held = none.assert(ubos_manager::PauseReason::UserRequest);
    assert_eq!(
        crate::rest::pause_state(false, false, held),
        (true, Some(ApiPauseReason::UserRequest))
    );
}
