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

//! Generates the `OpenAPI` document for `/api/*` and gates the committed
//! copy at `ubos/ubos-api/openapi.json`.
//!
//! The table below is the one hand-written part: the types know their
//! shape, and nothing in the crate knows which operation serves which
//! type. Every `/api/system` entry takes its method and path from
//! [`Route::endpoint`], and every body schema comes from the `JsonSchema`
//! derive on the wire type, so a field change fails
//! `committed_document_is_current` until the file is regenerated with
//! `UBOS_OPENAPI_WRITE=1`.

extern crate std;

use std::string::{String, ToString};
use std::vec::Vec;

use schemars::{JsonSchema, SchemaGenerator, generate::SchemaSettings};
use serde_json::{Map, Value, json};

use crate::schema::{
    API_VARIANT, Ack, GenericResponse, OtaChunkAck, SystemInfo, SystemNetwork,
    SystemScoreboardEntry, SystemStatistics, SystemTemperature,
    events::{EventDefJson, EventsResponse},
    info::ApiInfo,
};
use crate::system::{DispatchOutcome, NetworkPostRequest, Route, route};

/// The committed document this test keeps current.
const COMMITTED: &str = include_str!("../../openapi.json");

/// Builds the schema of one body against the shared generator.
type BodyFn = fn(&mut SchemaGenerator) -> Value;

/// A body that is one wire type.
fn schema<T: JsonSchema>(generator: &mut SchemaGenerator) -> Value {
    to_value(generator.subschema_for::<T>())
}

/// A body that is an array of one wire type.
fn array_of<T: JsonSchema>(generator: &mut SchemaGenerator) -> Value {
    json!({ "type": "array", "items": generator.subschema_for::<T>() })
}

/// A raw firmware image.
fn octet_stream(_: &mut SchemaGenerator) -> Value {
    json!({ "type": "string", "format": "binary" })
}

/// A body the handler reads and discards.
fn any_object(_: &mut SchemaGenerator) -> Value {
    json!({ "type": "object", "description": "Read and discarded." })
}

fn to_value<T: serde::Serialize>(value: T) -> Value {
    serde_json::to_value(value).expect("schemas are JSON")
}

/// Who may call: any authenticated client, or a session client with a
/// CSRF token as well.
enum Auth {
    Any,
    Csrf,
}

/// A query or header parameter.
struct Param {
    name: &'static str,
    place: &'static str,
    required: bool,
    integer: bool,
    description: &'static str,
}

/// One response: status, description, optional JSON body.
struct Response {
    status: u16,
    description: &'static str,
    body: Option<BodyFn>,
}

/// The request body an operation reads.
struct RequestBody {
    media_type: &'static str,
    body: BodyFn,
    required: bool,
}

/// One operation of the table.
struct Operation {
    method: &'static str,
    path: &'static str,
    summary: &'static str,
    auth: Auth,
    params: &'static [Param],
    request: Option<RequestBody>,
    responses: Vec<Response>,
}

const fn ok(description: &'static str, body: BodyFn) -> Response {
    Response {
        status: 200,
        description,
        body: Some(body),
    }
}

const fn error(status: u16, description: &'static str) -> Response {
    Response {
        status,
        description,
        body: Some(schema::<GenericResponse>),
    }
}

const fn json_body(body: BodyFn) -> RequestBody {
    RequestBody {
        media_type: "application/json",
        body,
        required: true,
    }
}

const fn image_body() -> RequestBody {
    RequestBody {
        media_type: "application/octet-stream",
        body: octet_stream,
        required: true,
    }
}

const SINCE: Param = Param {
    name: "since",
    place: "query",
    required: false,
    integer: true,
    description: "Return only records whose seq is greater than this cursor.",
};

const OTA_OFFSET: Param = Param {
    name: "X-OTA-Offset",
    place: "header",
    required: true,
    integer: true,
    description: "Decimal byte offset where this chunk begins in the image.",
};

