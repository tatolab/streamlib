// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Output ports' catalog entries: the one check an entry passes, the live
//! entries a node's output ports carry, and the upstream entries its inbound
//! links reach.
//!
//! An entry is documentation a reader follows. No read or write path consults
//! it, `connect` compares nothing against it, and no payload is ever read for
//! it.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};

use crate::core::descriptors::{CatalogEntry, CatalogSection};
use crate::core::error::{Error, Result};
use crate::core::graph::{LinkUniqueId, PortInfo, cast_exposed_name_to_url_safe};
use crate::core::pubsub::OutputPortExposureChange;
use crate::iceoryx2::InboundLinkName;

/// The key under which hang's catalog lists a section's tracks by name:
/// `renditions` for video and audio, each data section's own map otherwise.
pub fn the_hang_map_a_catalog_section_lists_its_tracks_under(
    section: CatalogSection,
) -> &'static str {
    use hang::catalog::Section;

    match section {
        CatalogSection::Video | CatalogSection::Audio => "renditions",
        CatalogSection::Text => hang::catalog::Text::MAP,
        CatalogSection::Json => hang::catalog::Json::MAP,
        CatalogSection::Binary => hang::catalog::Binary::MAP,
    }
}

/// Refuse an entry hang's own catalog types do not read, naming the port and
/// the field hang refused.
pub fn refuse_a_catalog_entry_hang_does_not_read(
    port_name: &str,
    catalog_entry: &CatalogEntry,
) -> Result<()> {
    use hang::catalog::{AudioConfig, BinaryConfig, JsonConfig, TextConfig, VideoConfig};

    let entry = catalog_entry.entry.clone();
    let parsed = match catalog_entry.section {
        CatalogSection::Video => serde_json::from_value::<VideoConfig>(entry).map(drop),
        CatalogSection::Audio => serde_json::from_value::<AudioConfig>(entry).map(drop),
        CatalogSection::Text => serde_json::from_value::<TextConfig>(entry).map(drop),
        CatalogSection::Json => serde_json::from_value::<JsonConfig>(entry).map(drop),
        CatalogSection::Binary => serde_json::from_value::<BinaryConfig>(entry).map(drop),
    };
    parsed.map_err(|hang_refusal| Error::CatalogEntryRefused {
        port_name: port_name.to_string(),
        section: catalog_entry.section,
        refusal: hang_refusal.to_string(),
    })
}

/// The current catalog entry of each of one node's output ports, shared by the
/// node, the processor's writer or its helper's bridge, and every downstream
/// node an inbound link reaches it from.
#[derive(Debug, Default)]
pub struct CatalogEntriesOfOutputPorts {
    current_entry_by_output_port: RwLock<HashMap<String, Option<CatalogEntry>>>,
}

impl CatalogEntriesOfOutputPorts {
    /// Each output port with the entry it declared, or none.
    pub fn as_declared_by(output_ports: &[PortInfo]) -> Self {
        Self {
            current_entry_by_output_port: RwLock::new(
                output_ports
                    .iter()
                    .map(|port| (port.name.clone(), port.catalog_entry.clone()))
                    .collect(),
            ),
        }
    }

    /// The entry `output_port` carries now, or `None` — also for a port the
    /// node does not have.
    pub fn current_entry_of(&self, output_port: &str) -> Option<CatalogEntry> {
        let output_port = cast_exposed_name_to_url_safe(output_port).ok()?;
        self.current_entry_by_output_port
            .read()
            .get(output_port.as_ref())
            .cloned()
            .flatten()
    }

    /// Replace `output_port`'s entry, refusing a port the node does not declare
    /// and an entry hang does not read. A different entry is said on
    /// [`topics::OUTPUT_PORT_EXPOSURE`](crate::core::pubsub::topics::OUTPUT_PORT_EXPOSURE).
    pub fn replace_entry_of(&self, output_port: &str, catalog_entry: CatalogEntry) -> Result<()> {
        let output_port = cast_exposed_name_to_url_safe(output_port)?;
        refuse_a_catalog_entry_hang_does_not_read(&output_port, &catalog_entry)?;
        let mut current_entry_by_output_port = self.current_entry_by_output_port.write();
        let Some(current_entry) = current_entry_by_output_port.get_mut(output_port.as_ref()) else {
            let mut declared_output_ports: Vec<&str> = current_entry_by_output_port
                .keys()
                .map(String::as_str)
                .collect();
            declared_output_ports.sort_unstable();
            return Err(Error::Link(format!(
                "no output port `{output_port}` to carry a catalog entry — the node declares {}",
                listed_or_none(&declared_output_ports)
            )));
        };
        if current_entry.as_ref() == Some(&catalog_entry) {
            return Ok(());
        }
        *current_entry = Some(catalog_entry);
        drop(current_entry_by_output_port);
        OutputPortExposureChange::CatalogEntryReplaced {
            port_name: output_port.into_owned(),
        }
        .publish();
        Ok(())
    }
}

