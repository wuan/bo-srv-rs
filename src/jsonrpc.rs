//! JSON-RPC message handling and method dispatch, ported from
//! `txjsonrpc_ng.web.jsonrpc.JSONRPC.render` (with
//! `treat_zero_id_as_pre1 = True` as set by `base.py`) and the
//! `jsonrpclib.dumps` envelope rendering.
//!
//! Dialects (matching the live `tests/live/test_endpoint_legacy_protocol.py`):
//!
//! * explicit `jsonrpc` version field -> versioned dict envelope
//!   (`{"jsonrpc": "2.0", "result": .., "id": ..}`);
//! * no version field and id `0` (or missing/falsy) -> pre-1.0 bare array
//!   `[result]`;
//! * no version field and a truthy id -> JSON-RPC 1.0 dict envelope
//!   (`{"result": .., "error": null, "id": ..}`).
//!
//! Faults render dialect-specific: pre-1.0 `{"faultCode": .., "faultString":
//! .., "fault": "Fault"}`, v1 `{"result": null, "error": {...}, "id": ..}`,
//! v2 `{"jsonrpc": "2.0", "error": {"code": .., "message": .., "data": ""},
//! "id": ..}`.

use serde_json::{json, Value};

use crate::metrics::Metrics;
use crate::service::{Request, Service};

/// txjsonrpc's generic failure code (`JSONRPC.FAILURE`), also used for any
/// exception raised by a handler (the Python `TypeError` for a missing
/// required argument lands here via `_ebRender` + `_map_exception`).
pub const FAILURE: i64 = 8002;

/// JSON-RPC / XML-RPC standard error codes (the `xmlrpc.client` values that
/// txjsonrpc_ng re-exports).
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;

/// The response dialect selected for a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Envelope {
    /// `jsonrpclib.VERSION_PRE1`: bare array of one element.
    Pre1,
    /// `jsonrpclib.VERSION_1`.
    V1,
    /// `jsonrpclib.VERSION_2`.
    V2,
}

/// A successful method result rendered into the response envelope.
fn success_envelope(envelope: Envelope, id: &Value, result: &Value) -> Value {
    match envelope {
        // `_cbRender` wraps non-fault results in a tuple -> JSON array.
        Envelope::Pre1 => json!([result]),
        Envelope::V1 => json!({"result": result, "error": Value::Null, "id": id}),
        Envelope::V2 => json!({"jsonrpc": "2.0", "result": result, "id": id}),
    }
}

/// A fault rendered into the response envelope (the analogue of
/// `jsonrpclib.dumps(Fault, id=id, version=version)`).
fn fault_envelope(envelope: Envelope, id: &Value, code: i64, message: &str) -> Value {
    match envelope {
        Envelope::Pre1 => json!({
            "fault": "Fault",
            "faultCode": code,
            "faultString": message,
        }),
        Envelope::V1 => json!({
            "result": Value::Null,
            "error": {"fault": "Fault", "faultCode": code, "faultString": message},
            "id": id,
        }),
        Envelope::V2 => json!({
            "jsonrpc": "2.0",
            "error": {"message": message, "code": code, "data": ""},
            "id": id,
        }),
    }
}

