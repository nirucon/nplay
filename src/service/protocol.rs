//! Versioned local control protocol (port of `nplay/service/protocol.py`).
//!
//! Framing is newline-delimited JSON (UTF-8) over a Unix-domain socket owned
//! by the invoking user; requests and notifications use JSON-RPC 2.0. This
//! module has no playback knowledge: constants, safe framing, errors only.
//!
//! Public API:
//! - Constants `PROTOCOL_VERSION` (1), `PROTOCOL_MINOR` (2), `SERVICE_NAME`,
//!   `MAX_FRAME` (1 MiB), `MAX_TRACKS` (5000), `POSITION_BUCKETS` (64) and the
//!   error codes `PROTOCOL_ERROR`, `METHOD_NOT_FOUND`, `INVALID_PARAMS`,
//!   `INTERNAL_ERROR`, `BACKEND_ERROR`, `TOO_MANY_OUTSTANDING`,
//!   `INCOMPATIBLE_PROTOCOL`.
//! - `RpcError { code, message, data }`: structured error safe to show to
//!   local clients. Constructors `new`, `with_data`, `protocol` (-32600),
//!   `method` (-32601), `params` (-32602), `backend` (-32000), plus
//!   `invalid_parameters(detail)` / `internal(detail)` which build the
//!   server's generic -32602 "invalid parameters" / -32603 "internal error"
//!   with a redacted detail bounded to 200 characters (what Python produced
//!   for ValueError/KeyError/TypeError and other exceptions). `payload()`
//!   gives `{"code","message"[,"data"]}` (data only when non-empty).
//! - `runtime_dir()`, `socket_path(override)`, `lock_path(path)`: the socket
//!   location rules (`--service-socket`, `NPLAY_SERVICE_SOCKET`,
//!   `$XDG_RUNTIME_DIR/nplay`, `$XDG_STATE_HOME/nplay/run`,
//!   `~/.local/state/nplay/run`), `control.sock` and `control.sock.lock`.
//! - `encode(&Value) -> Vec<u8>`: one frame, byte identical to Python
//!   `json.dumps(obj, ensure_ascii=False, separators=(",", ":")) + "\n"`
//!   (same string escapes, Python float repr such as `1e+16`, `1.0`).
//! - `dumps(&Value) -> String`: the same without the newline.
//! - `response`, `error_response`, `notification`: the envelopes, key order
//!   `jsonrpc, id, result|error` and `jsonrpc, method, params`.
//! - `parse_request(&[u8]) -> Result<(Value, String, Map), RpcError>`:
//!   validate one incoming envelope with Python's exact error messages; the
//!   id is `null`, a string, an integer or a bool (Python's `int` includes
//!   `bool`).

use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

pub const PROTOCOL_VERSION: i64 = 1;
// Minor version: additions only, every 1.x request keeps working.
//   1.0  NPLAY 1.7.0  initial protocol
//   1.1  NPLAY 1.8.0  visualization.subscribe/unsubscribe/status push stream
//   1.2  NPLAY 1.8.1  spotify.local_authorize, spotify.local_auth and
//                     service.error notifications, local receiver state
pub const PROTOCOL_MINOR: i64 = 2;
pub const SERVICE_NAME: &str = "nplay";
pub const MAX_FRAME: usize = 1024 * 1024;
pub const MAX_TRACKS: usize = 5000;
pub const POSITION_BUCKETS: usize = 64;

pub const PROTOCOL_ERROR: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;
pub const BACKEND_ERROR: i64 = -32000;
pub const TOO_MANY_OUTSTANDING: i64 = -32005;
pub const INCOMPATIBLE_PROTOCOL: i64 = -32010;

