// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Default link delegate implementation.

use crate::core::delegates::LinkDelegate;

/// Default implementation that does nothing.
pub struct DefaultLinkDelegate;

impl LinkDelegate for DefaultLinkDelegate {}

impl Default for DefaultLinkDelegate {
    fn default() -> Self {
        Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::delegates::LinkDelegate;
    use crate::core::graph::{Link, LinkUniqueId};

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct CountingLinkDelegate {
        wire_count: AtomicUsize,
        unwire_count: AtomicUsize,
    }

    impl CountingLinkDelegate {
        fn new() -> Self {
            Self {
                wire_count: AtomicUsize::new(0),
                unwire_count: AtomicUsize::new(0),
            }
        }
    }

    impl LinkDelegate for CountingLinkDelegate {
        fn will_wire(&self, _link: &Link) -> crate::core::Result<()> {
            self.wire_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn did_wire(&self, _link: &Link) -> crate::core::Result<()> {
            self.wire_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn will_unwire(&self, _link_id: &LinkUniqueId) -> crate::core::Result<()> {
            self.unwire_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn did_unwire(&self, _link_id: &LinkUniqueId) -> crate::core::Result<()> {
            self.unwire_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn test_default_link_delegate_does_nothing() {
        let delegate = DefaultLinkDelegate;
        let link = Link::new("source.output", "target.input");

        assert!(delegate.will_wire(&link).is_ok());
        assert!(delegate.did_wire(&link).is_ok());
        assert!(delegate.will_unwire(&link.id).is_ok());
        assert!(delegate.did_unwire(&link.id).is_ok());
    }

    #[test]
    fn test_counting_link_delegate() {
        let delegate = Arc::new(CountingLinkDelegate::new());
        let link = Link::new("source.output", "target.input");

        delegate.will_wire(&link).unwrap();
        delegate.did_wire(&link).unwrap();
        delegate.will_unwire(&link.id).unwrap();

        assert_eq!(delegate.wire_count.load(Ordering::SeqCst), 2);
        assert_eq!(delegate.unwire_count.load(Ordering::SeqCst), 1);
    }
}
