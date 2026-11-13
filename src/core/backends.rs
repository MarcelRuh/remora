//! Wählt je Verbindung das passende Sitzungs-Backend (RDP oder SSH).

use std::sync::Arc;

use crate::database::Protocol;
use crate::rdp::{ConnectRequest, RdpBackend, RdpSession, SessionEventSink};

pub struct ProtocolBackends {
    rdp: Box<dyn RdpBackend>,
    ssh: Box<dyn RdpBackend>,
}

impl ProtocolBackends {
    pub fn new(rdp: Box<dyn RdpBackend>, ssh: Box<dyn RdpBackend>) -> Self {
        Self { rdp, ssh }
    }
}

impl RdpBackend for ProtocolBackends {
    fn name(&self) -> &'static str {
        "IronRDP + russh"
    }

    fn connect(&self, req: ConnectRequest, sink: Arc<dyn SessionEventSink>) -> anyhow::Result<Box<dyn RdpSession>> {
        match req.connection.protocol {
            Protocol::Rdp => self.rdp.connect(req, sink),
            Protocol::Ssh => self.ssh.connect(req, sink),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Connection;
    use crate::rdp::{InputEvent, SessionEvent, SessionId};
    use std::sync::Mutex;
    use std::time::Duration;

    struct Recorder(&'static str, Arc<Mutex<Vec<&'static str>>>);
    struct Null;
    impl RdpSession for Null {
        fn send_input(&self, _: InputEvent) {}
        fn resize(&self, _: u16, _: u16, _: u32) {}
        fn set_visible(&self, _: bool) {}
        fn disconnect(&self) {}
    }
    impl RdpBackend for Recorder {
        fn name(&self) -> &'static str {
            self.0
        }
        fn connect(&self, _: ConnectRequest, _: Arc<dyn SessionEventSink>) -> anyhow::Result<Box<dyn RdpSession>> {
            self.1.lock().unwrap().push(self.0);
            Ok(Box::new(Null))
        }
    }
    struct NullSink;
    impl SessionEventSink for NullSink {
        fn emit(&self, _: SessionId, _: SessionEvent) {}
    }

    #[test]
    fn dispatches_by_protocol() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let b = ProtocolBackends::new(
            Box::new(Recorder("rdp", log.clone())),
            Box::new(Recorder("ssh", log.clone())),
        );
        let req = |connection| ConnectRequest {
            session_id: 1,
            connection,
            password: None,
            gateway_password: None,
            desktop_width: 800,
            desktop_height: 600,
            scale_factor: 100,
            connect_timeout: Duration::from_secs(5),
            auto_reconnect_attempts: 0,
            terminal_font_size: 14.0,
        };
        b.connect(req(Connection::new("a", "h")), Arc::new(NullSink)).unwrap();
        b.connect(req(Connection::new_ssh("b", "h")), Arc::new(NullSink))
            .unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["rdp", "ssh"]);
    }
}
