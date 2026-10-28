// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A stream verb's stream argument resolved to the one stream id it means, through `list_streams`:
//! a stream id, a display name exactly one stream holds, or — omitted — the one stream recorded
//! from the working directory. Anything else is refused listing the candidates' ids and display
//! names, so a script or an agent can name one by its id. The tools only ever see ids.

use std::path::Path;

use streamlib_runtime_client_contract::local_api_wire_contract::{
    ListStreamsToolResult, ListStreamsToolResultStream,
};

use crate::TatolabCommandFailure;
use crate::stream_actions_on_the_runtime::{StreamActionTool, call_one_stream_action_tool};

/// The id of the one stream the runtime holds that `stream_argument` names, or the one recorded
/// from the working directory when it is omitted.
pub(crate) fn stream_id_the_stream_argument_names_on_the_running_runtime(
    stream_argument: Option<&str>,
) -> Result<String, TatolabCommandFailure> {
    let list_streams_result: ListStreamsToolResult =
        call_one_stream_action_tool(StreamActionTool::ListStreams, serde_json::Map::new())?;
    let resolved_working_directory = std::env::current_dir()
        .and_then(|working_directory| working_directory.canonicalize())
        .map_err(|io_failure| {
            TatolabCommandFailure::refused(format!(
                "cannot read the working directory: {io_failure}"
            ))
        })?;
    stream_id_the_stream_argument_names(
        &list_streams_result.streams,
        stream_argument,
        &resolved_working_directory,
    )
    .map_err(TatolabCommandFailure::refused)
}

/// The id of the one stream of `listed_streams` that `stream_argument` names — its id, else a
/// display name only it holds — or, when it is omitted or empty, the one whose project directory
/// is `resolved_working_directory`; else why not, listing the candidates.
pub(crate) fn stream_id_the_stream_argument_names(
    listed_streams: &[ListStreamsToolResultStream],
    stream_argument: Option<&str>,
    resolved_working_directory: &Path,
) -> Result<String, String> {
    match stream_argument.filter(|stream_argument| !stream_argument.is_empty()) {
        Some(stream_argument) => {
            stream_id_named_by_id_or_display_name(listed_streams, stream_argument)
        }
        None => stream_id_recorded_from_the_directory(listed_streams, resolved_working_directory),
    }
}

fn stream_id_named_by_id_or_display_name(
    listed_streams: &[ListStreamsToolResultStream],
    stream_argument: &str,
) -> Result<String, String> {
    if let Some(listed_stream) = listed_streams
        .iter()
        .find(|listed_stream| listed_stream.stream == stream_argument)
    {
        return Ok(listed_stream.stream.clone());
    }
    let holding_the_display_name: Vec<&ListStreamsToolResultStream> = listed_streams
        .iter()
        .filter(|listed_stream| listed_stream.display_name == stream_argument)
        .collect();
    match holding_the_display_name.as_slice() {
        [the_one] => Ok(the_one.stream.clone()),
        [] => Err(format!(
            "no stream in this runtime has the id or the display name \"{stream_argument}\"{}",
            every_stream_listed_as_a_candidate(listed_streams)
        )),
        several => Err(format!(
            "{} streams are shown as \"{stream_argument}\"; name one by its id:\n{}",
            several.len(),
            candidates_listed(several.iter().copied())
        )),
    }
}

fn stream_id_recorded_from_the_directory(
    listed_streams: &[ListStreamsToolResultStream],
    resolved_working_directory: &Path,
) -> Result<String, String> {
    let recorded_from_the_directory: Vec<&ListStreamsToolResultStream> = listed_streams
        .iter()
        .filter(|listed_stream| {
            listed_stream.location.as_ref().is_some_and(|location| {
                location.project_directory.as_path() == resolved_working_directory
            })
        })
        .collect();
    let directory = resolved_working_directory.display();
    match recorded_from_the_directory.as_slice() {
        [the_one] => Ok(the_one.stream.clone()),
        [] => Err(format!(
            "no stream in this runtime was loaded from {directory}; name one by its id or its \
             display name{}",
            every_stream_listed_as_a_candidate(listed_streams)
        )),
        several => Err(format!(
            "{} streams in this runtime were loaded from {directory}; name one by its id:\n{}",
            several.len(),
            candidates_listed(several.iter().copied())
        )),
    }
}

/// `:` and every stream the runtime holds, one per line, or that it holds none.
fn every_stream_listed_as_a_candidate(listed_streams: &[ListStreamsToolResultStream]) -> String {
    if listed_streams.is_empty() {
        return ". This runtime holds no stream.".to_owned();
    }
    format!(":\n{}", candidates_listed(listed_streams.iter()))
}

