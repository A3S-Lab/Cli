use std::path::PathBuf;

/// Return a project-specific socket path derived from the canonical directory
/// that contains `config_path`. Two projects on the same machine get distinct
/// sockets so their daemons never interfere with each other.
pub fn socket_path(config_path: &std::path::Path) -> PathBuf {
    let dir = config_path.parent().unwrap_or(std::path::Path::new("."));
    let canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    // djb2 hash of the canonical path string
    let s = canonical.to_string_lossy();
    let mut h: u64 = 5381;
    for b in s.bytes() {
        h = h.wrapping_mul(33).wrapping_add(u64::from(b));
    }
    std::env::temp_dir().join(format!("a3s-{:08x}.sock", h as u32))
}

/// IPC request from client commands to the running daemon.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum IpcRequest {
    Status,
    Stop {
        services: Vec<String>,
    },
    Restart {
        service: String,
    },
    Logs {
        services: Vec<String>,
        follow: bool,
    },
    History {
        services: Vec<String>,
        lines: usize,
    },
    /// Reload A3sfile.hcl without restarting unchanged services.
    Reload,
}

/// IPC response from daemon to client.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcResponse {
    Status { rows: Vec<StatusRow> },
    Ok,
    Error { msg: String },
    LogLine { service: String, line: String, color_idx: usize },
    Reloaded {
        started: Vec<String>,
        stopped: Vec<String>,
        restarted: Vec<String>,
    },
    Stopped { services: Vec<String> },
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct StatusRow {
    pub name: String,
    pub state: String,
    pub pid: Option<u32>,
    pub port: u16,
    pub subdomain: Option<String>,
    pub uptime_secs: Option<u64>,
    pub proxy_port: u16,
    pub restart_count: u32,
    /// None = no health check configured; Some(true/false) = last check result.
    pub healthy: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_socket_path_differs_per_project() {
        let a = socket_path(std::path::Path::new("/tmp/proj_a/A3sfile.hcl"));
        let b = socket_path(std::path::Path::new("/tmp/proj_b/A3sfile.hcl"));
        assert_ne!(a, b, "different projects must get different socket paths");
    }

    #[test]
    fn test_socket_path_same_project_is_stable() {
        let a = socket_path(std::path::Path::new("/tmp/myproj/A3sfile.hcl"));
        let b = socket_path(std::path::Path::new("/tmp/myproj/A3sfile.hcl"));
        assert_eq!(a, b);
    }

    #[test]
    fn test_request_status_roundtrip() {
        let req = IpcRequest::Status;
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"cmd\":\"status\""));
        let _: IpcRequest = serde_json::from_str(&json).unwrap();
    }

    #[test]
    fn test_request_stop_roundtrip() {
        let req = IpcRequest::Stop {
            services: vec!["web".into(), "api".into()],
        };
        let json = serde_json::to_string(&req).unwrap();
        let decoded: IpcRequest = serde_json::from_str(&json).unwrap();
        if let IpcRequest::Stop { services } = decoded {
            assert_eq!(services, vec!["web", "api"]);
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn test_request_logs_roundtrip() {
        let req = IpcRequest::Logs {
            services: vec!["web".into()],
            follow: true,
        };
        let json = serde_json::to_string(&req).unwrap();
        let decoded: IpcRequest = serde_json::from_str(&json).unwrap();
        if let IpcRequest::Logs { services, follow } = decoded {
            assert_eq!(services, vec!["web".to_string()]);
            assert!(follow);
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn test_response_ok_roundtrip() {
        let resp = IpcResponse::Ok;
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"type\":\"ok\""));
        let _: IpcResponse = serde_json::from_str(&json).unwrap();
    }

    #[test]
    fn test_response_log_line_roundtrip() {
        let resp = IpcResponse::LogLine {
            service: "api".into(),
            line: "started".into(),
            color_idx: 3,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: IpcResponse = serde_json::from_str(&json).unwrap();
        if let IpcResponse::LogLine { service, line, color_idx } = decoded {
            assert_eq!(service, "api");
            assert_eq!(line, "started");
            assert_eq!(color_idx, 3);
        } else {
            panic!("wrong variant");
        }
    }
}