fn listed_or_none(names: &[&str]) -> String {
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join(", ")
    }
}

/// The output port one inbound link reads, and the entries of the node that
/// holds it.
#[derive(Debug)]
struct UpstreamOutputPortOfAnInboundLink {
    link_id: LinkUniqueId,
    inbound_link_name: InboundLinkName,
    upstream_output_port: String,
    upstream_catalog_entries: Arc<CatalogEntriesOfOutputPorts>,
}

/// For each of one node's input ports, the upstream output port of every
/// inbound link wired into it, in wiring order.
#[derive(Debug, Default)]
pub struct UpstreamCatalogEntriesByInboundLink {
    upstream_ports_by_input_port: Mutex<HashMap<String, Vec<UpstreamOutputPortOfAnInboundLink>>>,
}

impl UpstreamCatalogEntriesByInboundLink {
    /// Record link `link_id` into `input_port`, reading `upstream_output_port`
    /// of the node whose entries are `upstream_catalog_entries`.
    pub fn record_inbound_link(
        &self,
        input_port: &str,
        link_id: &LinkUniqueId,
        inbound_link_name: InboundLinkName,
        upstream_output_port: &str,
        upstream_catalog_entries: Arc<CatalogEntriesOfOutputPorts>,
    ) {
        let mut upstream_ports_by_input_port = self.upstream_ports_by_input_port.lock();
        let upstream_ports = upstream_ports_by_input_port
            .entry(input_port.to_string())
            .or_default();
        upstream_ports.retain(|upstream_port| &upstream_port.link_id != link_id);
        upstream_ports.push(UpstreamOutputPortOfAnInboundLink {
            link_id: link_id.clone(),
            inbound_link_name,
            upstream_output_port: upstream_output_port.to_string(),
            upstream_catalog_entries,
        });
    }

    /// Forget link `link_id`, wherever it was wired.
    pub fn forget_inbound_link(&self, link_id: &LinkUniqueId) {
        let mut upstream_ports_by_input_port = self.upstream_ports_by_input_port.lock();
        for upstream_ports in upstream_ports_by_input_port.values_mut() {
            upstream_ports.retain(|upstream_port| &upstream_port.link_id != link_id);
        }
        upstream_ports_by_input_port.retain(|_, upstream_ports| !upstream_ports.is_empty());
    }

