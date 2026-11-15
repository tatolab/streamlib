// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

/// Why the Slang compiler would not open or would not compile a source, in
/// words for the author.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlangShaderCompilerRefusal(pub(crate) String);

impl SlangShaderCompilerRefusal {
    /// The refusal's text.
    pub fn message(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SlangShaderCompilerRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SlangShaderCompilerRefusal {}
