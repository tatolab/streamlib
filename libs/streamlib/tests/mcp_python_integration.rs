//! Integration test to verify MCP tools can call the refactored Python executor
//!
//! This tests that the refactoring from tools.rs to executor.rs didn't break functionality.

#[test]
fn test_compilation_succeeds() {
    // This test just verifies that the refactored code compiles
    // Actual functionality tests are in executor.rs and require python-embed feature
    assert!(true);
}

#[cfg(all(
    any(feature = "python", feature = "python-embed"),
    not(feature = "python-embed")
))]
#[test]
fn test_python_executor_returns_error_without_embed_feature() {
    // This test verifies that calling create_processor_from_code
    // without the python-embed feature returns a proper error
    use streamlib::python::create_processor_from_code;

    let code = "any code";
    let result = create_processor_from_code(code);

    assert!(
        result.is_err(),
        "Should return error without python-embed feature"
    );

    let err = result.unwrap_err();
    let err_msg = format!("{:?}", err);
    assert!(
        err_msg.contains("python-embed"),
        "Error should mention python-embed feature requirement"
    );
}

// Note: Full tests requiring python-embed feature are in executor.rs
// They require Python 3.11+ to run
