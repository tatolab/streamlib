//! Test complete trait implementation generation
//!
//! This test validates that the macro can generate complete BaseProcessor
//! and Processor implementations when generate_impls = true.

use streamlib_macros::Processor;

// Mock types for testing (normally provided by streamlib crate)
mod mock {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct TestMessage {
        pub data: i32,
    }

    pub struct LinkInput<T> {
        _phantom: std::marker::PhantomData<T>,
    }

    impl<T> LinkInput<T> {
        pub fn new(_name: &str) -> Self {
            Self {
                _phantom: std::marker::PhantomData,
            }
        }

        pub fn add_consumer(&mut self, _consumer: crate::mock::LinkOwnedConsumer<T>) {}
    }

    pub struct LinkOutput<T> {
        _phantom: std::marker::PhantomData<T>,
    }

    impl<T> LinkOutput<T> {
        pub fn new(_name: &str) -> Self {
            Self {
                _phantom: std::marker::PhantomData,
            }
        }

        pub fn add_producer(&mut self, _producer: crate::mock::LinkOwnedProducer<T>) {}
    }

    pub struct LinkOwnedConsumer<T> {
        _phantom: std::marker::PhantomData<T>,
    }

    pub struct LinkOwnedProducer<T> {
        _phantom: std::marker::PhantomData<T>,
    }
}

// Simple transform processor for testing
#[derive(Processor)]
#[processor(generate_impls = true)]
struct SimpleTransform {
    #[input]
    input: mock::LinkInput<mock::TestMessage>,

    #[output]
    output: mock::LinkOutput<mock::TestMessage>,
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
        input: mock::LinkInput::new("input"),
        output: mock::LinkOutput::new("output"),
    };
}
