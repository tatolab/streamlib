"""
Plugin registration system for extending the library.

This module provides a simple plugin architecture that allows users to:
- Register custom sources, sinks, and layers
- Discover available implementations
- Load plugins dynamically
"""

from typing import Type, Dict, Optional, Callable
from .base import StreamSource, StreamSink, Layer, Compositor


class PluginRegistry:
    """
    Registry for sources, sinks, layers, and compositors.

    This enables dynamic discovery and registration of implementations.
    """

    def __init__(self):
        self._sources: Dict[str, Type[StreamSource]] = {}
        self._sinks: Dict[str, Type[StreamSink]] = {}
        self._layers: Dict[str, Type[Layer]] = {}
        self._compositors: Dict[str, Type[Compositor]] = {}

    def register_source(self, name: str, source_class: Type[StreamSource]) -> None:
        """
        Register a source implementation.

        Args:
            name: Name to register under (e.g., 'webcam', 'file', 'network')
            source_class: Source class to register

        Example:
            registry.register_source('webcam', WebcamSource)
        """
        self._sources[name] = source_class

    def register_sink(self, name: str, sink_class: Type[StreamSink]) -> None:
        """
        Register a sink implementation.

        Args:
            name: Name to register under (e.g., 'file', 'hls', 'display')
            sink_class: Sink class to register

        Example:
            registry.register_sink('hls', HLSSink)
        """
        self._sinks[name] = sink_class

    def register_layer(self, name: str, layer_class: Type[Layer]) -> None:
        """
        Register a layer implementation.

        Args:
            name: Name to register under (e.g., 'video', 'drawing', 'ml')
            layer_class: Layer class to register

        Example:
            registry.register_layer('ml', MLLayer)
        """
        self._layers[name] = layer_class

    def register_compositor(self, name: str, compositor_class: Type[Compositor]) -> None:
        """
        Register a compositor implementation.

        Args:
            name: Name to register under (e.g., 'default', 'gpu')
            compositor_class: Compositor class to register

        Example:
            registry.register_compositor('default', DefaultCompositor)
        """
        self._compositors[name] = compositor_class

    def get_source(self, name: str) -> Optional[Type[StreamSource]]:
        """Get a registered source class by name."""
        return self._sources.get(name)

    def get_sink(self, name: str) -> Optional[Type[StreamSink]]:
        """Get a registered sink class by name."""
        return self._sinks.get(name)

    def get_layer(self, name: str) -> Optional[Type[Layer]]:
        """Get a registered layer class by name."""
        return self._layers.get(name)

    def get_compositor(self, name: str) -> Optional[Type[Compositor]]:
        """Get a registered compositor class by name."""
        return self._compositors.get(name)

    def list_sources(self) -> list[str]:
        """List all registered source names."""
        return list(self._sources.keys())

    def list_sinks(self) -> list[str]:
        """List all registered sink names."""
        return list(self._sinks.keys())

    def list_layers(self) -> list[str]:
        """List all registered layer names."""
        return list(self._layers.keys())

    def list_compositors(self) -> list[str]:
        """List all registered compositor names."""
        return list(self._compositors.keys())


# Global registry instance
_registry = PluginRegistry()


def register_source(name: str) -> Callable[[Type[StreamSource]], Type[StreamSource]]:
    """
    Decorator to register a source.

    Example:
        @register_source('webcam')
        class WebcamSource(StreamSource):
            ...
    """
    def decorator(cls: Type[StreamSource]) -> Type[StreamSource]:
        _registry.register_source(name, cls)
        return cls
    return decorator


def register_sink(name: str) -> Callable[[Type[StreamSink]], Type[StreamSink]]:
    """
    Decorator to register a sink.

    Example:
        @register_sink('hls')
        class HLSSink(StreamSink):
            ...
    """
    def decorator(cls: Type[StreamSink]) -> Type[StreamSink]:
        _registry.register_sink(name, cls)
        return cls
    return decorator


def register_layer(name: str) -> Callable[[Type[Layer]], Type[Layer]]:
    """
    Decorator to register a layer.

    Example:
        @register_layer('drawing')
        class DrawingLayer(Layer):
            ...
    """
    def decorator(cls: Type[Layer]) -> Type[Layer]:
        _registry.register_layer(name, cls)
        return cls
    return decorator


def register_compositor(name: str) -> Callable[[Type[Compositor]], Type[Compositor]]:
    """
    Decorator to register a compositor.

    Example:
        @register_compositor('default')
        class DefaultCompositor(Compositor):
            ...
    """
    def decorator(cls: Type[Compositor]) -> Type[Compositor]:
        _registry.register_compositor(name, cls)
        return cls
    return decorator


def get_registry() -> PluginRegistry:
    """Get the global plugin registry."""
    return _registry
