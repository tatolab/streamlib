use rtrb::{Producer, Consumer, RingBuffer};
use std::sync::Arc;
use parking_lot::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionId(pub u64);

impl ConnectionId {
    pub fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        Self(COUNTER.fetch_add(1, Ordering::Relaxed))
    }
}

impl std::fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "conn_{}", self.0)
    }
}

pub struct ProcessorConnection<T: Clone + Send + 'static> {
    pub id: ConnectionId,
    pub source_processor: String,
    pub source_port: String,
    pub dest_processor: String,
    pub dest_port: String,
    pub producer: Arc<Mutex<Producer<T>>>,
    pub consumer: Arc<Mutex<Consumer<T>>>,
    pub created_at: std::time::Instant,
}

impl<T: Clone + Send + 'static> ProcessorConnection<T> {
    pub fn new(
        source_processor: String,
        source_port: String,
        dest_processor: String,
        dest_port: String,
        capacity: usize,
    ) -> Self {
        let (producer, consumer) = RingBuffer::new(capacity);

        Self {
            id: ConnectionId::new(),
            source_processor,
            source_port,
            dest_processor,
            dest_port,
            producer: Arc::new(Mutex::new(producer)),
            consumer: Arc::new(Mutex::new(consumer)),
            created_at: std::time::Instant::now(),
        }
    }

    /// Write data to the connection with roll-off semantics
    /// Always succeeds by dropping oldest data when buffer is full
    pub fn write(&self, data: T) {
        let mut producer = self.producer.lock();

        // Try to push
        if let Err(rtrb::PushError::Full(_)) = producer.push(data.clone()) {
            // Buffer is full - pop oldest from consumer side to make room
            let _dropped = self.consumer.lock().pop();

            // Retry push - should succeed now
            if let Err(e) = producer.push(data) {
                // This should never happen, but log if it does
                tracing::error!(
                    "Failed to write to connection {} even after making space: {:?}",
                    self.id,
                    e
                );
            }
        }
    }

    /// Legacy write method that returns errors (deprecated)
    #[deprecated(note = "Use write() instead - it now always succeeds with roll-off semantics")]
    pub fn try_write(&self, data: T) -> Result<(), T> {
        let mut producer = self.producer.lock();
        match producer.push(data) {
            Ok(()) => Ok(()),
            Err(rtrb::PushError::Full(data)) => Err(data),
        }
    }

    pub fn read_latest(&self) -> Option<T> {
        let mut consumer = self.consumer.lock();
        let mut latest = None;
        while let Ok(data) = consumer.pop() {
            latest = Some(data);
        }
        latest
    }

    pub fn has_data(&self) -> bool {
        !self.consumer.lock().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_write_roll_off() {
        let conn = ProcessorConnection::<i32>::new(
            "proc1".to_string(),
            "out".to_string(),
            "proc2".to_string(),
            "in".to_string(),
            3, // Small buffer for testing
        );

        // Fill the buffer
        conn.write(1);
        conn.write(2);
        conn.write(3);

        // This should cause roll-off (oldest data drops)
        conn.write(4);

        // Read all data - should get 2, 3, 4 (1 was dropped)
        let mut consumer = conn.consumer.lock();
        assert_eq!(consumer.pop(), Ok(2));
        assert_eq!(consumer.pop(), Ok(3));
        assert_eq!(consumer.pop(), Ok(4));
        assert!(consumer.pop().is_err()); // Buffer empty
    }

    #[test]
    fn test_write_never_blocks() {
        let conn = ProcessorConnection::<i32>::new(
            "proc1".to_string(),
            "out".to_string(),
            "proc2".to_string(),
            "in".to_string(),
            2,
        );

        // Write many items - should never panic or block
        for i in 0..100 {
            conn.write(i); // Always succeeds
        }

        // Should have latest 2 items
        assert_eq!(conn.read_latest(), Some(99));
    }

    #[test]
    fn test_read_latest_gets_newest() {
        let conn = ProcessorConnection::<i32>::new(
            "proc1".to_string(),
            "out".to_string(),
            "proc2".to_string(),
            "in".to_string(),
            5,
        );

        conn.write(10);
        conn.write(20);
        conn.write(30);

        // read_latest should drain all and return newest
        assert_eq!(conn.read_latest(), Some(30));

        // Buffer should be empty now
        assert_eq!(conn.read_latest(), None);
    }

    #[test]
    fn test_has_data() {
        let conn = ProcessorConnection::<i32>::new(
            "proc1".to_string(),
            "out".to_string(),
            "proc2".to_string(),
            "in".to_string(),
            3,
        );

        assert!(!conn.has_data());

        conn.write(42);
        assert!(conn.has_data());

        conn.read_latest();
        assert!(!conn.has_data());
    }

    #[test]
    fn test_connection_id_unique() {
        let conn1 = ProcessorConnection::<i32>::new(
            "proc1".to_string(),
            "out".to_string(),
            "proc2".to_string(),
            "in".to_string(),
            3,
        );

        let conn2 = ProcessorConnection::<i32>::new(
            "proc1".to_string(),
            "out".to_string(),
            "proc2".to_string(),
            "in".to_string(),
            3,
        );

        assert_ne!(conn1.id, conn2.id);
    }
}
