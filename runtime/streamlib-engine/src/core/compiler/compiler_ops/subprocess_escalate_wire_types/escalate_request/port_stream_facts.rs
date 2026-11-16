// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

/// Read the current stream facts of the output port feeding one of the
/// helper's input ports. Reads no payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestReadInboundLinkStreamFacts {
    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// The helper's input port.
    pub(crate) port: String,

    /// The inbound link to read through, by the name a read reports it under.
    /// Absent on a port fed by one link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) inbound_link: Option<String>,
}

/// Replace the stream facts one of the helper's output ports carries. The
/// host refuses facts of another family than the port's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestSetOutputPortStreamFacts {
    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// The helper's output port.
    pub(crate) port: String,

    /// The facts, tagged by their family.
    pub(crate) stream_facts: streamlib_node_schema::PortStreamFacts,
}