const OTA_FINAL: Param = Param {
    name: "X-OTA-Final",
    place: "header",
    required: false,
    integer: false,
    description: "Set to \"1\" on the last chunk only.",
};

const CSRF: Param = Param {
    name: "X-CSRF-Token",
    place: "header",
    required: false,
    integer: false,
    description: "Required for a session client; a Basic client is exempt.",
};

const ACK: Response = ok("Acknowledged", schema::<GenericResponse>);

const UNSUPPORTED: Response = Response {
    status: 501,
    description: "Not available on this platform",
    body: Some(schema::<GenericResponse>),
};

const BUSY: Response = error(503, "Config task busy; retry");

/// The OTA failure set both upload routes share.
const OTA_ERRORS: [Response; 7] = [
    error(
        400,
        "Image or chunk malformed or too short, image for other hardware, or connection failed",
    ),
    error(
        409,
        "An update is in progress, the device is not provisioned, or the chunk offset does not match",
    ),
    error(411, "Content-Length required"),
    error(413, "Image or chunk exceeds its limit"),
    error(500, "Board could not write the image"),
    error(501, "OTA not available on this board"),
    error(503, "Mining did not stop in time; retry"),
];

fn ota_responses(ack: BodyFn) -> Vec<Response> {
    let mut responses = std::vec![ok("Image accepted", ack)];
    responses.extend(OTA_ERRORS);
    responses
}

/// An operation the router serves, at the method and path its variant
/// names.
fn served(
    route: Route,
    summary: &'static str,
    auth: Auth,
    params: &'static [Param],
    request: Option<RequestBody>,
    responses: Vec<Response>,
) -> Operation {
    let (method, path) = route.endpoint();
    Operation {
        method,
        path,
        summary,
        auth,
        params,
        request,
        responses,
    }
}

/// A POST that answers the ack body and nothing else.
fn action(route: Route, summary: &'static str) -> Operation {
    served(route, summary, Auth::Csrf, &[], None, std::vec![ACK])
}

/// A GET that answers one wire type.
fn read(route: Route, summary: &'static str, body: BodyFn, description: &'static str) -> Operation {
    served(
        route,
        summary,
        Auth::Any,
        &[],
        None,
        std::vec![Response {
            status: 200,
            description,
            body: Some(body)
        }],
    )
}

