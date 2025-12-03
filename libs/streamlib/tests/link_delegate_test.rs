//! LinkDelegate Integration Tests
//!
//! Verifies that LinkDelegate hooks are called during link wiring and unwiring.

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use streamlib::core::delegates::LinkDelegate;
use streamlib::core::error::Result;
use streamlib::core::frames::AudioFrame;
use streamlib::core::graph::Link;
use streamlib::core::runtime::{CommitMode, RuntimeBuilder, StreamRuntime};
use streamlib::core::{LinkId, LinkInput, LinkOutput, RuntimeContext};

// =============================================================================
// Test Processors
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SourceConfig {
    pub name: String,
}

#[streamlib::processor(execution = Manual, description = "Test source processor", unsafe_send)]
pub struct SourceProcessor {
    #[streamlib::output(description = "Output")]
    output: Arc<LinkOutput<AudioFrame>>,

    #[streamlib::config]
    config: SourceConfig,
}

impl SourceProcessor::Processor {
    fn setup(&mut self, _ctx: &RuntimeContext) -> Result<()> {
        Ok(())
    }

    fn teardown(&mut self) -> Result<()> {
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TransformConfig {
    pub name: String,
}

#[streamlib::processor(execution = Reactive, description = "Test transform processor")]
pub struct TransformProcessor {
    #[streamlib::input(description = "Input")]
    input: LinkInput<AudioFrame>,

    #[streamlib::output(description = "Output")]
    output: Arc<LinkOutput<AudioFrame>>,

    #[streamlib::config]
    config: TransformConfig,
}

impl TransformProcessor::Processor {
    fn setup(&mut self, _ctx: &RuntimeContext) -> Result<()> {
        Ok(())
    }

    fn teardown(&mut self) -> Result<()> {
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        // Pass through
        if let Some(frame) = self.input.read() {
            self.output.write(frame);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SinkConfig {
    pub name: String,
}

#[streamlib::processor(execution = Continuous, description = "Test sink processor")]
pub struct SinkProcessor {
    #[streamlib::input(description = "Input")]
    input: LinkInput<AudioFrame>,

    #[streamlib::config]
    config: SinkConfig,
}

impl SinkProcessor::Processor {
    fn setup(&mut self, _ctx: &RuntimeContext) -> Result<()> {
        Ok(())
    }

    fn teardown(&mut self) -> Result<()> {
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        let _ = self.input.read();
        Ok(())
    }
}

// =============================================================================
// Counting Link Delegate
// =============================================================================

/// A delegate that counts how many times each hook is called.
struct CountingLinkDelegate {
    will_wire_count: AtomicUsize,
    did_wire_count: AtomicUsize,
    will_unwire_count: AtomicUsize,
    did_unwire_count: AtomicUsize,
    /// Records the link IDs that were wired (for verification)
    wired_links: Mutex<Vec<String>>,
    /// Records the link IDs that were unwired (for verification)
    unwired_links: Mutex<Vec<String>>,
}

impl CountingLinkDelegate {
    fn new() -> Self {
        Self {
            will_wire_count: AtomicUsize::new(0),
            did_wire_count: AtomicUsize::new(0),
            will_unwire_count: AtomicUsize::new(0),
            did_unwire_count: AtomicUsize::new(0),
            wired_links: Mutex::new(Vec::new()),
            unwired_links: Mutex::new(Vec::new()),
        }
    }

    fn will_wire_count(&self) -> usize {
        self.will_wire_count.load(Ordering::SeqCst)
    }

    fn did_wire_count(&self) -> usize {
        self.did_wire_count.load(Ordering::SeqCst)
    }

    fn will_unwire_count(&self) -> usize {
        self.will_unwire_count.load(Ordering::SeqCst)
    }

    fn did_unwire_count(&self) -> usize {
        self.did_unwire_count.load(Ordering::SeqCst)
    }
}

impl LinkDelegate for CountingLinkDelegate {
    fn will_wire(&self, link: &Link) -> Result<()> {
        self.will_wire_count.fetch_add(1, Ordering::SeqCst);
        self.wired_links.lock().push(link.id.to_string());
        Ok(())
    }

    fn did_wire(&self, link: &Link) -> Result<()> {
        self.did_wire_count.fetch_add(1, Ordering::SeqCst);
        // Verify the same link is passed to did_wire
        assert!(
            self.wired_links.lock().contains(&link.id.to_string()),
            "did_wire should receive the same link as will_wire"
        );
        Ok(())
    }

    fn will_unwire(&self, link_id: &LinkId) -> Result<()> {
        self.will_unwire_count.fetch_add(1, Ordering::SeqCst);
        self.unwired_links.lock().push(link_id.to_string());
        Ok(())
    }

    fn did_unwire(&self, link_id: &LinkId) -> Result<()> {
        self.did_unwire_count.fetch_add(1, Ordering::SeqCst);
        // Verify the same link_id is passed to did_unwire
        assert!(
            self.unwired_links.lock().contains(&link_id.to_string()),
            "did_unwire should receive the same link_id as will_unwire"
        );
        Ok(())
    }
}

// =============================================================================
// Tests
// =============================================================================

#[test]
#[serial]
fn test_link_delegate_wire_hooks_called_on_connect() {
    let delegate = Arc::new(CountingLinkDelegate::new());
    let delegate_dyn: Arc<dyn LinkDelegate> = Arc::clone(&delegate) as Arc<dyn LinkDelegate>;

    let mut runtime = RuntimeBuilder::new()
        .with_commit_mode(CommitMode::Manual)
        .with_link_delegate_arc(delegate_dyn)
        .build();

    // Add processors
    let source = runtime
        .add_processor::<SourceProcessor::Processor>(SourceConfig {
            name: "source".into(),
        })
        .expect("add source");

    let sink = runtime
        .add_processor::<SinkProcessor::Processor>(SinkConfig {
            name: "sink".into(),
        })
        .expect("add sink");

    // Connect - hooks called during commit
    let _link = runtime
        .connect(
            format!("{}.output", source.id),
            format!("{}.input", sink.id),
        )
        .expect("connect");

    // Before start, no hooks should be called
    assert_eq!(delegate.will_wire_count(), 0);
    assert_eq!(delegate.did_wire_count(), 0);

    // Start the runtime - this initializes context and enables compilation
    runtime.start().expect("start");

    // In manual mode, need to commit after start
    runtime.commit().expect("commit");

    // After commit, wire hooks should have been called
    assert_eq!(
        delegate.will_wire_count(),
        1,
        "will_wire should be called once"
    );
    assert_eq!(
        delegate.did_wire_count(),
        1,
        "did_wire should be called once"
    );

    // Unwire hooks should not have been called
    assert_eq!(delegate.will_unwire_count(), 0);
    assert_eq!(delegate.did_unwire_count(), 0);

    // Clean up
    runtime.stop().expect("stop");
}

#[test]
#[serial]
fn test_link_delegate_unwire_hooks_called_on_disconnect() {
    let delegate = Arc::new(CountingLinkDelegate::new());
    let delegate_dyn: Arc<dyn LinkDelegate> = Arc::clone(&delegate) as Arc<dyn LinkDelegate>;

    let mut runtime = RuntimeBuilder::new()
        .with_commit_mode(CommitMode::Manual)
        .with_link_delegate_arc(delegate_dyn)
        .build();

    // Add processors
    let source = runtime
        .add_processor::<SourceProcessor::Processor>(SourceConfig {
            name: "source".into(),
        })
        .expect("add source");

    let sink = runtime
        .add_processor::<SinkProcessor::Processor>(SinkConfig {
            name: "sink".into(),
        })
        .expect("add sink");

    // Connect
    let link = runtime
        .connect(
            format!("{}.output", source.id),
            format!("{}.input", sink.id),
        )
        .expect("connect");

    // Start runtime and commit
    runtime.start().expect("start");
    runtime.commit().expect("commit connect");

    // Wire hooks should have been called
    assert_eq!(delegate.will_wire_count(), 1);
    assert_eq!(delegate.did_wire_count(), 1);

    // Disconnect
    runtime.disconnect(&link).expect("disconnect");

    // Before commit, unwire hooks should not be called yet
    assert_eq!(delegate.will_unwire_count(), 0);
    assert_eq!(delegate.did_unwire_count(), 0);

    // Commit triggers unwiring
    runtime.commit().expect("commit disconnect");

    // After commit, unwire hooks should have been called
    assert_eq!(
        delegate.will_unwire_count(),
        1,
        "will_unwire should be called once"
    );
    assert_eq!(
        delegate.did_unwire_count(),
        1,
        "did_unwire should be called once"
    );

    // Clean up
    runtime.stop().expect("stop");
}

#[test]
#[serial]
fn test_link_delegate_multiple_links() {
    let delegate = Arc::new(CountingLinkDelegate::new());
    let delegate_dyn: Arc<dyn LinkDelegate> = Arc::clone(&delegate) as Arc<dyn LinkDelegate>;

    let mut runtime = RuntimeBuilder::new()
        .with_commit_mode(CommitMode::Manual)
        .with_link_delegate_arc(delegate_dyn)
        .build();

    // Add three processors for a chain: source -> transform -> sink
    let source = runtime
        .add_processor::<SourceProcessor::Processor>(SourceConfig {
            name: "source".into(),
        })
        .expect("add source");

    let transform = runtime
        .add_processor::<TransformProcessor::Processor>(TransformConfig {
            name: "transform".into(),
        })
        .expect("add transform");

    let sink = runtime
        .add_processor::<SinkProcessor::Processor>(SinkConfig {
            name: "sink".into(),
        })
        .expect("add sink");

    // Create two links
    let link1 = runtime
        .connect(
            format!("{}.output", source.id),
            format!("{}.input", transform.id),
        )
        .expect("connect 1");

    let link2 = runtime
        .connect(
            format!("{}.output", transform.id),
            format!("{}.input", sink.id),
        )
        .expect("connect 2");

    // Start runtime and commit
    runtime.start().expect("start");
    runtime.commit().expect("commit");

    // Both links should trigger hooks
    assert_eq!(
        delegate.will_wire_count(),
        2,
        "will_wire should be called twice"
    );
    assert_eq!(
        delegate.did_wire_count(),
        2,
        "did_wire should be called twice"
    );

    // Disconnect both links
    runtime.disconnect(&link1).expect("disconnect 1");
    runtime.disconnect(&link2).expect("disconnect 2");
    runtime.commit().expect("commit disconnect");

    // Both unwire hooks should be called
    assert_eq!(
        delegate.will_unwire_count(),
        2,
        "will_unwire should be called twice"
    );
    assert_eq!(
        delegate.did_unwire_count(),
        2,
        "did_unwire should be called twice"
    );

    // Clean up
    runtime.stop().expect("stop");
}

#[test]
#[serial]
fn test_default_link_delegate_works() {
    // Verify that the default delegate (no custom delegate) works fine
    let mut runtime = StreamRuntime::builder()
        .with_commit_mode(CommitMode::Manual)
        .build();

    let source = runtime
        .add_processor::<SourceProcessor::Processor>(SourceConfig {
            name: "source".into(),
        })
        .expect("add source");

    let sink = runtime
        .add_processor::<SinkProcessor::Processor>(SinkConfig {
            name: "sink".into(),
        })
        .expect("add sink");

    // Connect
    let link = runtime
        .connect(
            format!("{}.output", source.id),
            format!("{}.input", sink.id),
        )
        .expect("connect");

    // Start and commit should work without errors
    runtime.start().expect("start");
    runtime.commit().expect("commit connect");

    // Disconnect and commit should also work
    runtime.disconnect(&link).expect("disconnect");
    runtime.commit().expect("commit disconnect");

    // Clean up
    runtime.stop().expect("stop");
}
