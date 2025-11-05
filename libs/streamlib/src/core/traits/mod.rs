
pub mod element;
pub mod processor;
pub mod dyn_element;
mod dyn_element_impl;

mod sealed {
    pub trait Sealed {}
}

pub use sealed::Sealed;

pub use element::{
    StreamElement,
    ElementType,
};

pub use processor::StreamProcessor;

pub use dyn_element::DynStreamElement;
