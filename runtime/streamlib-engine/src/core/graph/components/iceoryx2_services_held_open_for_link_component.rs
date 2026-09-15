// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::iceoryx2::{Iceoryx2NotifyService, Iceoryx2Service};

/// The iceoryx2 services a wired link holds open in the app process for as long
/// as the link stays in the graph.
///
/// iceoryx2 removes a service once no node holds it, and the next opener
/// re-creates it with whatever sizing that opener asks for. Held here, the
/// engine's own sizing outlives the op that opened it, so a helper opening its
/// end later — or first, on a link whose endpoints are both helpers — joins the
/// service the engine created rather than creating a smaller one. Every link of
/// a channel holds its own handle, so the services go with the channel's last
/// link.
pub struct Iceoryx2ServicesHeldOpenForLinkComponent {
    /// The data service of the link's source output port.
    pub channel_data_service: Iceoryx2Service,
    /// The destination's notify service, absent when the destination drains no
    /// listener.
    pub destination_notify_service: Option<Iceoryx2NotifyService>,
}