/// Every operation the firmware serves under `/api/`. The two routes
/// `rest.rs` serves outside `route()` name their path here; the rest
/// take theirs from `Route`.
// One table, read top to bottom against the router; splitting it
// would hide half the surface behind a call.
#[allow(clippy::too_many_lines)]
fn operations() -> Vec<Operation> {
    std::vec![
        Operation {
            method: "GET",
            path: "/api/info",
            summary: "Dashboard telemetry poll",
            auth: Auth::Any,
            params: &[],
            request: None,
            responses: std::vec![
                ok("Telemetry", schema::<ApiInfo>),
                error(500, "Body exceeds its buffer"),
            ],
        },
        Operation {
            method: "GET",
            path: "/api/config",
            summary: "Configuration snapshot",
            auth: Auth::Any,
            params: &[],
            request: None,
            responses: std::vec![
                ok(
                    "Snapshot with api.password blanked",
                    schema::<ubos_config::ConfigUpdate>
                ),
                error(500, "Body exceeds its buffer"),
            ],
        },
        Operation {
            method: "POST",
            path: "/api/config",
            summary: "Partial configuration update",
            auth: Auth::Csrf,
            params: &[],
            request: Some(json_body(schema::<ubos_config::ConfigUpdate>)),
            responses: std::vec![
                ok("Applied and persisted", schema::<Ack>),
                error(400, "Invalid JSON or validation failed"),
            ],
        },
        read(
            Route::SystemInfo,
            "System telemetry",
            schema::<SystemInfo>,
            "Versioned telemetry"
        ),
        read(
            Route::SystemAsic,
            "Chip facts and tuning bounds",
            schema::<ubos_config::MiningInfo>,
            "Chip facts"
        ),
        read(
            Route::SystemThermal,
            "Thermal limits and bounds",
            schema::<ubos_config::ThermalInfo>,
            "Thermal limits"
        ),
        read(
            Route::SystemTemperature,
            "Last thermal sample",
            schema::<SystemTemperature>,
            "Temperatures and PID source"
        ),
        read(
            Route::SystemCooling,
            "Cooling setpoints and bounds",
            schema::<ubos_config::CoolingInfo>,
            "Cooling setpoints"
        ),
        read(
            Route::SystemStatistics,
            "Time series ring",
            schema::<SystemStatistics>,
            "Label set and samples"
        ),
        read(
            Route::SystemScoreboard,
            "Best shares",
            array_of::<SystemScoreboardEntry>,
            "Scoreboard entries"
        ),
        read(
            Route::NetworkGet,
            "Network mode and pending change",
            schema::<SystemNetwork>,
            "Network state"
        ),
        served(
            Route::NetworkPost,
            "Apply a network change, previewed or final",
            Auth::Csrf,
            &[],
            Some(json_body(schema::<NetworkPostRequest>)),
            std::vec![
                ok("Applied", schema::<Ack>),
                error(
                    400,
                    "Invalid JSON, validation failed, or a static change needs the confirm window"
                ),
                BUSY,
            ],
        ),
        served(
            Route::NetworkConfirm,
            "Confirm a pending network preview",
            Auth::Csrf,
            &[],
            None,
            std::vec![ACK, BUSY],
        ),
        served(
            Route::NetworkRevert,
            "Revert a pending network preview",
            Auth::Csrf,
            &[],
            None,
            std::vec![ACK, BUSY],
        ),
        served(
            Route::SystemEvents,
            "Event ring snapshot, oldest first",
            Auth::Any,
            &[SINCE],
            None,
            std::vec![ok("Records after the cursor", schema::<EventsResponse>)],
        ),
        read(
            Route::SystemEventsCatalog,
            "Event catalog",
            array_of::<EventDefJson>,
            "One entry per event code"
        ),
        served(
            Route::SystemWifiScan,
            "Wi-Fi scan",
            Auth::Any,
            &[],
            None,
            std::vec![UNSUPPORTED],
        ),
        served(
            Route::SystemOta,
            "Upload a firmware image in one request",
            Auth::Csrf,
            &[],
            Some(image_body()),
            ota_responses(schema::<Ack>),
        ),
        served(
            Route::SystemOtaChunk,
            "Upload one sector of a firmware image; a continuation chunk, offset above 0, needs no CSRF token",
            Auth::Csrf,
            &[OTA_OFFSET, OTA_FINAL],
            Some(image_body()),
            // The final chunk answers without `next`; the schema marks it optional.
            ota_responses(schema::<OtaChunkAck>),
        ),
        served(
            Route::SystemOtaWww,
            "Upload a web UI image",
            Auth::Csrf,
            &[],
            None,
            std::vec![UNSUPPORTED],
        ),
        action(Route::PauseAck, "Stop mining"),
        served(
            Route::ResumeAck,
            "Start mining",
            Auth::Csrf,
            &[],
            None,
            std::vec![ACK, error(409, "An overheat halt stands; clear it first")],
        ),
        action(
            Route::ThermalClearAck,
            "Clear an overheat halt and start mining"
        ),
        action(Route::RestartAck, "Reboot once the response is sent"),
        action(Route::FactoryReset, "Wipe persisted config and reboot"),
        action(
            Route::IdentifyAck,
            "Identify stub: logs and acknowledges the request"
        ),
        action(
            Route::BlockFoundDismissAck,
            "Block-found dismissal stub: logs and acknowledges the request"
        ),
        served(
            Route::PatchSystem,
            "No-op stub: acknowledges and applies nothing",
            Auth::Csrf,
            &[],
            Some(RequestBody {
                media_type: "application/json",
                body: any_object,
                required: false,
            }),
            std::vec![ACK],
        ),
    ]
}

