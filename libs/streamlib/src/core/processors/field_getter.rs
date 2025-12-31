// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! FieldGetter processor for extracting a single field from a DataFrame.

use crate::core::schema::{
    DataFrameSchema, DataFrameSchemaDescriptor, DataFrameSchemaField, DynamicDataFrameSchema,
    PortDescriptor, PrimitiveType, ProcessorDescriptor,
};
use crate::core::{DataFrame, LinkInput, LinkOutput, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Configuration for the FieldGetter processor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldGetterConfig {
    /// The field to extract from the source DataFrame.
    pub source_field: DataFrameSchemaField,
}

impl Default for FieldGetterConfig {
    fn default() -> Self {
        Self {
            source_field: DataFrameSchemaField {
                name: "value".to_string(),
                primitive: PrimitiveType::F32,
                shape: vec![],
            },
        }
    }
}

#[crate::processor(
    execution = Reactive,
    description = "Extracts a single field from a DataFrame",
    descriptor_fn = "build_descriptor"
)]
pub struct FieldGetterProcessor {
    #[crate::input(description = "Source DataFrame")]
    input: LinkInput<DataFrame>,

    #[crate::output(description = "Extracted field as DataFrame")]
    output: LinkOutput<DataFrame>,

    #[crate::config]
    config: FieldGetterConfig,
}

impl crate::core::ReactiveProcessor for FieldGetterProcessor::Processor {
    fn process(&mut self) -> Result<()> {
        let output_schema = self.config.source_field.to_primitive_schema();

        while let Some(input_frame) = self.input.read() {
            // Get the source field data
            let (offset, size) = input_frame
                .schema
                .field_layout(&self.config.source_field.name)
                .ok_or_else(|| {
                    crate::core::StreamError::Config(format!(
                        "Field '{}' not found in input DataFrame schema",
                        self.config.source_field.name
                    ))
                })?;

            // Create output frame with just this field
            let mut output_frame = DataFrame::new(
                Arc::clone(&output_schema) as Arc<dyn DataFrameSchema>,
                input_frame.timestamp_ns,
            );

            // Copy the field data
            output_frame.data[..size].copy_from_slice(&input_frame.data[offset..offset + size]);

            self.output.write(output_frame);
        }

        Ok(())
    }
}

impl FieldGetterProcessor::Processor {
    /// Builds a dynamic descriptor with config-dependent output schema.
    fn build_descriptor(&self) -> Option<ProcessorDescriptor> {
        let output_schema = self.config.source_field.to_primitive_schema();

        Some(
            ProcessorDescriptor::new("FieldGetter", "Extracts a single field from a DataFrame")
                .with_input(PortDescriptor {
                    name: "input".to_string(),
                    schema: Arc::clone(&crate::core::SCHEMA_DATA_MESSAGE),
                    required: true,
                    description: "Source DataFrame".to_string(),
                    dataframe_schema: None, // Input accepts any DataFrame
                })
                .with_output(PortDescriptor {
                    name: "output".to_string(),
                    schema: Arc::clone(&crate::core::SCHEMA_DATA_MESSAGE),
                    required: true,
                    description: "Extracted field as DataFrame".to_string(),
                    dataframe_schema: Some(DataFrameSchemaDescriptor::from_schema(
                        output_schema.as_ref(),
                    )),
                }),
        )
    }

    /// Returns the output schema based on current config.
    pub fn output_schema(&self) -> Arc<DynamicDataFrameSchema> {
        self.config.source_field.to_primitive_schema()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let config = FieldGetterConfig::default();
        assert_eq!(config.source_field.name, "value");
        assert_eq!(config.source_field.primitive, PrimitiveType::F32);
        assert!(config.source_field.shape.is_empty());
    }

    #[test]
    fn test_config_custom() {
        let config = FieldGetterConfig {
            source_field: DataFrameSchemaField {
                name: "embedding".to_string(),
                primitive: PrimitiveType::F32,
                shape: vec![512],
            },
        };
        assert_eq!(config.source_field.name, "embedding");
        assert_eq!(config.source_field.shape, vec![512]);
    }
}
