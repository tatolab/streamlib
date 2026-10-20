// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `mint_a_moqpro_subscribe_url <project_id> <broadcast> [relay_url]`, the
//! project's signing key on stdin: writes to stdout the URL a reader dials to
//! subscribe to `broadcast` alone for ten minutes, its token in the query.
//! The key and the URL are secrets, so neither is ever an argument.

use std::io::{Read, Write};
use std::process::ExitCode;
use std::time::Duration;

use streamlib_relay_adapter_moqpro::{
    MoqProProjectId, MoqProProjectSigningKey, MoqProRelayInstanceSettings,
};
use streamlib_relay_adapter_sdk::RelayDialCredential;
use streamlib_relay_adapter_sdk::secrecy::{ExposeSecret, SecretString};

const A_SUBSCRIBE_URL_IS_VALID_FOR: Duration = Duration::from_secs(10 * 60);

fn main() -> ExitCode {
    match write_the_subscribe_url() {
        Ok(()) => ExitCode::SUCCESS,
        Err(refusal) => {
            let _ = writeln!(std::io::stderr(), "mint_a_moqpro_subscribe_url: {refusal}");
            ExitCode::FAILURE
        }
    }
}

fn write_the_subscribe_url() -> Result<(), String> {
    let mut arguments = std::env::args().skip(1);
    let (Some(project_id), Some(broadcast), relay_url, None) = (
        arguments.next(),
        arguments.next(),
        arguments.next(),
        arguments.next(),
    ) else {
        return Err(
            "usage: mint_a_moqpro_subscribe_url <project_id> <broadcast> [relay_url], the \
             signing key on stdin"
                .to_string(),
        );
    };
    let mut signing_key = String::new();
    std::io::stdin()
        .read_to_string(&mut signing_key)
        .map_err(|failure| format!("the signing key could not be read from stdin: {failure}"))?;
    let settings = MoqProRelayInstanceSettings {
        project_id: MoqProProjectId::try_from(project_id).map_err(|refusal| refusal.to_string())?,
        signing_key: SecretString::from(signing_key).into(),
        relay_url,
    };
    let dial_url = settings
        .the_projects_dial_url()
        .map_err(|refusal| refusal.to_string())?;
    let token = MoqProProjectSigningKey::read_from(settings.signing_key.expose_secret())
        .and_then(|signing_key| {
            signing_key.mint_a_subscribe_only_token(
                &settings.project_id,
                &broadcast,
                A_SUBSCRIBE_URL_IS_VALID_FOR,
            )
        })
        .map_err(|refusal| refusal.to_string())?;
    let subscribe_url = RelayDialCredential::token_in_the_dial_url_query(token)
        .dial_url_carrying_the_credential(&dial_url);
    writeln!(std::io::stdout(), "{}", subscribe_url.expose_secret())
        .map_err(|failure| format!("the URL could not be written: {failure}"))
}