fn parameter(param: &Param) -> Value {
    json!({
        "name": param.name,
        "in": param.place,
        "required": param.required,
        "description": param.description,
        "schema": { "type": if param.integer { "integer" } else { "string" } }
    })
}

/// The two OTA uploads read the connection themselves; every other
/// mutating route is pre-read into a bounded buffer and answers 413
/// when the body does not fit.
fn is_pre_read(operation: &Operation) -> bool {
    operation.method != "GET" && !operation.path.starts_with("/api/system/OTA")
}

fn render(operation: &Operation, generator: &mut SchemaGenerator) -> Value {
    let mut parameters: Vec<Value> = operation.params.iter().map(parameter).collect();
    let mut responses = Map::new();
    for response in &operation.responses {
        let mut entry = json!({ "description": response.description });
        if let Some(body) = response.body {
            entry["content"] = json!({ "application/json": { "schema": body(generator) } });
        }
        responses.insert(response.status.to_string(), entry);
    }
    let generic = schema::<GenericResponse>(generator);
    responses.insert(
        "401".to_string(),
        json!({ "description": "Not on the allow-list or not authenticated",
                "content": { "application/json": { "schema": generic } } }),
    );
    responses.insert(
        "405".to_string(),
        json!({ "description": "Method not allowed on this path" }),
    );
    if is_pre_read(operation) {
        responses.insert(
            "413".to_string(),
            json!({ "description": "Body exceeds the request buffer" }),
        );
    }
    if matches!(operation.auth, Auth::Csrf) {
        parameters.push(parameter(&CSRF));
        responses.insert(
            "403".to_string(),
            json!({ "description": "CSRF token invalid or missing",
                    "content": { "application/json": { "schema": generic } } }),
        );
    }
    let mut rendered = json!({
        "summary": operation.summary,
        "parameters": parameters,
        "responses": responses,
    });
    if let Some(request) = &operation.request {
        rendered["requestBody"] = json!({
            "required": request.required,
            "content": { request.media_type: { "schema": (request.body)(generator) } }
        });
    }
    rendered
}

/// Assembles the whole document from the table and the derives. Every
/// body is described as the device serializes it, so a key the device
/// always writes is required and a key it writes but never reads is
/// present; the two JSON request bodies are read by the same types and
/// take no harm from the stricter shape.
fn document() -> Value {
    let mut settings = SchemaSettings::draft2020_12().for_serialize();
    settings.definitions_path = "/components/schemas".into();
    let mut generator = settings.into_generator();
    let mut paths = Map::new();
    for operation in operations() {
        let entry = paths.entry(operation.path).or_insert_with(|| json!({}));
        entry[operation.method.to_lowercase()] = render(&operation, &mut generator);
    }
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "ubos REST API",
            "version": API_VARIANT,
            "description": "The /api/* surface of the ubos firmware. Generated from the Rust wire types by the ubos-api host tests."
        },
        "servers": [{ "url": "http://{host}", "variables": { "host": { "default": "ubos.local" } } }],
        "security": [{ "basic": [] }, { "session": [] }],
        "paths": paths,
        "components": {
            "securitySchemes": {
                "basic": { "type": "http", "scheme": "basic" },
                "session": { "type": "apiKey", "in": "cookie", "name": "session" }
            },
            "schemas": generator.take_definitions(true)
        }
    })
}

/// Every `description` string in the document, with the path it sits at.
fn descriptions(value: &Value, path: &str, out: &mut Vec<(String, String)>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if key == "description"
                    && let Some(text) = child.as_str()
                {
                    out.push((path.to_string(), text.to_string()));
                }
                descriptions(child, &std::format!("{path}/{key}"), out);
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                descriptions(child, &std::format!("{path}/{i}"), out);
            }
        }
        _ => {}
    }
}

