use super::link::LinkDirection;
use super::link_port_ref::LinkPortRef;
use super::node::ProcessorNode;

/// Marker trait for output ports.
pub trait OutputPortMarker {
    const PORT_NAME: &'static str;
    type Processor;
}

/// Marker trait for input ports.
pub trait InputPortMarker {
    const PORT_NAME: &'static str;
    type Processor;
}

/// Create a [`LinkPortRef`] for an output port using compile-time validated marker types.
pub fn output<M: OutputPortMarker>(node: &ProcessorNode) -> LinkPortRef {
    LinkPortRef::output(node.id.clone(), M::PORT_NAME)
}

/// Create a [`LinkPortRef`] for an input port using compile-time validated marker types.
pub fn input<M: InputPortMarker>(node: &ProcessorNode) -> LinkPortRef {
    LinkPortRef::input(node.id.clone(), M::PORT_NAME)
}

/// Wrapper trait for [`LinkPortRef`] creation from marker types.
pub trait PortMarker {
    const PORT_NAME: &'static str;
    const DIRECTION: LinkDirection;
}

impl<M: OutputPortMarker> PortMarker for M {
    const PORT_NAME: &'static str = M::PORT_NAME;
    const DIRECTION: LinkDirection = LinkDirection::Output;
}

// Note: We can't implement PortMarker for InputPortMarker because of overlapping impls.
// The output<T>() and input<T>() functions handle this distinction instead.

#[cfg(test)]
mod tests {
    use super::*;

    // Mock processor for testing
    struct MockProcessor;

    // Mock output port marker
    struct MockVideoOutput;
    impl OutputPortMarker for MockVideoOutput {
        const PORT_NAME: &'static str = "video";
        type Processor = MockProcessor;
    }

    // Mock input port marker
    struct MockVideoInput;
    impl InputPortMarker for MockVideoInput {
        const PORT_NAME: &'static str = "video";
        type Processor = MockProcessor;
    }

    #[test]
    fn test_output_marker() {
        let node = ProcessorNode::new(
            "camera_0".to_string(),
            "MockProcessor".to_string(),
            None,
            vec![],
            vec![],
        );

        let port_ref = output::<MockVideoOutput>(&node);

        assert_eq!(port_ref.processor_id, "camera_0");
        assert_eq!(port_ref.port_name, "video");
        assert!(port_ref.is_output());
    }

    #[test]
    fn test_input_marker() {
        let node = ProcessorNode::new(
            "display_0".to_string(),
            "MockProcessor".to_string(),
            None,
            vec![],
            vec![],
        );

        let port_ref = input::<MockVideoInput>(&node);

        assert_eq!(port_ref.processor_id, "display_0");
        assert_eq!(port_ref.port_name, "video");
        assert!(port_ref.is_input());
    }
}