    /// The current entry of the output port feeding `input_port`, or `None`
    /// when nothing feeds it or the upstream port carries none.
    ///
    /// A port fed by more than one link is asked about one of them by its
    /// `inbound_link` name; asked about none, it is refused naming them.
    pub fn catalog_entry_of_inbound_link(
        &self,
        input_port: &str,
        inbound_link: Option<&str>,
    ) -> Result<Option<CatalogEntry>> {
        let input_port = cast_exposed_name_to_url_safe(input_port)?;
        let upstream_ports_by_input_port = self.upstream_ports_by_input_port.lock();
        let upstream_ports = upstream_ports_by_input_port
            .get(input_port.as_ref())
            .map(Vec::as_slice)
            .unwrap_or_default();
        let inbound_link_names = || {
            listed_or_none(
                &upstream_ports
                    .iter()
                    .map(|upstream_port| upstream_port.inbound_link_name.as_str())
                    .collect::<Vec<_>>(),
            )
        };
        let upstream_port = match (inbound_link, upstream_ports) {
            (None, []) => return Ok(None),
            (None, [only_upstream_port]) => only_upstream_port,
            (None, _) => {
                return Err(Error::Link(format!(
                    "input port `{input_port}` is fed by {} links — name the one whose catalog \
                     entry to read: {}",
                    upstream_ports.len(),
                    inbound_link_names()
                )));
            }
            (Some(inbound_link), _) => upstream_ports
                .iter()
                .find(|upstream_port| upstream_port.inbound_link_name.as_str() == inbound_link)
                .ok_or_else(|| {
                    Error::Link(format!(
                        "input port `{input_port}` has no inbound link `{inbound_link}` — its \
                         links are: {}",
                        inbound_link_names()
                    ))
                })?,
        };
        Ok(upstream_port
            .upstream_catalog_entries
            .current_entry_of(&upstream_port.upstream_output_port))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::descriptors::{BinaryEntry, CatalogDataTrackMode, VideoEntry};

    fn msgpack_snapshot_entry() -> CatalogEntry {
        BinaryEntry {
            mode: CatalogDataTrackMode::Snapshot,
            mime: Some("application/msgpack".into()),
        }
        .into()
    }

    fn output_port_declaring(name: &str, catalog_entry: Option<CatalogEntry>) -> PortInfo {
        PortInfo {
            name: name.to_string(),
            description: String::new(),
            port_kind: Default::default(),
            delivery_profile: None,
            audio_window: None,
            catalog_entry,
        }
    }

    #[test]
    fn hang_reads_an_entry_of_every_section_carrying_the_fields_it_requires() {
        for catalog_entry in [
            CatalogEntry::from(VideoEntry {
                codec: "avc1.64001f".into(),
                coded_width: Some(1280),
                coded_height: Some(720),
                description: None,
            }),
            CatalogEntry {
                section: CatalogSection::Audio,
                entry: json!({"codec": "opus", "sampleRate": 48000, "numberOfChannels": 2}),
            },
            CatalogEntry {
                section: CatalogSection::Text,
                entry: json!({"format": "vtt"}),
            },
            CatalogEntry {
                section: CatalogSection::Json,
                entry: json!({"mode": "stream"}),
            },
            msgpack_snapshot_entry(),
        ] {
            refuse_a_catalog_entry_hang_does_not_read("port", &catalog_entry)
                .unwrap_or_else(|refusal| panic!("{catalog_entry:?} must parse: {refusal}"));
        }
    }

    #[test]
    fn an_entry_missing_a_field_hang_requires_is_refused_naming_the_port_and_the_field() {
        for (catalog_entry, missing_field) in [
            (
                CatalogEntry {
                    section: CatalogSection::Binary,
                    entry: json!({"mime": "application/msgpack"}),
                },
                "mode",
            ),
            (
                CatalogEntry {
                    section: CatalogSection::Audio,
                    entry: json!({"codec": "opus", "numberOfChannels": 2}),
                },
                "sampleRate",
            ),
            (
                CatalogEntry {
                    section: CatalogSection::Video,
                    entry: json!({"codedWidth": 1280}),
                },
                "codec",
            ),
        ] {
            let refusal =
                refuse_a_catalog_entry_hang_does_not_read("encoded_video", &catalog_entry)
                    .expect_err("an entry missing a required field is refused")
                    .to_string();
            assert!(
                refusal.contains("encoded_video") && refusal.contains(missing_field),
                "the refusal must name the port and `{missing_field}`; got {refusal}"
            );
        }
    }

    #[test]
    fn a_malformed_codec_string_is_refused() {
        let refusal = refuse_a_catalog_entry_hang_does_not_read(
            "video",
            &CatalogEntry {
                section: CatalogSection::Video,
                entry: json!({"codec": "avc1.zz"}),
            },
        )
        .expect_err("hang refuses an avc1 string it cannot read");
        assert!(refusal.to_string().contains("video"), "got {refusal}");
    }

    #[test]
    fn a_replaced_entry_is_the_one_read_and_a_refused_one_leaves_the_last() {
        let entries = CatalogEntriesOfOutputPorts::as_declared_by(&[
            output_port_declaring("video", Some(msgpack_snapshot_entry())),
            output_port_declaring("bare", None),
        ]);
        assert_eq!(
            entries.current_entry_of("video"),
            Some(msgpack_snapshot_entry())
        );
        assert_eq!(entries.current_entry_of("bare"), None);

        let encoded = CatalogEntry::from(VideoEntry {
            codec: "avc1.64001f".into(),
            coded_width: Some(1920),
            coded_height: Some(1080),
            description: None,
        });
        entries
            .replace_entry_of("video", encoded.clone())
            .expect("an entry hang reads replaces the declared one");
        assert_eq!(entries.current_entry_of("video"), Some(encoded.clone()));

        entries
            .replace_entry_of(
                "video",
                CatalogEntry {
                    section: CatalogSection::Binary,
                    entry: json!({}),
                },
            )
            .expect_err("an entry hang refuses is refused");
        assert_eq!(entries.current_entry_of("video"), Some(encoded));
    }

    #[test]
    fn an_entry_for_a_port_the_node_does_not_declare_is_refused_naming_the_ports_it_does() {
        let entries =
            CatalogEntriesOfOutputPorts::as_declared_by(&[output_port_declaring("video", None)]);
        let refusal = entries
            .replace_entry_of("audio", msgpack_snapshot_entry())
            .expect_err("a port the node does not declare carries no entry")
            .to_string();
        assert!(
            refusal.contains("`audio`") && refusal.contains("video"),
            "got {refusal}"
        );
    }

    #[test]
    fn a_reader_reads_the_current_entry_of_the_port_its_inbound_link_reads() {
        let upstream = Arc::new(CatalogEntriesOfOutputPorts::as_declared_by(&[
            output_port_declaring("video", Some(msgpack_snapshot_entry())),
        ]));
        let inbound = UpstreamCatalogEntriesByInboundLink::default();
        assert_eq!(
            inbound
                .catalog_entry_of_inbound_link("video_in", None)
                .expect("an unfed port answers"),
            None
        );

        inbound.record_inbound_link(
            "video_in",
            &LinkUniqueId::from("link-1"),
            InboundLinkName::from("camera/video"),
            "video",
            Arc::clone(&upstream),
        );
        assert_eq!(
            inbound
                .catalog_entry_of_inbound_link("video_in", None)
                .expect("a port fed by one link names it implicitly"),
            Some(msgpack_snapshot_entry())
        );

        let replaced = CatalogEntry {
            section: CatalogSection::Json,
            entry: json!({"mode": "stream"}),
        };
        upstream
            .replace_entry_of("video", replaced.clone())
            .expect("hang reads the replacement");
        assert_eq!(
            inbound
                .catalog_entry_of_inbound_link("video_in", Some("camera/video"))
                .expect("a named link answers"),
            Some(replaced),
            "the reader sees the upstream's current entry, not the one at wiring"
        );

        inbound.forget_inbound_link(&LinkUniqueId::from("link-1"));
        assert_eq!(
            inbound
                .catalog_entry_of_inbound_link("video_in", None)
                .expect("a port whose link went answers"),
            None
        );
    }

    #[test]
    fn a_port_fed_by_two_links_is_asked_about_one_by_name() {
        let first = Arc::new(CatalogEntriesOfOutputPorts::as_declared_by(&[
            output_port_declaring("video", Some(msgpack_snapshot_entry())),
        ]));
        let second = Arc::new(CatalogEntriesOfOutputPorts::as_declared_by(&[
            output_port_declaring("video", None),
        ]));
        let inbound = UpstreamCatalogEntriesByInboundLink::default();
        inbound.record_inbound_link(
            "tracks",
            &LinkUniqueId::from("link-1"),
            InboundLinkName::from("first/video"),
            "video",
            first,
        );
        inbound.record_inbound_link(
            "tracks",
            &LinkUniqueId::from("link-2"),
            InboundLinkName::from("second/video"),
            "video",
            second,
        );

        let refusal = inbound
            .catalog_entry_of_inbound_link("tracks", None)
            .expect_err("two links leave the entry ambiguous")
            .to_string();
        assert!(
            refusal.contains("first/video") && refusal.contains("second/video"),
            "the refusal must name both links; got {refusal}"
        );
        assert_eq!(
            inbound
                .catalog_entry_of_inbound_link("tracks", Some("first/video"))
                .expect("the first link answers"),
            Some(msgpack_snapshot_entry())
        );
        assert_eq!(
            inbound
                .catalog_entry_of_inbound_link("tracks", Some("second/video"))
                .expect("the second link answers"),
            None
        );
        inbound
            .catalog_entry_of_inbound_link("tracks", Some("third/video"))
            .expect_err("a link that does not feed the port is refused");
    }
}
