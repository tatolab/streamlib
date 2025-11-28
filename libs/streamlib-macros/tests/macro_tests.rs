//! Integration tests for Processor attribute macro
//!
//! NOTE: Full integration tests that verify macro-generated code compiles with streamlib
//! are located in the main streamlib crate's tests directory (attribute_macro_test.rs).
//!
//! These tests only cover internal parsing and analysis logic.

#[allow(unused_imports)]
use streamlib_macros::{config, input, output, processor};

// Processor macro tests require the full streamlib crate because the generated code
// references streamlib types. See libs/streamlib/tests/attribute_macro_test.rs for
// actual integration tests.
//
// This test file exists to ensure macro imports work.

#[test]
fn test_macros_can_be_imported() {
    // This just verifies the macros exist and can be imported
    // Actual functionality is tested in streamlib/tests/attribute_macro_test.rs
    assert!(true, "Macro imports successful");
}