/// The committed document matches the types. Set `UBOS_OPENAPI_WRITE=1`
/// to rewrite it, then run again.
#[test]
fn committed_document_is_current() {
    let mut rendered = serde_json::to_string_pretty(&document()).expect("document is JSON");
    rendered.push('\n');
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/openapi.json");
    if std::env::var_os("UBOS_OPENAPI_WRITE").is_some() {
        std::fs::write(path, &rendered).expect("openapi.json is writable");
        return;
    }
    assert!(
        rendered == COMMITTED,
        "{path} is stale. From ubos/ubos-api run \
         `UBOS_OPENAPI_WRITE=1 cargo test --no-default-features --features schema` \
         and commit the result."
    );
}

/// Two renders are byte-equal, so the committed file never churns.
#[test]
fn document_is_deterministic() {
    let first = serde_json::to_string(&document()).expect("document is JSON");
    let second = serde_json::to_string(&document()).expect("document is JSON");
    assert_eq!(first, second);
}

/// Every `/api/system` entry in the table reaches the router.
#[test]
fn every_table_entry_is_served() {
    for operation in operations() {
        if !operation.path.starts_with("/api/system") {
            continue;
        }
        let outcome = route(operation.method, operation.path);
        assert!(
            matches!(outcome, DispatchOutcome::Handled(_)),
            "{} {} is in the table but not in the router",
            operation.method,
            operation.path
        );
    }
}

/// Every variant of `Route` has a table entry, so a new route without
/// one fails here.
#[test]
fn every_route_variant_is_in_the_table() {
    let table: Vec<(&str, &str)> = operations()
        .iter()
        .map(|operation| (operation.method, operation.path))
        .collect();
    for variant in Route::ALL {
        let endpoint = variant.endpoint();
        assert!(
            table.contains(&endpoint),
            "{variant:?} ({} {}) has no table entry",
            endpoint.0,
            endpoint.1
        );
    }
}

/// No description leaks Rust: no intra-doc link, no `None` or `Some`,
/// no `Vec`, and no pointer into the repository.
#[test]
fn descriptions_address_the_client() {
    let mut found = Vec::new();
    descriptions(&document(), "", &mut found);
    assert!(!found.is_empty());
    let leaks: Vec<&(String, String)> = found
        .iter()
        .filter(|(_, text)| {
            ["[`", "`None`", "Some(", "Vec", "docs/", "::", "()", "crate"]
                .iter()
                .any(|needle| text.contains(needle))
        })
        .collect();
    assert!(leaks.is_empty(), "descriptions leak Rust: {leaks:#?}");
}

/// A response body is described as the device serializes it: a key the
/// device always writes is required, and a key it writes but never
/// reads, such as the fee entry of the config, is present.
#[test]
fn bodies_follow_the_serialize_contract() {
    let document = document();
    let schemas = &document["components"]["schemas"];
    let config = schemas["ConfigUpdate"]["properties"]
        .as_object()
        .expect("ConfigUpdate is an object");
    assert!(config.contains_key("fee"), "fee is missing: {config:#?}");
    assert!(config.contains_key("fee_permille"));
    let required = schemas["ApiInfo"]["required"]
        .as_array()
        .expect("ApiInfo lists its required keys");
    assert!(required.contains(&json!("pause_reason")), "{required:?}");
}

/// A bounded array states its capacity, so a client learns why a
/// longer one is rejected.
#[test]
fn bounded_arrays_state_their_capacity() {
    let document = document();
    let schemas = &document["components"]["schemas"];
    assert_eq!(
        schemas["ConfigUpdate"]["properties"]["pool"]["maxItems"],
        json!(ubos_config::pool::MAX_POOLS)
    );
    assert_eq!(
        schemas["HwInfo"]["properties"]["hashboardIds"]["maxItems"],
        json!(ubos_metrics::chain::MAX_CHAINS)
    );
    assert_eq!(
        schemas["SystemNetwork"]["properties"]["dns"]["maxItems"],
        json!(ubos_config::network::DNS_SERVERS)
    );
}
