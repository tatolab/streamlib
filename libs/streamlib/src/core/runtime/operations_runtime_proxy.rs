// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crossbeam_channel::Sender;

use super::commands::RuntimeCommand;
use super::RuntimeOperations;
use crate::core::graph::{LinkUniqueId, ProcessorUniqueId};
use crate::core::processors::ProcessorSpec;
use crate::core::{InputLinkPortRef, OutputLinkPortRef, Result, StreamError};

pub struct RuntimeProxy {
    command_tx: Sender<RuntimeCommand>,
}

impl RuntimeProxy {
    pub fn new(command_tx: Sender<RuntimeCommand>) -> Self {
        Self { command_tx }
    }

    fn send_and_recv<T>(
        &self,
        make_cmd: impl FnOnce(Sender<Result<T>>) -> RuntimeCommand,
    ) -> Result<T> {
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        let cmd = make_cmd(reply_tx);

        self.command_tx
            .send(cmd)
            .map_err(|_| StreamError::Runtime("Runtime command channel closed".into()))?;

        reply_rx
            .recv()
            .map_err(|_| StreamError::Runtime("Runtime reply channel closed".into()))?
    }
}

impl RuntimeOperations for RuntimeProxy {
    fn add_processor(&self, spec: ProcessorSpec) -> Result<ProcessorUniqueId> {
        self.send_and_recv(|reply| RuntimeCommand::AddProcessor { spec, reply })
    }

    fn remove_processor(&self, processor_id: &ProcessorUniqueId) -> Result<()> {
        self.send_and_recv(|reply| RuntimeCommand::RemoveProcessor {
            processor_id: processor_id.clone(),
            reply,
        })
    }

    fn connect(&self, from: OutputLinkPortRef, to: InputLinkPortRef) -> Result<LinkUniqueId> {
        self.send_and_recv(|reply| RuntimeCommand::Connect { from, to, reply })
    }

    fn disconnect(&self, link_id: &LinkUniqueId) -> Result<()> {
        self.send_and_recv(|reply| RuntimeCommand::Disconnect {
            link_id: link_id.clone(),
            reply,
        })
    }
}