/// A structured JSON-RPC error that is safe to show to local clients.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Map<String, Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        RpcError {
            code,
            message: message.into(),
            data: Map::new(),
        }
    }

    /// `data` that is not an object (or is empty) is dropped, as Python's
    /// `data or {}` left only dicts in practice.
    pub fn with_data(code: i64, message: impl Into<String>, data: Value) -> Self {
        let mut e = Self::new(code, message);
        if let Value::Object(m) = data {
            e.data = m;
        }
        e
    }

    /// -32600 (Python `ProtocolError`).
    pub fn protocol(message: impl Into<String>) -> Self {
        Self::new(PROTOCOL_ERROR, message)
    }

    /// -32601 (Python `MethodError`).
    pub fn method(message: impl Into<String>) -> Self {
        Self::new(METHOD_NOT_FOUND, message)
    }

    /// -32602 (Python `ParamsError`).
    pub fn params(message: impl Into<String>) -> Self {
        Self::new(INVALID_PARAMS, message)
    }

    /// -32000 (Python `BackendError`).
    pub fn backend(message: impl Into<String>) -> Self {
        Self::new(BACKEND_ERROR, message)
    }

    /// Add one `data` entry (builder style).
    pub fn data(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.data.insert(key.to_string(), value.into());
        self
    }

    /// The server's mapping of a bad-input failure (Python ValueError,
    /// KeyError, TypeError): -32602 "invalid parameters" + redacted detail.
    pub fn invalid_parameters(detail: &str) -> Self {
        Self::params("invalid parameters").data("detail", bounded_detail(detail))
    }

    /// The server's mapping of any other failure: -32603 "internal error"
    /// + redacted detail.
    pub fn internal(detail: &str) -> Self {
        Self::new(INTERNAL_ERROR, "internal error").data("detail", bounded_detail(detail))
    }

    pub fn payload(&self) -> Value {
        let mut out = Map::new();
        out.insert("code".into(), Value::from(self.code));
        out.insert("message".into(), Value::from(self.message.clone()));
        if !self.data.is_empty() {
            out.insert("data".into(), Value::Object(self.data.clone()));
        }
        Value::Object(out)
    }

    /// The `path` entry of `data` as text (used by `serve()`'s messages).
    pub fn data_path(&self) -> Option<String> {
        match self.data.get("path") {
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            _ => None,
        }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RpcError {}

/// `redact_text(detail)[:200]` (200 characters, like a Python slice).
pub fn bounded_detail(detail: &str) -> String {
    crate::redact::redact_text(detail)
        .chars()
        .take(200)
        .collect()
}

type EnvFn<'a> = &'a dyn Fn(&str) -> Option<String>;

fn real_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn get_nonempty(env: EnvFn, name: &str) -> Option<String> {
    env(name).filter(|v| !v.is_empty())
}

fn home_with(env: EnvFn) -> PathBuf {
    match get_nonempty(env, "HOME") {
        Some(h) => PathBuf::from(h),
        None => passwd_home(None).unwrap_or_else(|| PathBuf::from("/")),
    }
}

fn passwd_home(user: Option<&str>) -> Option<PathBuf> {
    use std::ffi::{CStr, CString};
    let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
    let mut out: *mut libc::passwd = std::ptr::null_mut();
    let mut buf = vec![0 as libc::c_char; 16384];
    let rc = unsafe {
        match user {
            Some(u) => {
                let name = CString::new(u).ok()?;
                libc::getpwnam_r(
                    name.as_ptr(),
                    &mut pw,
                    buf.as_mut_ptr(),
                    buf.len(),
                    &mut out,
                )
            }
            None => libc::getpwuid_r(
                libc::getuid(),
                &mut pw,
                buf.as_mut_ptr(),
                buf.len(),
                &mut out,
            ),
        }
    };
    if rc != 0 || out.is_null() || pw.pw_dir.is_null() {
        return None;
    }
    let dir = unsafe { CStr::from_ptr(pw.pw_dir) };
    Some(PathBuf::from(dir.to_string_lossy().into_owned()))
}

fn expanduser_with(p: &str, env: EnvFn) -> PathBuf {
    if !p.starts_with('~') {
        return PathBuf::from(p);
    }
    let (head, rest) = match p.find('/') {
        Some(i) => (&p[..i], &p[i..]),
        None => (p, ""),
    };
    let base = if head == "~" {
        Some(home_with(env))
    } else {
        passwd_home(Some(&head[1..]))
    };
    match base {
        Some(b) => {
            let rest = rest.trim_start_matches('/');
            if rest.is_empty() {
                b
            } else {
                b.join(rest)
            }
        }
        None => PathBuf::from(p),
    }
}

/// Python `Path.expanduser()`: `~` and `~user` prefixes.
pub fn expanduser(p: &str) -> PathBuf {
    expanduser_with(p, &real_env)
}

fn runtime_dir_with(env: EnvFn) -> PathBuf {
    if let Some(base) = get_nonempty(env, "XDG_RUNTIME_DIR") {
        return PathBuf::from(base).join(SERVICE_NAME);
    }
    if let Some(state) = get_nonempty(env, "XDG_STATE_HOME") {
        return PathBuf::from(state).join(SERVICE_NAME).join("run");
    }
    home_with(env)
        .join(".local")
        .join("state")
        .join(SERVICE_NAME)
        .join("run")
}

fn socket_path_with(override_path: Option<&Path>, env: EnvFn) -> PathBuf {
    if let Some(p) = override_path {
        if !p.as_os_str().is_empty() {
            return expanduser_with(&p.to_string_lossy(), env);
        }
    }
    if let Some(v) = get_nonempty(env, "NPLAY_SERVICE_SOCKET") {
        return expanduser_with(&v, env);
    }
    runtime_dir_with(env).join("control.sock")
}

/// The private runtime directory used for the control socket.
pub fn runtime_dir() -> PathBuf {
    runtime_dir_with(&real_env)
}

/// `--service-socket` override, else `NPLAY_SERVICE_SOCKET`, else
/// `runtime_dir()/control.sock`.
pub fn socket_path(override_path: Option<&Path>) -> PathBuf {
    socket_path_with(override_path, &real_env)
}

/// `<socket>.lock` next to the socket (default socket when `None`).
pub fn lock_path(path: Option<&Path>) -> PathBuf {
    let p = match path {
        Some(p) => p.to_path_buf(),
        None => socket_path(None),
    };
    let mut name = p.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    p.with_file_name(name)
}

// -- encoding ---------------------------------------------------------------

fn write_str(out: &mut String, s: &str) {
    out.push('"');
    let mut start = 0;
    for (i, b) in s.bytes().enumerate() {
        let esc: Option<&str> = match b {
            b'"' => Some("\\\""),
            b'\\' => Some("\\\\"),
            b'\n' => Some("\\n"),
            b'\r' => Some("\\r"),
            b'\t' => Some("\\t"),
            0x08 => Some("\\b"),
            0x0c => Some("\\f"),
            0..=0x1f => None,
            _ => continue,
        };
        out.push_str(&s[start..i]);
        match esc {
            Some(e) => out.push_str(e),
            None => out.push_str(&format!("\\u{b:04x}")),
        }
        start = i + 1;
    }
    out.push_str(&s[start..]);
    out.push('"');
}

/// Python `float.__repr__` (what `json.dumps` writes for floats).
pub fn py_float_repr(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    // Rust's `{:e}` is the shortest round-trip form, like Python's repr.
    let sci = format!("{:e}", f.abs());
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let sign = if f.is_sign_negative() { "-" } else { "" };
    if (-4..16).contains(&exp) {
        let body = if exp >= 0 {
            let int_len = exp as usize + 1;
            if digits.len() <= int_len {
                format!("{}{}.0", digits, "0".repeat(int_len - digits.len()))
            } else {
                format!("{}.{}", &digits[..int_len], &digits[int_len..])
            }
        } else {
            format!("0.{}{}", "0".repeat((-exp - 1) as usize), digits)
        };
        format!("{sign}{body}")
    } else {
        let m = if digits.len() > 1 {
            format!("{}.{}", &digits[..1], &digits[1..])
        } else {
            digits
        };
        let es = if exp < 0 { '-' } else { '+' };
        format!("{sign}{m}e{es}{:02}", exp.abs())
    }
}

fn write_value(out: &mut String, v: &Value) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                out.push_str(&i.to_string());
            } else if let Some(u) = n.as_u64() {
                out.push_str(&u.to_string());
            } else {
                out.push_str(&py_float_repr(n.as_f64().unwrap_or(0.0)));
            }
        }
        Value::String(s) => write_str(out, s),
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(out, x);
            }
            out.push(']');
        }
        Value::Object(m) => {
            out.push('{');
            for (i, (k, x)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_str(out, k);
                out.push(':');
                write_value(out, x);
            }
            out.push('}');
        }
    }
}

