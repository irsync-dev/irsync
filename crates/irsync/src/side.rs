//! Which end of a connection a process is: the client dials, the daemon
//! answers. Whether it sends or receives file data is its
//! [`Role`](crate::telemetry::Role), a separate thing.

use irsync_proto::capture::v1 as capture;

/// Which end of the connection this process is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Client,
    Daemon,
}

impl Side {
    /// `client` or `daemon`, as logs, metrics and `IRSYNC_NETSIM` write it.
    pub fn label(self) -> &'static str {
        match self {
            Side::Client => "client",
            Side::Daemon => "daemon",
        }
    }
}

impl From<Side> for capture::Side {
    fn from(side: Side) -> capture::Side {
        match side {
            Side::Client => capture::Side::Client,
            Side::Daemon => capture::Side::Daemon,
        }
    }
}
