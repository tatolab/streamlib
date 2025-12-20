// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use super::commands::RuntimeCommand;
use super::StreamRuntime;

pub trait CommandReceiver {
    fn process_command(&self, cmd: RuntimeCommand);
}

impl CommandReceiver for StreamRuntime {
    fn process_command(&self, cmd: RuntimeCommand) {
        match cmd {
            RuntimeCommand::AddProcessor { spec, reply } => {
                let _ = reply.send(self.add_processor(spec));
            }
            RuntimeCommand::RemoveProcessor {
                processor_id,
                reply,
            } => {
                let _ = reply.send(self.remove_processor(&processor_id));
            }
            RuntimeCommand::Connect { from, to, reply } => {
                let _ = reply.send(self.connect(from, to));
            }
            RuntimeCommand::Disconnect { link_id, reply } => {
                let _ = reply.send(self.disconnect(&link_id));
            }
        }
    }
}
