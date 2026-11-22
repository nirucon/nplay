//! One-shot, read-only query of a running NPLAY service (diagnostics only;
//! port of `nplay/service/probe.py`).
//!
//! Public API:
//! - `service_info(timeout, path) -> Option<Value>`: the running service's
//!   `service.info` result, or `None` when nothing answers.
//! - `vis_line(local_running, path) -> String`: the diagnostics line
//!   `VIS: CAVA, <n> subscriber(s) · terminal running|off · service ...`.

use super::protocol::socket_path;
use super::server::connect_timeout;
use serde_json::Value;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

/// Default timeout of `service_info` (Python's 0.3 s).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(300);

/// Return the running service's `service.info` result, or None.
pub fn service_info(timeout: Duration, path: Option<&Path>) -> Option<Value> {
    let mut s = connect_timeout(&socket_path(path), timeout).ok()?;
    s.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"service.info\",\"params\":{}}\n")
        .ok()?;
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; 65536];
    while !buf.contains(&b'\n') && buf.len() < 65536 {
        let n = s.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let line = match buf.iter().position(|b| *b == b'\n') {
        Some(i) => &buf[..i],
        None => &buf[..],
    };
    let msg: Value = serde_json::from_slice(line).ok()?;
    match msg.get("result") {
        None | Some(Value::Null) => None,
        Some(r) => Some(r.clone()),
    }
}

/// Python truthiness of a JSON value.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// The U3 diagnostics line: 'VIS: CAVA, n subscribers' (+ who runs CAVA).
pub fn vis_line(local_running: bool, path: Option<&Path>) -> String {
    vis_line_for(local_running, service_info(DEFAULT_TIMEOUT, path).as_ref())
}

fn vis_line_for(local_running: bool, info: Option<&Value>) -> String {
    let vis = if truthy(info) {
        info.and_then(|i| i.get("visualization"))
            .filter(|v| !v.is_null())
    } else {
        None
    };
    let mut parts = vec![format!(
        "terminal {}",
        if local_running { "running" } else { "off" }
    )];
    let subs = match vis {
        None => {
            parts.push(
                if info.is_none() {
                    "service not running"
                } else {
                    "service ≤1.7 (polling only)"
                }
                .to_string(),
            );
            0
        }
        Some(v) => {
            let subs = if truthy(v.get("subscribers")) {
                crate::model::value_i64(&v["subscribers"])
            } else {
                0
            };
            parts.push(format!(
                "service {}{}",
                if truthy(v.get("running")) {
                    "running"
                } else {
                    "off"
                },
                if truthy(v.get("polled_recently")) {
                    " · polled"
                } else {
                    ""
                }
            ));
            subs
        }
    };
    format!(
        "VIS: CAVA, {subs} subscriber{} · {}",
        if subs != 1 { "s" } else { "" },
        parts.join(" · ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn vis_line_variants() {
        assert_eq!(
            vis_line_for(false, None),
            "VIS: CAVA, 0 subscribers · terminal off · service not running"
        );
        assert_eq!(
            vis_line_for(true, Some(&json!({}))),
            "VIS: CAVA, 0 subscribers · terminal running · service ≤1.7 (polling only)"
        );
        assert_eq!(
            vis_line_for(true, Some(&json!({"version": "1.7.0"}))),
            "VIS: CAVA, 0 subscribers · terminal running · service ≤1.7 (polling only)"
        );
        assert_eq!(
            vis_line_for(
                true,
                Some(
                    &json!({"visualization": {"subscribers": 1, "running": true, "polled_recently": false}})
                )
            ),
            "VIS: CAVA, 1 subscriber · terminal running · service running"
        );
        assert_eq!(
            vis_line_for(
                false,
                Some(
                    &json!({"visualization": {"subscribers": 2, "running": false, "polled_recently": true}})
                )
            ),
            "VIS: CAVA, 2 subscribers · terminal off · service off · polled"
        );
    }

    #[test]
    fn missing_service_is_none() {
        let d = std::env::temp_dir().join(format!("nplay-probe-{}", fastrand::u64(..)));
        let p = d.join("none.sock");
        assert!(service_info(DEFAULT_TIMEOUT, Some(&p)).is_none());
        assert!(vis_line(false, Some(&p)).contains("service not running"));
    }
}
