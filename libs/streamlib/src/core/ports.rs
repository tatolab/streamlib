use parking_lot::Mutex;
use std::sync::Arc;

use super::runtime::WakeupEvent;
use super::bus::{Bus, BusReader, AudioBus, VideoBus, DataBus, BusMessage};
use super::frames::{VideoFrame, AudioFrame, DataFrame, AudioSignal};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortType {
    Video,
    Audio,
    Data,
}

pub trait PortMessage: BusMessage + Clone + Send + 'static {
    fn port_type() -> PortType;
    fn schema() -> std::sync::Arc<crate::core::Schema>;
    fn examples() -> Vec<(&'static str, serde_json::Value)> {
        Vec::new()
    }
}

impl PortType {
    pub fn default_capacity(&self) -> usize {
        match self {
            PortType::Video => 3,
            PortType::Audio => 3,
            PortType::Data => 16,
        }
    }
}

pub struct StreamOutput<T: PortMessage> {
    name: String,
    port_type: PortType,
    bus: Arc<Mutex<Option<Arc<dyn Bus<T>>>>>,
    downstream_wakeup: Mutex<Option<crossbeam_channel::Sender<WakeupEvent>>>,
}

impl<T: PortMessage> StreamOutput<T> {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            port_type: T::port_type(),
            bus: Arc::new(Mutex::new(None)),
            downstream_wakeup: Mutex::new(None),
        }
    }

    pub fn write(&self, data: T) {
        if let Some(bus) = self.bus.lock().as_ref() {
            bus.write(data);

            if let Some(wakeup_tx) = self.downstream_wakeup.lock().as_ref() {
                let _ = wakeup_tx.send(WakeupEvent::DataAvailable);
            }
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn port_type(&self) -> PortType {
        self.port_type
    }

    pub fn set_bus(&self, bus: Arc<dyn Bus<T>>) {
        *self.bus.lock() = Some(bus);
    }

    pub fn get_bus(&self) -> Option<Arc<dyn Bus<T>>> {
        self.bus.lock().clone()
    }

    pub fn get_or_create_bus(&self) -> Arc<dyn Bus<T>>
    where
        T: 'static,
    {
        let mut bus_guard = self.bus.lock();
        if let Some(bus) = bus_guard.as_ref() {
            return Arc::clone(bus);
        }

        use crate::core::frames::{VideoFrame, AudioSignal, MonoSignal, StereoSignal, DataFrame};

        let bus: Arc<dyn Bus<T>> = if std::any::TypeId::of::<T>() == std::any::TypeId::of::<VideoFrame>() {
            let video_bus = create_video_bus();
            unsafe { std::mem::transmute(video_bus) }
        } else if std::any::TypeId::of::<T>() == std::any::TypeId::of::<StereoSignal>() {
            let audio_bus = create_audio_bus::<2>();
            unsafe { std::mem::transmute(audio_bus) }
        } else if std::any::TypeId::of::<T>() == std::any::TypeId::of::<MonoSignal>() {
            let audio_bus = create_audio_bus::<1>();
            unsafe { std::mem::transmute(audio_bus) }
        } else if std::any::TypeId::of::<T>() == std::any::TypeId::of::<DataFrame>() {
            let data_bus = create_data_bus();
            unsafe { std::mem::transmute(data_bus) }
        } else {
            panic!("Unsupported port message type for bus creation");
        };

        *bus_guard = Some(Arc::clone(&bus));
        bus
    }

    pub fn set_downstream_wakeup(&self, wakeup_tx: crossbeam_channel::Sender<WakeupEvent>) {
        *self.downstream_wakeup.lock() = Some(wakeup_tx);
    }
}

impl<T: PortMessage> Clone for StreamOutput<T> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            port_type: self.port_type,
            bus: Arc::clone(&self.bus),
            downstream_wakeup: Mutex::new(self.downstream_wakeup.lock().clone()),
        }
    }
}

impl<T: PortMessage> std::fmt::Debug for StreamOutput<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamOutput")
            .field("name", &self.name)
            .field("port_type", &self.port_type)
            .finish()
    }
}

pub struct StreamInput<T: PortMessage> {
    name: String,
    port_type: PortType,
    reader: Mutex<Option<Box<dyn BusReader<T>>>>,
}

