// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The config refusals the conversion nodes share, raised at setup.

use streamlib::sdk::error::{Error, Result};

/// Refuse `channel_names` that name no channel or name one twice, naming
/// `node_type_name`.
pub(crate) fn refuse_channel_names_naming_none_or_one_twice(
    node_type_name: &str,
    channel_names: &[String],
) -> Result<()> {
    if channel_names.is_empty() {
        return Err(Error::Configuration(format!(
            "{node_type_name}: `channel_names` names no channel; it names one per channel"
        )));
    }
    if let Some(named_twice) = channel_names
        .iter()
        .enumerate()
        .find(|(index, name)| channel_names[..*index].contains(name))
        .map(|(_, name)| name)
    {
        return Err(Error::Configuration(format!(
            "{node_type_name}: `channel_names` names {named_twice:?} twice; each channel has its \
             own name"
        )));
    }
    Ok(())
}

/// Refuse a Channels output's `channel_names` as
/// [`refuse_channel_names_naming_none_or_one_twice`] does, and a
/// `sample_rate` of 0.
pub(crate) fn refuse_a_channels_output_config(
    node_type_name: &str,
    channel_names: &[String],
    sample_rate: u32,
) -> Result<()> {
    refuse_channel_names_naming_none_or_one_twice(node_type_name, channel_names)?;
    if sample_rate == 0 {
        return Err(Error::Configuration(format!(
            "{node_type_name}: `sample_rate` is 0; it is the output's samples per second per \
             channel"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_names_naming_none_or_one_twice_are_refused() {
        let none = refuse_channel_names_naming_none_or_one_twice("Node", &[])
            .unwrap_err()
            .to_string();
        let twice = refuse_channel_names_naming_none_or_one_twice(
            "Node",
            &["gain".into(), "pan".into(), "gain".into()],
        )
        .unwrap_err()
        .to_string();

        assert!(none.contains("names no channel"), "{none}");
        assert!(twice.contains("\"gain\" twice"), "{twice}");
        assert!(
            refuse_channel_names_naming_none_or_one_twice("Node", &["gain".into(), "pan".into()])
                .is_ok()
        );
    }

    #[test]
    fn a_sample_rate_of_zero_is_refused() {
        let refusal = refuse_a_channels_output_config("Node", &["gain".into()], 0)
            .unwrap_err()
            .to_string();

        assert!(refusal.contains("`sample_rate` is 0"), "{refusal}");
    }
}
