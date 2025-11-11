//! Test complete trait implementation generation
//!
//! This test validates that the macro can generate complete StreamElement
//! and StreamProcessor implementations when generate_impls = true.

use streamlib_macros::StreamProcessor;

// Mock types for testing (normally provided by streamlib crate)
mod mock {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct TestMessage {
        pub data: i32,
    }

    pub struct StreamInput<T> {
        _phantom: std::marker::PhantomData<T>,
    }

    impl<T> StreamInput<T> {
        pub fn new(_name: &str) -> Self {
            Self {
                _phantom: std::marker::PhantomData,
            }
        }

        pub fn add_consumer(&mut self, _consumer: crate::mock::OwnedConsumer<T>) {}
    }

    pub struct StreamOutput<T> {
        _phantom: std::marker::PhantomData<T>,
    }

    impl<T> StreamOutput<T> {
        pub fn new(_name: &str) -> Self {
            Self {
                _phantom: std::marker::PhantomData,
            }
        }

        pub fn add_producer(&mut self, _producer: crate::mock::OwnedProducer<T>) {}
    }

    pub struct OwnedConsumer<T> {
        _phantom: std::marker::PhantomData<T>,
    }

    pub struct OwnedProducer<T> {
        _phantom: std::marker::PhantomData<T>,
    }
}

// Simple transform processor for testing
#[derive(StreamProcessor)]
#[processor(generate_impls = true)]
struct SimpleTransform {
    #[input]
    input: mock::StreamInput<mock::TestMessage>,

    #[output]
    output: mock::StreamOutput<mock::TestMessage>,
}

impl SimpleTransform {
    fn process(&mut self) -> Result<(), String> {
        // Business logic only
        Ok(())
    }
}

#[test]
fn test_macro_generates_code() {
    // This test just needs to compile to prove the macro works
    let _ = SimpleTransform {
        input: mock::StreamInput::new("input"),
        output: mock::StreamOutput::new("output"),
    };
}
