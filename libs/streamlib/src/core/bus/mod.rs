#[allow(clippy::module_inception)] // Common pattern: bus/bus.rs for Bus struct
pub mod bus;
pub mod connection;
pub mod connection_id;
pub mod connection_manager;
pub mod connections;
pub mod plugs;
pub mod ports;

pub use bus::Bus;
pub use connection::{create_owned_connection, OwnedConsumer, OwnedProducer};
pub use connection_id::{ConnectionId, ConnectionIdError};
pub use connection_manager::ConnectionManager;
pub use connections::{InputConnection, OutputConnection};
pub use plugs::{DisconnectedConsumer, DisconnectedProducer};
pub use ports::{PortAddress, PortMessage, PortType, StreamInput, StreamOutput};
