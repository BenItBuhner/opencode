//! PTY slice: process-local registry, buffered output, single-use tickets,
//! and the WebSocket protocol framing shared with packages/core/src/pty.
//!
//! The submodules mirror packages/core/src/pty/{pty,protocol,ticket}.ts so the
//! Rust port and TS reference stay recognizable to one another.

pub mod protocol;
pub mod router;
pub mod session;
pub mod ticket;

pub use session::{CreateInput, Error, Event as PtyEvent, Registry, UpdateInput};
pub use ticket::{Registry as TicketRegistry, Scope};