/// Select the response dialect for `req`, mirroring `JSONRPC._select_version`
/// with `treat_zero_id_as_pre1 = True`.  An unparseable `jsonrpc` field makes
/// the Python code raise inside `_select_version` (`int(float(version))`),
/// which surfaces as a pre-1.0 `INVALID_JSONRPC` fault; `Err` signals that.
fn select_version(req: &Value) -> Result<Envelope, ()> {
    // An explicit version field always wins; `int(float(field))` selects the
    // dialect (2.0 -> v2, anything else -> the v1 dict shape).
    if let Some(field) = req.get("jsonrpc").and_then(Value::as_str) {
        let v: f64 = field.parse().map_err(|_| ())?;
        if !v.is_finite() {
            // float("inf"/"nan") parses but `int()` raises.
            return Err(());
        }
        return Ok(if v.trunc() as i64 == 2 {
            Envelope::V2
        } else {
            Envelope::V1
        });
    }
    match req.get("id") {
        // `if id is None` -> pre-1.0; `treat_zero_id_as_pre1 and not id` ->
        // pre-1.0 for 0/""/false.
        None | Some(Value::Null) => Ok(Envelope::Pre1),
        Some(Value::Number(n)) if n.as_i64() == Some(0) => Ok(Envelope::Pre1),
        Some(Value::String(s)) if s.is_empty() => Ok(Envelope::Pre1),
        Some(Value::Bool(false)) => Ok(Envelope::Pre1),
        _ => Ok(Envelope::V1),
    }
}

/// Method parameter spec: (name, required).
fn method_spec(method: &str) -> Option<&'static [(&'static str, bool)]> {
    match method {
        "check" => Some(&[]),
        "get_strikes" => Some(&[("minute_length", true), ("id_or_offset", false)]),
        "get_strikes_grid" | "get_strikes_raster" | "get_strokes_raster" => Some(&[
            ("minute_length", true),
            ("grid_base_length", false),
            ("minute_offset", false),
            ("region", false),
            ("count_threshold", false),
        ]),
        "get_global_strikes_grid" => Some(&[
            ("minute_length", true),
            ("grid_base_length", false),
            ("minute_offset", false),
            ("count_threshold", false),
        ]),
        "get_local_strikes_grid" => Some(&[
            ("x", true),
            ("y", true),
            ("grid_base_length", false),
            ("minute_length", false),
            ("minute_offset", false),
            ("count_threshold", false),
            ("data_area", false),
        ]),
        _ => None,
    }
}

/// Default parameter values (the Python method defaults).
fn method_defaults(method: &str) -> Vec<Value> {
    match method {
        "check" => vec![],
        "get_strikes" => vec![Value::Null, json!(0)],
        "get_strikes_grid" | "get_strikes_raster" | "get_strokes_raster" => {
            vec![Value::Null, json!(10_000), json!(0), json!(1), json!(0)]
        }
        "get_global_strikes_grid" => vec![Value::Null, json!(10_000), json!(0), json!(0)],
        "get_local_strikes_grid" => {
            vec![
                Value::Null,
                Value::Null,
                json!(10_000),
                json!(60),
                json!(0),
                json!(0),
                json!(5),
            ]
        }
        _ => vec![],
    }
}

