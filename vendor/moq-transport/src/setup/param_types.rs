/// Setup Parameter Types
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u64)]
pub enum ParameterType {
    Path = 0x1,
    MaxRequestId = 0x2,
    AuthorizationToken = 0x3,
    MaxAuthTokenCacheSize = 0x4,
    Authority = 0x5,
    MOQTImplementation = 0x7,
}

impl From<ParameterType> for u64 {
    fn from(value: ParameterType) -> Self {
        value as u64
    }
}
