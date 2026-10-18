// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The `catalog = <Section>Entry { … }` key of an `output(...)` port.
//!
//! The entry is read at expansion, as literals, so the schema projection
//! carries it whole: `VideoEntry { codec: "avc1.64001f", coded_width: 1280 }`,
//! `BinaryEntry { mode: Snapshot, mime: "application/msgpack" }`. An optional
//! field takes a bare literal or `Some(<literal>)`; `mode` takes `Snapshot` or
//! `Stream`, bare or as `CatalogDataTrackMode::…`.

use std::collections::BTreeMap;

use proc_macro2::Span;
use streamlib_processor_schema::{
    AudioEntry, BinaryEntry, CatalogDataTrackMode, CatalogEntry, JsonEntry, TextEntry, VideoEntry,
};
use syn::spanned::Spanned;
use syn::{Expr, ExprStruct, Lit, Member};

/// The entry types the key takes, by the name an author writes.
const CATALOG_ENTRY_TYPE_NAMES: [&str; 5] = [
    "VideoEntry",
    "AudioEntry",
    "TextEntry",
    "JsonEntry",
    "BinaryEntry",
];

/// One field's literal, as written.
enum CatalogEntryFieldLiteral {
    Text(String),
    Count(u32),
    DataTrackMode(CatalogDataTrackMode),
}

/// The fields of one entry literal, each consumed as its type reads it.
struct CatalogEntryFieldsWritten {
    port_name: String,
    entry_type_name: String,
    entry_span: Span,
    fields: BTreeMap<String, (Span, CatalogEntryFieldLiteral)>,
}

impl CatalogEntryFieldsWritten {
    fn take(&mut self, field_name: &str) -> Option<(Span, CatalogEntryFieldLiteral)> {
        self.fields.remove(field_name)
    }

    fn optional_text(&mut self, field_name: &str) -> syn::Result<Option<String>> {
        match self.take(field_name) {
            None => Ok(None),
            Some((_, CatalogEntryFieldLiteral::Text(text))) => Ok(Some(text)),
            Some((span, _)) => Err(self.field_type_error(span, field_name, "a string literal")),
        }
    }

    fn optional_count(&mut self, field_name: &str) -> syn::Result<Option<u32>> {
        match self.take(field_name) {
            None => Ok(None),
            Some((_, CatalogEntryFieldLiteral::Count(count))) => Ok(Some(count)),
            Some((span, _)) => Err(self.field_type_error(span, field_name, "an integer literal")),
        }
    }

    fn required<T>(
        &mut self,
        field_name: &str,
        read: impl FnOnce(&mut Self, &str) -> syn::Result<Option<T>>,
    ) -> syn::Result<T> {
        read(self, field_name)?.ok_or_else(|| {
            syn::Error::new(
                self.entry_span,
                format!(
                    "output port `{}`: `{}` needs `{field_name}` — hang's catalog reads no \
                     entry without it",
                    self.port_name, self.entry_type_name
                ),
            )
        })
    }

    fn mode(&mut self) -> syn::Result<Option<CatalogDataTrackMode>> {
        match self.take("mode") {
            None => Ok(None),
            Some((_, CatalogEntryFieldLiteral::DataTrackMode(mode))) => Ok(Some(mode)),
            Some((span, _)) => Err(self.field_type_error(span, "mode", "`Snapshot` or `Stream`")),
        }
    }

    fn field_type_error(&self, span: Span, field_name: &str, expected: &str) -> syn::Error {
        syn::Error::new(
            span,
            format!(
                "output port `{}`: `{}.{field_name}` takes {expected}",
                self.port_name, self.entry_type_name
            ),
        )
    }

