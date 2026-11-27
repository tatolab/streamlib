// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Why a page could not be opened, each refusal its own kind.

use tatolab_browser_tab_protocol::TabProtocolSocketFailure;

/// Why a page could not be opened, each refusal its own kind; each displays as its words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserPageRefused {
    /// The url, size, frame rate, sample rate or pool depth is one CEF cannot render with.
    NotRenderable(String),
    /// A port the node lent could not be opened.
    PortRefused(String),
    /// The runtime did not lease the machine's browser host.
    BrowserHostNotLeased(String),
    /// The browser host did not accept the connection, or did not answer the open.
    BrowserHostUnreachable(String),
    /// The browser host refused to open the page.
    PageRefusedByTheBrowserHost(String),
    /// The browser host answered outside the tab protocol, or the frame slots it handed over
    /// could not be taken.
    TabProtocolBroken(String),
    /// What the node sets up on its own side to open a page on, a socket or a port the host
    /// hands painted surfaces over, could not be set up; the host was not at fault.
    NodeSetUpNothingToOpenItOn(String),
    /// A thread the page needs could not start.
    ThreadNotStarted(String),
}

impl BrowserPageRefused {
    pub(crate) fn of_the_socket(failure: std::io::Error) -> Self {
        BrowserPageRefused::NodeSetUpNothingToOpenItOn(format!(
            "the socket to the browser host could not be set up: {failure}"
        ))
    }

    /// A tab protocol message that did not cross: one that could not be encoded or decoded, or
    /// came without its descriptor, broke the protocol; any other failure left the host
    /// unreachable.
    pub(crate) fn of_the_tab_protocol(
        failure: TabProtocolSocketFailure,
        what_failed: &str,
    ) -> Self {
        let why = format!("{what_failed}: {failure}");
        match failure {
            TabProtocolSocketFailure::Encoding(_) | TabProtocolSocketFailure::MissingDescriptor => {
                BrowserPageRefused::TabProtocolBroken(why)
            }
            TabProtocolSocketFailure::Ended | TabProtocolSocketFailure::Io(_) => {
                BrowserPageRefused::BrowserHostUnreachable(why)
            }
        }
    }
}

impl std::fmt::Display for BrowserPageRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BrowserPageRefused::NotRenderable(why)
            | BrowserPageRefused::PortRefused(why)
            | BrowserPageRefused::BrowserHostNotLeased(why)
            | BrowserPageRefused::BrowserHostUnreachable(why)
            | BrowserPageRefused::PageRefusedByTheBrowserHost(why)
            | BrowserPageRefused::TabProtocolBroken(why)
            | BrowserPageRefused::NodeSetUpNothingToOpenItOn(why)
            | BrowserPageRefused::ThreadNotStarted(why) => f.write_str(why),
        }
    }
}
