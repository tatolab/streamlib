#[allow(clippy::module_inception)] // Common pattern: bus/bus.rs for Bus struct
pub mod bus;
pub mod connection;
pub mod connection_manager;
pub mod ports;

pub use bus::Bus;
pub use connection::{create_owned_connection, ConnectionId, OwnedConsumer, OwnedProducer};
pub use connection_manager::ConnectionManager;
pub use ports::{PortAddress, PortMessage, PortType, StreamInput, StreamOutput};