    /// Refuse a field the entry type does not have, naming the ones it does.
    fn refuse_any_field_left(self, fields_the_type_has: &[&str]) -> syn::Result<()> {
        match self.fields.into_iter().next() {
            None => Ok(()),
            Some((field_name, (span, _))) => Err(syn::Error::new(
                span,
                format!(
                    "output port `{}`: `{}` has no field `{field_name}` — it takes {}",
                    self.port_name,
                    self.entry_type_name,
                    fields_the_type_has
                        .iter()
                        .map(|name| format!("`{name}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )),
        }
    }
}

/// Read the expression after `catalog =` into the entry it declares.
pub(crate) fn parse_catalog_entry(
    entry_expression: &Expr,
    port_name: &str,
) -> syn::Result<CatalogEntry> {
    let Expr::Struct(entry_struct) = entry_expression else {
        return Err(syn::Error::new(
            entry_expression.span(),
            format!(
                "output port `{port_name}`: `catalog` takes an entry literal — one of {} — \
                 written with literal fields",
                rendered_entry_type_names()
            ),
        ));
    };
    let mut written = catalog_entry_fields_written(entry_struct, port_name)?;
    match written.entry_type_name.as_str() {
        "VideoEntry" => {
            let video = VideoEntry {
                codec: written.required("codec", CatalogEntryFieldsWritten::optional_text)?,
                coded_width: written.optional_count("coded_width")?,
                coded_height: written.optional_count("coded_height")?,
                description: written.optional_text("description")?,
            };
            written.refuse_any_field_left(&[
                "codec",
                "coded_width",
                "coded_height",
                "description",
            ])?;
            Ok(video.into())
        }
        "AudioEntry" => {
            let audio = AudioEntry {
                codec: written.required("codec", CatalogEntryFieldsWritten::optional_text)?,
                sample_rate: written
                    .required("sample_rate", CatalogEntryFieldsWritten::optional_count)?,
                number_of_channels: written.required(
                    "number_of_channels",
                    CatalogEntryFieldsWritten::optional_count,
                )?,
                description: written.optional_text("description")?,
            };
            written.refuse_any_field_left(&[
                "codec",
                "sample_rate",
                "number_of_channels",
                "description",
            ])?;
            Ok(audio.into())
        }
        "TextEntry" => {
            let text = TextEntry {
                format: written.required("format", CatalogEntryFieldsWritten::optional_text)?,
            };
            written.refuse_any_field_left(&["format"])?;
            Ok(text.into())
        }
        "JsonEntry" => {
            let json = JsonEntry {
                mode: written.required("mode", |written, _| written.mode())?,
            };
            written.refuse_any_field_left(&["mode"])?;
            Ok(json.into())
        }
        "BinaryEntry" => {
            let binary = BinaryEntry {
                mode: written.required("mode", |written, _| written.mode())?,
                mime: written.optional_text("mime")?,
            };
            written.refuse_any_field_left(&["mode", "mime"])?;
            Ok(binary.into())
        }
        other => Err(syn::Error::new(
            entry_struct.path.span(),
            format!(
                "output port `{port_name}`: `catalog` takes one of {}, not `{other}`",
                rendered_entry_type_names()
            ),
        )),
    }
}

fn rendered_entry_type_names() -> String {
    CATALOG_ENTRY_TYPE_NAMES
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn catalog_entry_fields_written(
    entry_struct: &ExprStruct,
    port_name: &str,
) -> syn::Result<CatalogEntryFieldsWritten> {
    let entry_type_name = entry_struct
        .path
        .segments
        .last()
        .map(|segment| segment.ident.to_string())
        .unwrap_or_default();
    if let Some(rest) = &entry_struct.rest {
        return Err(syn::Error::new(
            rest.span(),
            format!(
                "output port `{port_name}`: `{entry_type_name}` is read as literals, so it \
                 takes no `..` — leave an optional field out instead"
            ),
        ));
    }
    let mut fields = BTreeMap::new();
    for field in &entry_struct.fields {
        let Member::Named(field_ident) = &field.member else {
            return Err(syn::Error::new(
                field.member.span(),
                format!("output port `{port_name}`: `{entry_type_name}` fields are named"),
            ));
        };
        let field_name = field_ident.to_string();
        let literal = catalog_entry_field_literal(&field.expr, port_name, &entry_type_name)?;
        if fields
            .insert(field_name.clone(), (field.span(), literal))
            .is_some()
        {
            return Err(syn::Error::new(
                field.span(),
                format!(
                    "output port `{port_name}`: `{entry_type_name}` names `{field_name}` twice"
                ),
            ));
        }
    }
    Ok(CatalogEntryFieldsWritten {
        port_name: port_name.to_string(),
        entry_type_name,
        entry_span: entry_struct.span(),
        fields,
    })
}

fn catalog_entry_field_literal(
    field_expression: &Expr,
    port_name: &str,
    entry_type_name: &str,
) -> syn::Result<CatalogEntryFieldLiteral> {
    let not_a_literal = || {
        syn::Error::new(
            field_expression.span(),
            format!(
                "output port `{port_name}`: every `{entry_type_name}` field is a literal — a \
                 string, an integer, `Some(<literal>)`, or `Snapshot` / `Stream` for `mode`"
            ),
        )
    };
    match field_expression {
        Expr::Lit(literal) => match &literal.lit {
            Lit::Str(text) => Ok(CatalogEntryFieldLiteral::Text(text.value())),
            Lit::Int(count) => Ok(CatalogEntryFieldLiteral::Count(count.base10_parse()?)),
            _ => Err(not_a_literal()),
        },
        Expr::Call(call)
            if call.args.len() == 1
                && matches!(&*call.func, Expr::Path(path) if path.path.is_ident("Some")) =>
        {
            catalog_entry_field_literal(&call.args[0], port_name, entry_type_name)
        }
        Expr::Path(path) => match path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
            .as_deref()
        {
            Some("Snapshot") => Ok(CatalogEntryFieldLiteral::DataTrackMode(
                CatalogDataTrackMode::Snapshot,
            )),
            Some("Stream") => Ok(CatalogEntryFieldLiteral::DataTrackMode(
                CatalogDataTrackMode::Stream,
            )),
            _ => Err(not_a_literal()),
        },
        _ => Err(not_a_literal()),
    }
}
