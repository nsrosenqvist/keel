//! Translators from `keel-config` service declarations into the
//! `CustomEntry` shape that `crate::container::custom::CustomBackend`
//! consumes. Pure functions — no I/O, no async, no systemd
//! interrogation. Keeps the container crate config-free and the
//! config crate container-free; the runtime sits in the middle and
//! mediates.

use crate::config::{CustomService, SystemdScope, SystemdService};
use crate::container::custom::CustomEntry;

/// Translate a `[[services.custom]]` entry. The fallback for the
/// optional `restart` is `<stop> && <start>`, computed here so the
/// container crate sees a fully-formed entry.
pub fn from_custom(svc: &CustomService) -> CustomEntry {
    CustomEntry {
        name: svc.name.clone(),
        status_cmd: svc.status.clone(),
        start_cmd: svc.start.clone(),
        stop_cmd: svc.stop.clone(),
        restart_cmd: svc.restart_cmd().into_owned(),
        logs_cmd: svc.logs.clone(),
    }
}

/// Translate a `[[services.systemd]]` entry. Synthesises systemctl /
/// journalctl invocations driven by the unit name and scope. Both
/// scopes share the same verb shape; only the `--user` flag differs.
pub fn from_systemd(svc: &SystemdService) -> CustomEntry {
    let scope_flag = match svc.scope {
        SystemdScope::User => "--user ",
        SystemdScope::System => "",
    };
    let unit = &svc.unit;
    CustomEntry {
        name: svc.name.clone(),
        // `is-active` exits 0 when active, non-zero otherwise — exactly
        // the `CustomBackend::status` contract. Output goes to stdout
        // but we read only the exit code.
        status_cmd: format!("systemctl {scope_flag}is-active --quiet {unit}"),
        start_cmd: format!("systemctl {scope_flag}start {unit}"),
        stop_cmd: format!("systemctl {scope_flag}stop {unit}"),
        restart_cmd: format!("systemctl {scope_flag}restart {unit}"),
        logs_cmd: Some(format!(
            "journalctl {scope_flag}-u {unit} -f --output=short"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CustomService, SystemdScope, SystemdService};

    #[test]
    fn translates_minimal_custom() {
        let svc = CustomService {
            name: "ngrok".into(),
            desc: None,
            status: "pgrep -x ngrok".into(),
            start: "ngrok http 8080".into(),
            stop: "pkill -x ngrok".into(),
            restart: None,
            logs: None,
        };
        let entry = from_custom(&svc);
        assert_eq!(entry.name, "ngrok");
        assert_eq!(entry.status_cmd, "pgrep -x ngrok");
        // Restart synthesised from stop + start.
        assert_eq!(entry.restart_cmd, "pkill -x ngrok && ngrok http 8080");
        assert!(entry.logs_cmd.is_none());
    }

    #[test]
    fn translates_custom_with_explicit_restart_and_logs() {
        let svc = CustomService {
            name: "tunnel".into(),
            desc: None,
            status: "true".into(),
            start: "true".into(),
            stop: "true".into(),
            restart: Some("kill -HUP $(pgrep tunnel)".into()),
            logs: Some("tail -f /tmp/tunnel.log".into()),
        };
        let entry = from_custom(&svc);
        assert_eq!(entry.restart_cmd, "kill -HUP $(pgrep tunnel)");
        assert_eq!(entry.logs_cmd.as_deref(), Some("tail -f /tmp/tunnel.log"));
    }

    #[test]
    fn translates_systemd_user_scope() {
        let svc = SystemdService {
            name: "postgres".into(),
            desc: None,
            unit: "postgresql.service".into(),
            scope: SystemdScope::User,
        };
        let entry = from_systemd(&svc);
        assert_eq!(entry.name, "postgres");
        assert_eq!(
            entry.status_cmd,
            "systemctl --user is-active --quiet postgresql.service"
        );
        assert_eq!(entry.start_cmd, "systemctl --user start postgresql.service");
        assert_eq!(entry.stop_cmd, "systemctl --user stop postgresql.service");
        assert_eq!(
            entry.restart_cmd,
            "systemctl --user restart postgresql.service"
        );
        assert_eq!(
            entry.logs_cmd.as_deref(),
            Some("journalctl --user -u postgresql.service -f --output=short")
        );
    }

    #[test]
    fn translates_systemd_system_scope() {
        let svc = SystemdService {
            name: "shared-db".into(),
            desc: None,
            unit: "postgresql.service".into(),
            scope: SystemdScope::System,
        };
        let entry = from_systemd(&svc);
        // No `--user` flag for system scope.
        assert_eq!(
            entry.status_cmd,
            "systemctl is-active --quiet postgresql.service"
        );
        assert_eq!(entry.start_cmd, "systemctl start postgresql.service");
        assert_eq!(
            entry.logs_cmd.as_deref(),
            Some("journalctl -u postgresql.service -f --output=short")
        );
    }
}
