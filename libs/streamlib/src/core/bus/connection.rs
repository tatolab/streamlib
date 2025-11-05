use rtrb::{Producer, Consumer, RingBuffer};
use std::sync::Arc;
use parking_lot::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionId(u64);

impl ConnectionId {
    pub fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        Self(COUNTER.fetch_add(1, Ordering::Relaxed))
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

    pub fn write(&self, data: T) -> Result<(), T> {
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
