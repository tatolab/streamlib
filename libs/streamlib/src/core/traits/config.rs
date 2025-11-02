/// Empty configuration for processors that don't need configuration
///
/// This is a generic config type used by processors that have no
/// configurable parameters (e.g., simple passthroughs, mock processors).
#[derive(Debug, Clone, Default)]
pub struct EmptyConfig;