/// Compact JSON exactly as Python `json.dumps(obj, ensure_ascii=False,
/// separators=(",", ":"))`.
pub fn dumps(v: &Value) -> String {
    let mut out = String::with_capacity(128);
    write_value(&mut out, v);
    out
}

/// Encode one frame (newline terminated, UTF-8).
pub fn encode(v: &Value) -> Vec<u8> {
    let mut s = dumps(v);
    s.push('\n');
    s.into_bytes()
}

pub fn response(req_id: Value, result: Value) -> Value {
    let mut m = Map::new();
    m.insert("jsonrpc".into(), Value::from("2.0"));
    m.insert("id".into(), req_id);
    m.insert("result".into(), result);
    Value::Object(m)
}

pub fn error_response(req_id: Value, err: &RpcError) -> Value {
    let mut m = Map::new();
    m.insert("jsonrpc".into(), Value::from("2.0"));
    m.insert("id".into(), req_id);
    m.insert("error".into(), err.payload());
    Value::Object(m)
}

pub fn notification(method: &str, params: Value) -> Value {
    let mut m = Map::new();
    m.insert("jsonrpc".into(), Value::from("2.0"));
    m.insert("method".into(), Value::from(method));
    m.insert("params".into(), params);
    Value::Object(m)
}

/// Validate an incoming request envelope without executing anything.
pub fn parse_request(line: &[u8]) -> Result<(Value, String, Map<String, Value>), RpcError> {
    if line.len() > MAX_FRAME {
        return Err(RpcError::protocol("frame too large").data("limit", MAX_FRAME as u64));
    }
    let value: Value =
        serde_json::from_slice(line).map_err(|_| RpcError::protocol("malformed JSON"))?;
    let Value::Object(mut obj) = value else {
        return Err(RpcError::protocol("request must be an object"));
    };
    if obj.get("jsonrpc") != Some(&Value::from("2.0")) {
        return Err(RpcError::protocol("jsonrpc must be 2.0"));
    }
    let method = match obj.get("method") {
        Some(Value::String(m)) if !m.is_empty() && m.chars().count() <= 128 => m.clone(),
        _ => return Err(RpcError::protocol("method must be a non-empty string")),
    };
    let params = match obj.remove("params") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(m)) => m,
        Some(_) => return Err(RpcError::params("params must be an object")),
    };
    let req_id = obj.remove("id").unwrap_or(Value::Null);
    match &req_id {
        Value::Null | Value::String(_) | Value::Bool(_) => {}
        Value::Number(n) if n.is_i64() || n.is_u64() => {}
        _ => return Err(RpcError::protocol("id must be a string or integer")),
    }
    Ok((req_id, method, params))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn floats_match_python_repr() {
        let cases = [
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.0, "1.0"),
            (100.0, "100.0"),
            (0.1, "0.1"),
            (1.5, "1.5"),
            (1e16, "1e+16"),
            (1.5e16, "1.5e+16"),
            (1e15, "1000000000000000.0"),
            (123456789.125, "123456789.125"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.5e-7, "1.5e-07"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (1763812345.678901, "1763812345.678901"),
            (-2.5, "-2.5"),
            (1e22, "1e+22"),
            (f64::NAN, "NaN"),
            (f64::INFINITY, "Infinity"),
            (f64::NEG_INFINITY, "-Infinity"),
        ];
        for (f, want) in cases {
            assert_eq!(py_float_repr(f), want, "{f:?}");
        }
    }

    #[test]
    fn encode_matches_python_json_dumps() {
        // Expected bytes recorded with Python 3: json.dumps(obj,
        // ensure_ascii=False, separators=(",", ":")) + "\n".
        let v = json!({"s": "a\"b\\c\n\r\t\u{8}\u{c}\u{1}\u{1f}\u{7f}åäö ☃ 𝄞 \u{2028}", "i": 3, "neg": -7,
                        "f": 2.0, "big": 18446744073709551615u64, "n": null, "b": [true, false], "e": {}, "a": []});
        let want = "{\"s\":\"a\\\"b\\\\c\\n\\r\\t\\b\\f\\u0001\\u001f\u{7f}åäö ☃ 𝄞 \u{2028}\",\"i\":3,\"neg\":-7,\"f\":2.0,\"big\":18446744073709551615,\"n\":null,\"b\":[true,false],\"e\":{},\"a\":[]}\n";
        assert_eq!(String::from_utf8(encode(&v)).unwrap(), want);
        // Key order is insertion order (Python dicts).
        let v = json!({"z": 1, "a": 2, "m": 3});
        assert_eq!(dumps(&v), "{\"z\":1,\"a\":2,\"m\":3}");
    }

    #[test]
    fn envelopes_and_payload_key_order() {
        assert_eq!(
            dumps(&response(json!(1), json!({"pong": 1.5}))),
            r#"{"jsonrpc":"2.0","id":1,"result":{"pong":1.5}}"#
        );
        assert_eq!(
            dumps(&error_response(
                Value::Null,
                &RpcError::protocol("malformed JSON")
            )),
            r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"malformed JSON"}}"#
        );
        let e = RpcError::with_data(
            -32010,
            "incompatible protocol version",
            json!({"server": 1, "client": [9]}),
        );
        assert_eq!(
            dumps(&error_response(json!(2), &e)),
            r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32010,"message":"incompatible protocol version","data":{"server":1,"client":[9]}}}"#
        );
        assert_eq!(
            dumps(&notification("state.changed", json!({"revision": 3}))),
            r#"{"jsonrpc":"2.0","method":"state.changed","params":{"revision":3}}"#
        );
        // Empty data is left out.
        assert_eq!(
            RpcError::with_data(-32000, "x", json!({})).payload(),
            json!({"code": -32000, "message": "x"})
        );
        let e = RpcError::invalid_parameters(&"x".repeat(500));
        assert_eq!(e.code, -32602);
        assert_eq!(e.message, "invalid parameters");
        assert_eq!(e.data["detail"].as_str().unwrap().len(), 200);
        let e = RpcError::internal("boom http://h/x?u=me&t=abc");
        assert_eq!(e.code, -32603);
        assert!(!e.data["detail"].as_str().unwrap().contains("abc"));
    }

    fn perr(line: &[u8]) -> (i64, String) {
        let e = parse_request(line).unwrap_err();
        (e.code, e.message)
    }

    #[test]
    fn parse_request_validation_messages() {
        assert_eq!(perr(b"not json"), (-32600, "malformed JSON".into()));
        assert_eq!(perr(&b"[".repeat(100)), (-32600, "malformed JSON".into()));
        assert_eq!(perr(b"\xff\xfe"), (-32600, "malformed JSON".into()));
        assert_eq!(perr(b"[1]"), (-32600, "request must be an object".into()));
        assert_eq!(
            perr(br#"{"method":"x"}"#),
            (-32600, "jsonrpc must be 2.0".into())
        );
        assert_eq!(
            perr(br#"{"jsonrpc":2.0,"method":"x"}"#),
            (-32600, "jsonrpc must be 2.0".into())
        );
        assert_eq!(
            perr(br#"{"jsonrpc":"2.0"}"#),
            (-32600, "method must be a non-empty string".into())
        );
        assert_eq!(
            perr(br#"{"jsonrpc":"2.0","method":""}"#),
            (-32600, "method must be a non-empty string".into())
        );
        let long = format!(r#"{{"jsonrpc":"2.0","method":"{}"}}"#, "m".repeat(129));
        assert_eq!(
            perr(long.as_bytes()),
            (-32600, "method must be a non-empty string".into())
        );
        let ok128 = format!(r#"{{"jsonrpc":"2.0","method":"{}"}}"#, "å".repeat(128));
        assert!(parse_request(ok128.as_bytes()).is_ok());
        assert_eq!(
            perr(br#"{"jsonrpc":"2.0","method":"x","params":[]}"#),
            (-32602, "params must be an object".into())
        );
        assert_eq!(
            perr(br#"{"jsonrpc":"2.0","method":"x","id":1.5}"#),
            (-32600, "id must be a string or integer".into())
        );
        assert_eq!(
            perr(br#"{"jsonrpc":"2.0","method":"x","id":[1]}"#),
            (-32600, "id must be a string or integer".into())
        );
        let big = vec![b' '; MAX_FRAME + 1];
        let e = parse_request(&big).unwrap_err();
        assert_eq!(
            e.payload(),
            json!({"code": -32600, "message": "frame too large", "data": {"limit": 1048576}})
        );

        let (id, m, p) =
            parse_request(br#" {"jsonrpc":"2.0","id":"a","method":"ping","params":null} "#)
                .unwrap();
        assert_eq!((id, m.as_str(), p.len()), (json!("a"), "ping", 0));
        let (id, _, p) =
            parse_request(br#"{"jsonrpc":"2.0","id":7,"method":"x","params":{"k":1}}"#).unwrap();
        assert_eq!((id, p["k"].clone()), (json!(7), json!(1)));
        let (id, _, _) = parse_request(br#"{"jsonrpc":"2.0","method":"x"}"#).unwrap();
        assert_eq!(id, Value::Null);
        // Python's isinstance(True, int) is True.
        let (id, _, _) = parse_request(br#"{"jsonrpc":"2.0","id":true,"method":"x"}"#).unwrap();
        assert_eq!(id, json!(true));
    }

    #[test]
    fn path_rules() {
        use std::collections::HashMap;
        let mut vars: HashMap<&str, &str> = HashMap::new();
        vars.insert("HOME", "/home/u");
        vars.insert("XDG_RUNTIME_DIR", "/run/user/7");
        vars.insert("XDG_STATE_HOME", "/st");
        let env = |vars: &HashMap<&str, &str>, k: &str| vars.get(k).map(|v| v.to_string());
        assert_eq!(
            socket_path_with(None, &|k| env(&vars, k)),
            PathBuf::from("/run/user/7/nplay/control.sock")
        );
        vars.insert("XDG_RUNTIME_DIR", "");
        assert_eq!(
            runtime_dir_with(&|k| env(&vars, k)),
            PathBuf::from("/st/nplay/run")
        );
        vars.remove("XDG_STATE_HOME");
        assert_eq!(
            runtime_dir_with(&|k| env(&vars, k)),
            PathBuf::from("/home/u/.local/state/nplay/run")
        );
        vars.insert("NPLAY_SERVICE_SOCKET", "~/x.sock");
        assert_eq!(
            socket_path_with(None, &|k| env(&vars, k)),
            PathBuf::from("/home/u/x.sock")
        );
        assert_eq!(
            socket_path_with(Some(Path::new("/o/c.sock")), &|k| env(&vars, k)),
            PathBuf::from("/o/c.sock")
        );
        assert_eq!(
            socket_path_with(Some(Path::new("")), &|k| env(&vars, k)),
            PathBuf::from("/home/u/x.sock")
        );
        assert_eq!(
            socket_path_with(Some(Path::new("~")), &|k| env(&vars, k)),
            PathBuf::from("/home/u")
        );
        assert_eq!(
            lock_path(Some(Path::new("/o/c.sock"))),
            PathBuf::from("/o/c.sock.lock")
        );
        assert_eq!(expanduser("a/~b"), PathBuf::from("a/~b"));
    }
}