/// Each candidate on its own line: its id, then its display name in quotes.
fn candidates_listed<'listing>(
    candidates: impl Iterator<Item = &'listing ListStreamsToolResultStream>,
) -> String {
    candidates
        .map(|candidate| format!("  {}  \"{}\"", candidate.stream, candidate.display_name))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use streamlib_runtime_client_contract::local_api_wire_contract::{
        ListedStreamState, StreamLocationOnTheWire,
    };

    use super::*;

    fn listed(
        stream_id: &str,
        display_name: &str,
        project_directory: Option<&str>,
        state: ListedStreamState,
    ) -> ListStreamsToolResultStream {
        ListStreamsToolResultStream {
            stream: stream_id.to_owned(),
            display_name: display_name.to_owned(),
            location: project_directory.map(|project_directory| StreamLocationOnTheWire {
                project_directory: PathBuf::from(project_directory),
                stream_function: "stream:main".to_owned(),
            }),
            state,
            node_count: None,
            failed_because: None,
        }
    }

    fn the_machines_streams() -> Vec<ListStreamsToolResultStream> {
        vec![
            listed(
                "hello-main-3f2a9c",
                "Hello camera",
                Some("/srv/hello"),
                ListedStreamState::Kept,
            ),
            listed(
                "rig-front-0a1b2c",
                "camera",
                Some("/srv/rig"),
                ListedStreamState::Stopped,
            ),
            listed(
                "rig-back-9f8e7d",
                "camera",
                Some("/srv/rig"),
                ListedStreamState::Failed,
            ),
            listed("main", "main", None, ListedStreamState::Failed),
        ]
    }

    fn resolved(stream_argument: Option<&str>, working_directory: &str) -> Result<String, String> {
        stream_id_the_stream_argument_names(
            &the_machines_streams(),
            stream_argument,
            Path::new(working_directory),
        )
    }

    #[test]
    fn an_argument_naming_a_streams_id_resolves_to_it_wherever_the_command_runs() {
        assert_eq!(
            resolved(Some("rig-back-9f8e7d"), "/srv/hello"),
            Ok("rig-back-9f8e7d".to_owned())
        );
        assert_eq!(resolved(Some("main"), "/"), Ok("main".to_owned()));
    }

    #[test]
    fn an_argument_naming_a_display_name_one_stream_holds_resolves_to_that_streams_id() {
        assert_eq!(
            resolved(Some("Hello camera"), "/"),
            Ok("hello-main-3f2a9c".to_owned())
        );
    }

    #[test]
    fn an_id_wins_over_a_display_name_another_stream_holds() {
        let mut listed_streams = the_machines_streams();
        listed_streams.push(listed(
            "desk-main-112233",
            "hello-main-3f2a9c",
            Some("/srv/desk"),
            ListedStreamState::Kept,
        ));

        assert_eq!(
            stream_id_the_stream_argument_names(
                &listed_streams,
                Some("hello-main-3f2a9c"),
                Path::new("/")
            ),
            Ok("hello-main-3f2a9c".to_owned())
        );
    }

    #[test]
    fn a_display_name_several_streams_hold_is_refused_listing_each_by_id_and_display_name() {
        assert_eq!(
            resolved(Some("camera"), "/srv/hello"),
            Err("2 streams are shown as \"camera\"; name one by its id:\n  \
                 rig-front-0a1b2c  \"camera\"\n  rig-back-9f8e7d  \"camera\""
                .to_owned())
        );
    }

    #[test]
    fn an_argument_naming_no_stream_is_refused_listing_every_stream() {
        let refusal = resolved(Some("Hello"), "/srv/hello").unwrap_err();

        assert!(
            refusal.starts_with(
                "no stream in this runtime has the id or the display name \"Hello\":\n"
            ),
            "{refusal}"
        );
        for stream_id in [
            "hello-main-3f2a9c",
            "rig-front-0a1b2c",
            "rig-back-9f8e7d",
            "main",
        ] {
            assert!(refusal.contains(&format!("  {stream_id}  \"")), "{refusal}");
        }
    }

    #[test]
    fn an_omitted_argument_is_the_one_stream_recorded_from_the_working_directory() {
        assert_eq!(
            resolved(None, "/srv/hello"),
            Ok("hello-main-3f2a9c".to_owned())
        );
        assert_eq!(
            resolved(Some(""), "/srv/hello"),
            Ok("hello-main-3f2a9c".to_owned()),
            "an empty argument is an omitted one"
        );
    }

    #[test]
    fn an_omitted_argument_counts_stopped_and_failed_streams_of_the_directory() {
        let listed_streams = vec![listed(
            "rig-back-9f8e7d",
            "camera",
            Some("/srv/rig"),
            ListedStreamState::Failed,
        )];

        assert_eq!(
            stream_id_the_stream_argument_names(&listed_streams, None, Path::new("/srv/rig")),
            Ok("rig-back-9f8e7d".to_owned())
        );
    }

    #[test]
    fn a_directory_several_streams_were_loaded_from_is_refused_listing_them() {
        assert_eq!(
            resolved(None, "/srv/rig"),
            Err(
                "2 streams in this runtime were loaded from /srv/rig; name one by its id:\n  \
                 rig-front-0a1b2c  \"camera\"\n  rig-back-9f8e7d  \"camera\""
                    .to_owned()
            )
        );
    }

    #[test]
    fn a_directory_no_stream_was_loaded_from_is_refused_listing_every_stream() {
        let refusal = resolved(None, "/srv/hello/nodes").unwrap_err();

        assert!(
            refusal.starts_with(
                "no stream in this runtime was loaded from /srv/hello/nodes; name one by its id \
                 or its display name:\n"
            ),
            "a subdirectory of a project is not the project: {refusal}"
        );
        assert!(
            refusal.contains("  hello-main-3f2a9c  \"Hello camera\""),
            "{refusal}"
        );
    }

    #[test]
    fn a_runtime_holding_no_stream_says_so() {
        assert_eq!(
            stream_id_the_stream_argument_names(&[], None, Path::new("/srv/hello")),
            Err(
                "no stream in this runtime was loaded from /srv/hello; name one by its id or its \
                 display name. This runtime holds no stream."
                    .to_owned()
            )
        );
    }
}
