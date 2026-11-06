//! Bounds on the session's queues.

/// Listing items a queue holds: the peer's listing on its way to the
/// planner, the receiver's own listing on its way to the planner, and a
/// listing on its way to the listing stream's writer. A full queue stalls
/// whoever fills it (the listing reader, and through QUIC flow control the
/// peer's walk; or this side's walk), never the control stream.
pub(super) const LISTING_QUEUE: usize = 1024;

/// Peer events a queue holds: control frames, and the listing reader's
/// start, end and failure signals, on their way to the role's main loop. A
/// full queue stops both stream readers, so through QUIC flow control a
/// peer that floods them waits for this side's main loop.
pub(super) const PEER_EVENTS: usize = 1024;
