use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolVersion {
    pub major: u32,
    pub minor: u32,
}

pub const CURRENT_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion { major: 1, minor: 0 };

impl ProtocolVersion {
    pub fn is_compatible(&self, other: &ProtocolVersion) -> bool {
        self.major == other.major
    }
}
