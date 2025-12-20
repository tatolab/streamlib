// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crossbeam_channel::Sender;

use crate::core::graph::{LinkUniqueId, ProcessorUniqueId};
use crate::core::processors::ProcessorSpec;
use crate::core::{InputLinkPortRef, OutputLinkPortRef, Result};

type Reply<T> = Sender<Result<T>>;

pub enum RuntimeCommand {
    AddProcessor {
        spec: ProcessorSpec,
        reply: Reply<ProcessorUniqueId>,
    },
    RemoveProcessor {
        processor_id: ProcessorUniqueId,
        reply: Reply<()>,
    },
    Connect {
        from: OutputLinkPortRef,
        to: InputLinkPortRef,
        reply: Reply<LinkUniqueId>,
    },
    Disconnect {
        link_id: LinkUniqueId,
        reply: Reply<()>,
    },
}