impl<T: PortMessage> StreamInput<T> {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            port_type: T::port_type(),
            reader: Mutex::new(None),
        }
    }

    pub fn connect_reader(&self, reader: Box<dyn BusReader<T>>) {
        *self.reader.lock() = Some(reader);
    }

    pub fn connect_bus(&self, bus: Arc<dyn std::any::Any + Send + Sync>) -> bool {
        use crate::core::bus::Bus;

        if let Ok(typed_bus) = bus.downcast::<Arc<dyn Bus<T>>>() {
            let reader = typed_bus.create_reader();
            self.connect_reader(reader);
            true
        } else {
            false
        }
    }

    pub fn take_reader(&self) -> Option<Box<dyn BusReader<T>>> {
        self.reader.lock().take()
    }

    pub fn read_latest(&self) -> Option<T> {
        self.reader.lock().as_mut()?.read_latest()
    }

    pub fn read_all(&self) -> Vec<T> {
        if let Some(reader) = self.reader.lock().as_mut() {
            let mut items = Vec::new();
            while let Some(item) = reader.read_latest() {
                items.push(item);
            }
            items
        } else {
            Vec::new()
        }
    }

    pub fn is_connected(&self) -> bool {
        self.reader.lock().is_some()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn port_type(&self) -> PortType {
        self.port_type
    }
}

impl<T: PortMessage> Clone for StreamInput<T> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            port_type: self.port_type,
            reader: Mutex::new(
                self.reader.lock()
                    .as_ref()
                    .map(|r| r.clone_reader())
            ),
        }
    }
}

impl<T: PortMessage> std::fmt::Debug for StreamInput<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamInput")
            .field("name", &self.name)
            .field("port_type", &self.port_type)
            .field("connected", &self.is_connected())
            .finish()
    }
}

pub fn create_video_bus() -> Arc<dyn Bus<VideoFrame>> {
    Arc::new(VideoBus::with_default_capacity())
}

pub fn create_data_bus() -> Arc<dyn Bus<DataFrame>> {
    Arc::new(DataBus::new())
}

pub fn create_audio_bus<const CHANNELS: usize>() -> Arc<dyn Bus<AudioSignal<CHANNELS>>> {
    Arc::new(AudioBus::<CHANNELS>::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    impl PortMessage for i32 {
        fn port_type() -> PortType {
            PortType::Data
        }

        fn schema() -> std::sync::Arc<crate::core::Schema> {
            use crate::core::{Schema, Field, FieldType, SemanticVersion, SerializationFormat};
            std::sync::Arc::new(
                Schema::new(
                    "i32",
                    SemanticVersion::new(1, 0, 0),
                    vec![Field::new("value", FieldType::Int32)],
                    SerializationFormat::Bincode,
                )
            )
        }
    }

    #[test]
    fn test_port_type_defaults() {
        assert_eq!(PortType::Video.default_capacity(), 3);
        assert_eq!(PortType::Audio.default_capacity(), 3);
        assert_eq!(PortType::Data.default_capacity(), 16);
    }

    #[test]
    fn test_output_creation() {
        let output = StreamOutput::<i32>::new("test");
        assert_eq!(output.name(), "test");
        assert_eq!(output.port_type(), PortType::Data);
    }

    #[test]
    fn test_input_creation() {
        let input = StreamInput::<i32>::new("test");
        assert_eq!(input.name(), "test");
        assert_eq!(input.port_type(), PortType::Data);
        assert!(!input.is_connected());
    }

    #[test]
    fn test_write_and_read() {
        let output = StreamOutput::<i32>::new("test");
        let input = StreamInput::<i32>::new("test");

        let bus = create_data_bus();
        output.set_bus(Arc::clone(&bus) as Arc<dyn Bus<i32>>);

        let reader = bus.create_reader();
        input.connect_reader(reader as Box<dyn BusReader<i32>>);
        assert!(input.is_connected());

        output.write(42);
        output.write(100);

        assert_eq!(input.read_latest(), Some(100));
    }

    #[test]
    fn test_fan_out() {
        let output = StreamOutput::<i32>::new("test");
        let input1 = StreamInput::<i32>::new("test1");
        let input2 = StreamInput::<i32>::new("test2");

        let bus = create_data_bus();
        output.set_bus(Arc::clone(&bus) as Arc<dyn Bus<i32>>);

        let reader1 = bus.create_reader();
        let reader2 = bus.create_reader();
        input1.connect_reader(reader1 as Box<dyn BusReader<i32>>);
        input2.connect_reader(reader2 as Box<dyn BusReader<i32>>);

        output.write(42);

        assert_eq!(input1.read_latest(), Some(42));
        assert_eq!(input2.read_latest(), Some(42));
    }

    #[test]
    fn test_read_all() {
        let output = StreamOutput::<i32>::new("test");
        let input = StreamInput::<i32>::new("test");

        let bus = create_data_bus();
        output.set_bus(Arc::clone(&bus) as Arc<dyn Bus<i32>>);

        let reader = bus.create_reader();
        input.connect_reader(reader as Box<dyn BusReader<i32>>);

        output.write(1);
        output.write(2);
        output.write(3);

        let data = input.read_all();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0], 3);

        let data2 = input.read_all();
        assert_eq!(data2.len(), 0);
    }

    #[test]
    fn test_read_from_unconnected() {
        let input = StreamInput::<i32>::new("test");
        assert_eq!(input.read_latest(), None);
        assert_eq!(input.read_all().len(), 0);
    }
}
