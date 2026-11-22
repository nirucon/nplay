//! Python-compatible coercion of JSON-RPC parameters (what the 1.x backend
//! got from `int(...)`, `float(...)`, `str(...)` and truthiness). Failures
//! map like the 1.x server mapped ValueError/TypeError: -32602
//! "invalid parameters" with a bounded, redacted detail.

use crate::service::protocol::{py_float_repr, RpcError};
use serde_json::{Map, Value};

/// Python truthiness of a JSON value.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `p.get(key) or <default>`: the value when present and truthy.
pub fn get_or<'a>(p: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    p.get(key).filter(|v| truthy(v))
}

/// `bool(p.get(key))`.
pub fn flag(p: &Map<String, Value>, key: &str) -> bool {
    p.get(key).is_some_and(truthy)
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// Python `repr()` of a JSON value (single-quoted strings, True/None).
pub fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => {
            let q = if s.contains('\'') && !s.contains('"') {
                '"'
            } else {
                '\''
            };
            let mut out = String::with_capacity(s.len() + 2);
            out.push(q);
            for c in s.chars() {
                match c {
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if c == q => {
                        out.push('\\');
                        out.push(c);
                    }
                    c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                        out.push_str(&format!("\\x{:02x}", c as u32))
                    }
                    c => out.push(c),
                }
            }
            out.push(q);
            out
        }
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| format!("{}: {}", py_repr(&Value::String(k.clone())), py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => py_str(other),
    }
}

/// Python `str()` of a JSON value.
pub fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else {
                py_float_repr(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// `str(p.get(key) or default)`.
pub fn str_or(p: &Map<String, Value>, key: &str, default: &str) -> String {
    get_or(p, key)
        .map(py_str)
        .unwrap_or_else(|| default.to_string())
}

fn bad(detail: String) -> RpcError {
    RpcError::invalid_parameters(&detail)
}

fn parse_int_text(s: &str) -> Option<i64> {
    let t = s.trim();
    let (neg, body) = match t.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    if body.is_empty()
        || body.starts_with('_')
        || body.ends_with('_')
        || body.contains("__")
        || !body.chars().all(|c| c.is_ascii_digit() || c == '_')
    {
        return None;
    }
    let digits: String = body.chars().filter(|c| *c != '_').collect();
    let n: i64 = digits.parse().ok()?;
    Some(if neg { -n } else { n })
}

/// Python `int(v)` (None when the key is absent counts as `int(None)`).
pub fn py_int(v: Option<&Value>) -> Result<i64, RpcError> {
    let v = v.unwrap_or(&Value::Null);
    match v {
        Value::Bool(b) => Ok(*b as i64),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i)
            } else {
                let f = n.as_f64().unwrap_or(0.0);
                if f.is_nan() {
                    Err(bad("cannot convert float NaN to integer".into()))
                } else if f.is_infinite() || f.abs() >= 9.2e18 {
                    Err(RpcError::internal(
                        "cannot convert float infinity to integer",
                    ))
                } else {
                    Ok(f.trunc() as i64)
                }
            }
        }
        Value::String(s) => parse_int_text(s).ok_or_else(|| {
            bad(format!(
                "invalid literal for int() with base 10: {}",
                py_repr(v)
            ))
        }),
        other => Err(bad(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            type_name(other)
        ))),
    }
}

/// `int(p.get(key) or default)`.
pub fn int_or(p: &Map<String, Value>, key: &str, default: i64) -> Result<i64, RpcError> {
    match get_or(p, key) {
        Some(v) => py_int(Some(v)),
        None => Ok(default),
    }
}

/// Python `float(v)`.
pub fn py_float(v: Option<&Value>) -> Result<f64, RpcError> {
    let v = v.unwrap_or(&Value::Null);
    match v {
        Value::Bool(b) => Ok(*b as i64 as f64),
        Value::Number(n) => Ok(n.as_f64().unwrap_or(0.0)),
        Value::String(s) => {
            let t = s.trim().replace('_', "");
            let low = t.to_ascii_lowercase();
            let ok = !t.is_empty()
                && (low.trim_start_matches(['+', '-']) == "inf"
                    || low.trim_start_matches(['+', '-']) == "infinity"
                    || low.trim_start_matches(['+', '-']) == "nan"
                    || t.chars().all(|c| c.is_ascii_digit() || "+-.eE".contains(c)));
            ok.then(|| t.parse::<f64>().ok())
                .flatten()
                .ok_or_else(|| bad(format!("could not convert string to float: {}", py_repr(v))))
        }
        other => Err(bad(format!(
            "float() argument must be a string or a real number, not '{}'",
            type_name(other)
        ))),
    }
}

/// `float(p.get(key) or default)`.
pub fn float_or(p: &Map<String, Value>, key: &str, default: f64) -> Result<f64, RpcError> {
    match get_or(p, key) {
        Some(v) => py_float(Some(v)),
        None => Ok(default),
    }
}

/// The first `n` characters (a Python slice `s[:n]`).
pub fn clip_chars(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

/// Python `ParamsError(message, data)` (-32602).
pub fn params_error(message: &str, data: Option<Value>) -> RpcError {
    match data {
        Some(d) => RpcError::with_data(crate::service::protocol::INVALID_PARAMS, message, d),
        None => RpcError::params(message),
    }
}

/// Python `BackendError(message, data)` (-32000).
pub fn backend_error(message: &str, data: Option<Value>) -> RpcError {
    match data {
        Some(d) => RpcError::with_data(crate::service::protocol::BACKEND_ERROR, message, d),
        None => RpcError::backend(message),
    }
}

/// An unexpected provider/database failure (Python's generic exception path).
pub fn internal(e: impl std::fmt::Display) -> RpcError {
    RpcError::internal(&e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn python_coercions() {
        assert_eq!(py_int(Some(&json!(3.9))).unwrap(), 3);
        assert_eq!(py_int(Some(&json!(" -12 "))).unwrap(), -12);
        assert_eq!(py_int(Some(&json!(true))).unwrap(), 1);
        let e = py_int(None).unwrap_err();
        assert_eq!(e.code, -32602);
        assert!(e.data["detail"].as_str().unwrap().contains("NoneType"));
        let e = py_int(Some(&json!("x"))).unwrap_err();
        assert_eq!(
            e.data["detail"],
            json!("invalid literal for int() with base 10: 'x'")
        );
        assert_eq!(py_float(Some(&json!("2.5"))).unwrap(), 2.5);
        assert!(py_float(Some(&json!("abc"))).is_err());
        assert_eq!(py_str(&json!(true)), "True");
        assert_eq!(py_str(&json!(2.0)), "2.0");
        assert_eq!(py_repr(&json!({"a": [1, null]})), "{'a': [1, None]}");
        assert!(!truthy(&json!(0)) && !truthy(&json!("")) && truthy(&json!([0])));
        assert_eq!(clip_chars("åäö", 2), "åä");
    }
}