/// Resolve the arguments for a request: positional array or named object.
#[derive(Debug, PartialEq, Eq)]
enum ArgError {
    /// `params` is neither an array nor an object ->
    /// `INVALID_METHOD_PARAMS` (-32602).
    InvalidParamsType,
    /// A required argument is missing entirely — the Python handler raises a
    /// `TypeError` here, which txjsonrpc maps to `FAILURE` (8002).  A
    /// parameter that is *present* (even as `null`) is passed through, where
    /// the service's `__to_int` coercion decides whether the request is
    /// blocked.
    MissingRequired(&'static str),
}

fn resolve_args(method: &str, params: &Value) -> Result<Vec<Value>, ArgError> {
    let spec = method_spec(method).expect("checked by caller");
    let defaults = method_defaults(method);
    let mut args = vec![Value::Null; defaults.len()];
    let mut present = vec![false; defaults.len()];

    match params {
        Value::Null => {}
        Value::Array(items) => {
            for (index, value) in items.iter().enumerate() {
                if index < args.len() {
                    args[index] = value.clone();
                    present[index] = true;
                }
            }
        }
        Value::Object(map) => {
            for (index, (name, _)) in spec.iter().enumerate() {
                if let Some(value) = map.get(*name) {
                    args[index] = value.clone();
                    present[index] = true;
                }
            }
        }
        _ => return Err(ArgError::InvalidParamsType),
    }

    for index in 0..args.len() {
        if present[index] {
            continue;
        }
        if spec[index].1 {
            return Err(ArgError::MissingRequired(spec[index].0));
        }
        args[index] = defaults[index].clone();
    }

    Ok(args)
}

/// Invoke a JSON-RPC method on the service.  Returns the raw result value.
fn invoke<M: Metrics>(service: &Service<M>, request: &mut Request, method: &str, args: &[Value]) -> Value {
    match method {
        "check" => service.check(),
        "get_strikes" => service
            .get_strikes(request, &args[0], args.get(1).unwrap_or(&json!(0)))
            .unwrap_or(Value::Null),
        "get_strikes_grid" => service.jsonrpc_get_strikes_grid(
            request,
            &args[0],
            &args[1],
            &args[2],
            &args[3],
            &args[4],
        ),
        "get_strikes_raster" | "get_strokes_raster" => service.jsonrpc_get_strikes_raster(
            request,
            &args[0],
            &args[1],
            &args[2],
            &args[3],
        ),
        "get_global_strikes_grid" => service.jsonrpc_get_global_strikes_grid(
            request,
            &args[0],
            &args[1],
            &args[2],
            &args[3],
        ),
        "get_local_strikes_grid" => service.jsonrpc_get_local_strikes_grid(
            request,
            &args[0],
            &args[1],
            &args[2],
            &args[3],
            &args[4],
            &args[5],
            &args[6],
        ),
        _ => unreachable!("method checked by caller"),
    }
}

/// Process a single parsed JSON-RPC request object.
fn process_request<M: Metrics>(service: &Service<M>, request: &mut Request, req: &Value) -> Option<String> {
    // A non-object request (e.g. a batch array) is rejected like txjsonrpc's
    // `raise Fault(INVALID_JSONRPC, "Invalid Request: expected a JSON object")`.
    if !req.is_object() {
        let response = request_fault(
            Envelope::Pre1,
            &Value::Null,
            INVALID_REQUEST,
            "Invalid Request: expected a JSON object",
        );
        return Some(serde_json::to_string(&response).expect("serializable"));
    }

    let envelope = match select_version(req) {
        Ok(envelope) => envelope,
        Err(()) => {
            // `int(float("abc"))` raises a ValueError inside
            // `_select_version`: pre-1.0 INVALID_JSONRPC fault.
            let response = request_fault(
                Envelope::Pre1,
                &Value::Null,
                INVALID_REQUEST,
                "invalid jsonrpc version field",
            );
            return Some(serde_json::to_string(&response).expect("serializable"));
        }
    };
    let id = req.get("id").cloned().unwrap_or(Value::Null);

    let Some(method) = req.get("method").and_then(Value::as_str) else {
        let response = request_fault(envelope, &id, INVALID_REQUEST, "Invalid Request: missing method");
        return Some(serde_json::to_string(&response).expect("serializable"));
    };

    if method_spec(method).is_none() {
        // txjsonrpc: NoSuchFunction -> Fault(METHOD_NOT_FOUND).
        let response = request_fault(
            envelope,
            &id,
            METHOD_NOT_FOUND,
            &format!("function {method} not found"),
        );
        return Some(serde_json::to_string(&response).expect("serializable"));
    }

    let params = req.get("params").cloned().unwrap_or(Value::Null);
    let args = match resolve_args(method, &params) {
        Ok(args) => args,
        Err(ArgError::InvalidParamsType) => {
            // txjsonrpc raises this Fault *before* reading `id` and selecting
            // the version, so it renders as a bare pre-1.0 fault dict.
            let response = request_fault(
                Envelope::Pre1,
                &Value::Null,
                INVALID_PARAMS,
                "Invalid params: expected an array or object",
            );
            return Some(serde_json::to_string(&response).expect("serializable"));
        }
        Err(ArgError::MissingRequired(name)) => {
            // The Python handler would raise
            // `TypeError: jsonrpc_get_strikes_grid() missing 1 required
            // positional argument: 'minute_length'`, which txjsonrpc maps to
            // a FAILURE fault (message = repr of the exception).
            let message = format!("missing 1 required positional argument: '{name}'");
            let response = request_fault(envelope, &id, FAILURE, &message);
            return Some(serde_json::to_string(&response).expect("serializable"));
        }
    };

    let result = invoke(service, request, method, &args);
    let response = success_envelope(envelope, &id, &result);
    Some(serde_json::to_string(&response).expect("serializable"))
}

/// Request-level fault (parse error, unknown method, missing parameter).
fn request_fault(envelope: Envelope, id: &Value, code: i64, message: &str) -> Value {
    fault_envelope(envelope, id, code, message)
}

/// Process a JSON-RPC request body and return the serialized response body.
pub fn process<M: Metrics>(service: &Service<M>, request: &mut Request, body: &str) -> Option<String> {
    let parsed: Result<Value, serde_json::Error> = serde_json::from_str(body);
    let request_value = match parsed {
        Ok(v) => v,
        Err(_) => {
            // A JSON parse error surfaces as a `ValueError` in txjsonrpc and
            // is answered with a Fault(INVALID_JSONRPC) in the default
            // pre-1.0 dialect (id/version defaults apply because parsing
            // failed up front).
            let response = request_fault(Envelope::Pre1, &Value::Null, INVALID_REQUEST, "parse error");
            return Some(serde_json::to_string(&response).expect("serializable"));
        }
    };
    process_request(service, request, &request_value)
}

/// Dispatch helper used by the transport.
pub fn dispatch<M: Metrics>(service: &Service<M>, request: &mut Request, body: &str) -> Option<String> {
    process(service, request, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockExecutor;

    fn service() -> Service<crate::metrics::NoopMetrics> {
        Service::new(std::sync::Arc::new(MockExecutor::new()))
    }

    fn parse(body: &str) -> Value {
        serde_json::from_str(body).expect("valid JSON response")
    }

    #[test]
    fn legacy_zero_id_gets_bare_array() {
        let service = service();
        let response = process(
            &service,
            &mut Request::default(),
            r#"{"method":"check","params":[],"id":0}"#,
        )
        .unwrap();
        let v = parse(&response);
        assert!(v.is_array(), "expected bare array, got {v}");
        assert_eq!(v[0]["count"], 1);
    }

    #[test]
    fn non_zero_id_gets_v1_object() {
        let service = service();
        let response = process(
            &service,
            &mut Request::default(),
            r#"{"method":"check","params":[],"id":1}"#,
        )
        .unwrap();
        let v = parse(&response);
        assert!(v.is_object());
        assert_eq!(v["id"], 1);
        assert_eq!(v["result"]["count"], 1);
        assert_eq!(v["error"], Value::Null);
    }

    #[test]
    fn explicit_jsonrpc_2_zero_id_gets_v2_object() {
        let service = service();
        let response = process(
            &service,
            &mut Request::default(),
            r#"{"jsonrpc":"2.0","method":"check","params":[],"id":0}"#,
        )
        .unwrap();
        let v = parse(&response);
        assert!(v.is_object());
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 0);
        assert_eq!(v["result"]["count"], 1);
    }

    #[test]
    fn missing_id_defaults_to_pre1() {
        let service = service();
        let response = process(
            &service,
            &mut Request::default(),
            r#"{"method":"check","params":[]}"#,
        )
        .unwrap();
        let v = parse(&response);
        assert!(v.is_array());
        assert_eq!(v[0]["count"], 1);
    }

    #[test]
    fn parse_error_returns_legacy_fault() {
        let service = service();
        let body = process(&service, &mut Request::default(), "{not json").unwrap();
        let v = parse(&body);
        // txjsonrpc maps parse errors to Fault(INVALID_JSONRPC).
        assert_eq!(v["faultCode"], INVALID_REQUEST);
    }

    #[test]
    fn non_object_request_is_invalid() {
        let service = service();
        let body = process(&service, &mut Request::default(), "[1, 2]").unwrap();
        let v = parse(&body);
        assert_eq!(v["faultCode"], INVALID_REQUEST);
    }

    #[test]
    fn method_not_found() {
        let service = service();
        let body = process(
            &service,
            &mut Request::default(),
            r#"{"jsonrpc":"2.0","id":1,"method":"nope"}"#,
        )
        .unwrap();
        let v = parse(&body);
        assert_eq!(v["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(v["id"], 1);
    }

    #[test]
    fn missing_required_param_is_a_fault() {
        let service = service();
        let body = process(
            &service,
            &mut Request::default(),
            r#"{"jsonrpc":"2.0","id":2,"method":"get_strikes_grid"}"#,
        )
        .unwrap();
        let v = parse(&body);
        // Python TypeError -> txjsonrpc FAILURE fault (8002).
        assert_eq!(v["error"]["code"], FAILURE);
        assert_eq!(
            v["error"]["message"],
            "missing 1 required positional argument: 'minute_length'"
        );
    }

    #[test]
    fn params_of_wrong_type_is_invalid_params() {
        let service = service();
        // The Fault is raised before id/version selection in txjsonrpc, so
        // the response is a bare pre-1.0 fault dict even for a v2 request.
        let body = process(
            &service,
            &mut Request::default(),
            r#"{"jsonrpc":"2.0","id":2,"method":"check","params":42}"#,
        )
        .unwrap();
        let v = parse(&body);
        assert_eq!(v["faultCode"], INVALID_PARAMS);
        assert!(v.get("id").is_none());
    }

    #[test]
    fn invalid_version_field_is_a_legacy_fault() {
        let service = service();
        // ValueError inside _select_version -> pre-1.0 INVALID_JSONRPC fault.
        let body = process(
            &service,
            &mut Request::default(),
            r#"{"jsonrpc":"abc","id":5,"method":"check","params":[]}"#,
        )
        .unwrap();
        let v = parse(&body);
        assert_eq!(v["faultCode"], INVALID_REQUEST);
        assert!(v.get("id").is_none());
    }

    #[test]
    fn explicit_null_required_param_is_not_a_fault() {
        let service = service();
        // minute_length is present (null) so no TypeError: the service's
        // __to_int coercion blocks it instead with {}.
        let body = process(
            &service,
            &mut Request::default(),
            r#"{"jsonrpc":"2.0","id":2,"method":"get_strikes_grid","params":[null,10000,0,1,0]}"#,
        )
        .unwrap();
        let v = parse(&body);
        assert_eq!(v["result"], json!({}));
    }

    #[test]
    fn named_params_are_supported() {
        let service = service();
        let body = process(
            &service,
            &mut Request::default(),
            r#"{"jsonrpc":"2.0","id":3,"method":"check","params":{}}"#,
        )
        .unwrap();
        let v = parse(&body);
        assert_eq!(v["result"]["count"], 1);
    }

    #[test]
    fn get_strikes_returns_null_result() {
        let service = service();
        let body = process(
            &service,
            &mut Request::default(),
            r#"{"jsonrpc":"2.0","id":4,"method":"get_strikes","params":[60,0]}"#,
        )
        .unwrap();
        let v = parse(&body);
        assert_eq!(v["result"], Value::Null);
    }

    #[test]
    fn blocked_data_request_pre1_returns_bare_empty_dict() {
        let service = service();
        // No headers -> user agent invalid -> blocked -> {} inside the
        // pre-1.0 array.
        let body = process(
            &service,
            &mut Request::default(),
            r#"{"method":"get_strikes_grid","params":[60,10000,0,1,0],"id":0}"#,
        )
        .unwrap();
        let v = parse(&body);
        assert!(v.is_array());
        assert_eq!(v[0], json!({}));
    }
}