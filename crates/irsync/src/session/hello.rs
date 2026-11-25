//! The request a sync's `Hello` carries, read out of its oneof.

use irsync_proto::v1::{hello, BuildInfo, Hello, SyncRequest};
use irsync_proto::PROTOCOL_VERSION;

use crate::options::Direction;

/// A push or a pull as its `Hello` asks for it: the request out of the
/// Hello's oneof, with the direction its arm stands for.
#[derive(Debug, Clone)]
pub struct SyncHello {
    pub direction: Direction,
    pub req: SyncRequest,
    /// The client's build.
    pub build: Option<BuildInfo>,
    /// The client's W3C trace context; empty: no trace.
    pub traceparent: String,
    pub tracestate: String,
}

impl SyncHello {
    /// `req` from this build, with this build's chunking parameters and no
    /// trace context.
    pub fn new(direction: Direction, req: SyncRequest) -> SyncHello {
        let p = irsync_proto::chunk::CHUNK_PARAMS;
        SyncHello {
            direction,
            req: SyncRequest {
                chunk_min: p.min,
                chunk_avg: p.avg,
                chunk_max: p.max,
                ..req
            },
            build: Some(crate::build::BuildInfo::current().to_wire()),
            traceparent: String::new(),
            tracestate: String::new(),
        }
    }

    /// `hello`'s push or pull; `None` for any other request.
    pub fn from_hello(hello: Hello) -> Option<SyncHello> {
        let (direction, req) = match hello.request? {
            hello::Request::Push(r) => (Direction::ClientSends, r),
            hello::Request::Pull(r) => (Direction::DaemonSends, r),
            _ => return None,
        };
        Some(SyncHello {
            direction,
            req,
            build: hello.build,
            traceparent: hello.traceparent,
            tracestate: hello.tracestate,
        })
    }

    /// The `Hello` that asks for it.
    pub fn to_hello(&self) -> Hello {
        let req = self.req.clone();
        Hello {
            protocol_version: PROTOCOL_VERSION,
            build: self.build.clone(),
            traceparent: self.traceparent.clone(),
            tracestate: self.tracestate.clone(),
            request: Some(match self.direction {
                Direction::ClientSends => hello::Request::Push(req),
                Direction::DaemonSends => hello::Request::Pull(req),
            }),
        }
    }
}

impl From<SyncHello> for Hello {
    fn from(h: SyncHello) -> Hello {
        h.to_hello()
    }
}

/// A `Hello` from this build asking for `request`: this protocol version,
/// this build's info and no trace context.
pub fn hello(request: hello::Request) -> Hello {
    Hello {
        protocol_version: PROTOCOL_VERSION,
        build: Some(crate::build::BuildInfo::current().to_wire()),
        request: Some(request),
        ..Hello::default()
    }
}
