"""
Pipeline Builder for streamlib.

Provides a fluent API for building streaming pipelines with automatic
connection management, type checking, and branching support.

Example:
    # Simple linear pipeline
    pipeline = (
        runtime.pipeline()
        .source(camera)
        .effect(blur, sigma=2.0)
        .effect(denoise, strength=0.5)
        .sink(display)
    )
    await pipeline.start()

    # Pipeline with split/join
    pipeline = (
        runtime.pipeline()
        .source(camera)
        .split(
            audio=lambda p: p.effect(reverb).effect(normalize),
            video=lambda p: p.effect(blur).effect(sharpen)
        )
        .effect(av_sync)  # Receives joined audio+video
        .sink(output)
    )
"""

from typing import Any, Callable, Dict, List, Optional, Union, TYPE_CHECKING
from dataclasses import dataclass, field
import inspect

if TYPE_CHECKING:
    from .runtime import StreamRuntime
    from .handler import StreamHandler
    from .stream import Stream


@dataclass
class PipelineNode:
    """Represents a node in the pipeline graph."""
    handler: 'StreamHandler'
    params: Dict[str, Any] = field(default_factory=dict)
    node_type: str = 'effect'  # 'source', 'effect', 'sink', 'split', 'join'

    # For split/join nodes
    branches: Optional[Dict[str, 'PipelineBuilder']] = None
    join_handler: Optional['StreamHandler'] = None


class PipelineBuilder:
    """
    Fluent API for building streaming pipelines.

    Automatically handles:
    - Stream creation and addition to runtime
    - Port connection and type validation
    - Branching with split/join patterns
    - Parameter passing to effects
    """

    def __init__(self, runtime: 'StreamRuntime'):
        """
        Initialize pipeline builder.

        Args:
            runtime: StreamRuntime to add pipeline to
        """
        self._runtime = runtime
        self._nodes: List[PipelineNode] = []
        self._built = False
        self._streams: List['Stream'] = []

    def source(self, handler: Union['StreamHandler', Callable], **kwargs) -> 'PipelineBuilder':
        """
        Add a source handler to the pipeline.

        Args:
            handler: Source handler or decorated function
            **kwargs: Parameters to pass to handler if it's a decorator

        Returns:
            Self for chaining

        Example:
            pipeline.source(camera)
            pipeline.source(test_pattern, fps=30)
        """
        if self._nodes and self._nodes[0].node_type == 'source':
            raise ValueError("Pipeline already has a source. Use multiple pipelines for multiple sources.")

        # Handle decorated functions
        handler_instance = self._get_handler_instance(handler, kwargs)

        # Verify it has outputs but no required inputs
        if hasattr(handler_instance, 'outputs') and not handler_instance.outputs:
            raise ValueError(f"Source handler {handler_instance} has no outputs")

        self._nodes.insert(0, PipelineNode(
            handler=handler_instance,
            params=kwargs,
            node_type='source'
        ))
        return self

    def effect(self, handler: Union['StreamHandler', Callable], **kwargs) -> 'PipelineBuilder':
        """
        Add an effect handler to the pipeline.

        Args:
            handler: Effect handler or decorated function
            **kwargs: Parameters to pass to handler if it's a decorator

        Returns:
            Self for chaining

        Example:
            pipeline.effect(blur, sigma=2.0)
            pipeline.effect(denoise)
        """
        if not self._nodes:
            raise ValueError("Pipeline must start with a source")

        handler_instance = self._get_handler_instance(handler, kwargs)

        self._nodes.append(PipelineNode(
            handler=handler_instance,
            params=kwargs,
            node_type='effect'
        ))
        return self

    def sink(self, handler: Union['StreamHandler', Callable], **kwargs) -> 'PipelineBuilder':
        """
        Add a sink handler to the pipeline.

        Args:
            handler: Sink handler or decorated function
            **kwargs: Parameters to pass to handler if it's a decorator

        Returns:
            Self for chaining

        Example:
            pipeline.sink(display)
            pipeline.sink(file_writer, filename='output.mp4')
        """
        if not self._nodes:
            raise ValueError("Pipeline must start with a source")

        handler_instance = self._get_handler_instance(handler, kwargs)

        # Verify it has inputs but no outputs (or optional outputs)
        if hasattr(handler_instance, 'inputs') and not handler_instance.inputs:
            raise ValueError(f"Sink handler {handler_instance} has no inputs")

        self._nodes.append(PipelineNode(
            handler=handler_instance,
            params=kwargs,
            node_type='sink'
        ))
        return self

    def split(self, **branches: Callable[['PipelineBuilder'], 'PipelineBuilder']) -> 'PipelineBuilder':
        """
        Split the pipeline into multiple branches based on output types.

        Args:
            **branches: Named branches with pipeline builders
                       Keys should match output port names (e.g., 'audio', 'video')

        Returns:
            Self for chaining (after branches rejoin)

        Example:
            pipeline.split(
                audio=lambda p: p.effect(reverb).effect(normalize),
                video=lambda p: p.effect(blur).effect(sharpen)
            )
        """
        if not self._nodes:
            raise ValueError("Cannot split empty pipeline")

        # Get the last handler's outputs to verify branch names
        last_handler = self._nodes[-1].handler
        if hasattr(last_handler, 'outputs'):
            available_outputs = set(last_handler.outputs.keys())
            requested_branches = set(branches.keys())

            # Check if all branches correspond to actual outputs
            invalid_branches = requested_branches - available_outputs
            if invalid_branches:
                raise ValueError(
                    f"Invalid branch names: {invalid_branches}. "
                    f"Available outputs: {available_outputs}"
                )

        # Create sub-pipelines for each branch
        branch_builders = {}
        for branch_name, branch_func in branches.items():
            # Create a new pipeline builder for this branch
            branch_pipeline = PipelineBuilder(self._runtime)
            # Let the user define the branch pipeline
            branch_builders[branch_name] = branch_func(branch_pipeline)

        # Add split node
        self._nodes.append(PipelineNode(
            handler=last_handler,  # The handler whose outputs we're splitting
            node_type='split',
            branches=branch_builders
        ))

        # The pipeline continues after the split (branches will rejoin)
        return self

    def join(self, handler: Optional[Union['StreamHandler', Callable]] = None, **kwargs) -> 'PipelineBuilder':
        """
        Explicitly join branches back together with an optional handler.

        Args:
            handler: Optional handler to process joined streams
            **kwargs: Parameters for the handler

        Returns:
            Self for chaining

        Example:
            pipeline.split(
                audio=lambda p: p.effect(audio_effect),
                video=lambda p: p.effect(video_effect)
            ).join(av_sync)  # Explicitly join with sync handler
        """
        # Find the last split node
        split_node = None
        for node in reversed(self._nodes):
            if node.node_type == 'split':
                split_node = node
                break

        if not split_node:
            raise ValueError("join() can only be called after split()")

        if handler:
            handler_instance = self._get_handler_instance(handler, kwargs)
            split_node.join_handler = handler_instance

        return self

    def build(self) -> List['Stream']:
        """
        Build the pipeline and add all streams to runtime.

        This method:
        1. Creates Stream wrappers for all handlers
        2. Adds streams to runtime
        3. Connects ports based on pipeline order
        4. Validates type compatibility

        Returns:
            List of created streams

        Raises:
            RuntimeError: If pipeline is invalid or types don't match
        """
        if self._built:
            raise RuntimeError("Pipeline already built")

        if not self._nodes:
            raise RuntimeError("Cannot build empty pipeline")

        # Create streams and add to runtime
        streams = []
        prev_handler = None

        for i, node in enumerate(self._nodes):
            if node.node_type == 'split':
                # Handle split/join pattern
                streams.extend(self._build_split(node, prev_handler))
                # After split, the join handler becomes prev_handler
                if node.join_handler:
                    prev_handler = node.join_handler
            else:
                # Regular linear node
                from .stream import Stream
                stream = Stream(node.handler)
                self._runtime.add_stream(stream)
                streams.append(stream)

                # Connect to previous handler
                if prev_handler:
                    self._connect_handlers(prev_handler, node.handler)

                prev_handler = node.handler

        self._streams = streams
        self._built = True
        return streams

    async def start(self) -> None:
        """
        Build the pipeline and start the runtime.

        Convenience method that calls build() and runtime.start().

        Example:
            await pipeline.start()
        """
        if not self._built:
            self.build()
        await self._runtime.start()

    async def stop(self) -> None:
        """
        Stop the runtime.

        Convenience method that calls runtime.stop().

        Example:
            await pipeline.stop()
        """
        await self._runtime.stop()

    def _get_handler_instance(self, handler: Union['StreamHandler', Callable], params: Dict) -> 'StreamHandler':
        """
        Get a handler instance from a handler class, instance, or decorated function.

        Args:
            handler: Handler class, instance, or decorated function
            params: Parameters to pass to handler

        Returns:
            StreamHandler instance
        """
        from .handler import StreamHandler

        # Already an instance
        if isinstance(handler, StreamHandler):
            return handler

        # Decorated function (has _stream_metadata)
        if hasattr(handler, '_stream_metadata'):
            # It's already wrapped by decorator, just return it
            return handler

        # Class that needs instantiation
        if inspect.isclass(handler) and issubclass(handler, StreamHandler):
            return handler(**params)

        # Regular function - try to wrap with @video_effect
        if callable(handler):
            # Try to auto-detect if it's a video effect
            from .decorators import video_effect
            sig = inspect.signature(handler)

            # If first param looks like VideoFrame, treat as video effect
            params_list = list(sig.parameters.keys())
            if params_list and 'frame' in params_list[0].lower():
                # Wrap with video_effect decorator
                decorated = video_effect(handler)
                return decorated

        raise ValueError(f"Don't know how to convert {handler} to StreamHandler")

    def _connect_handlers(self, source: 'StreamHandler', dest: 'StreamHandler') -> None:
        """
        Connect output ports from source to input ports of destination.

        Automatically matches ports by type compatibility.

        Args:
            source: Source handler
            dest: Destination handler

        Raises:
            RuntimeError: If no compatible ports found
        """
        # Get outputs from source and inputs from dest
        source_outputs = source.outputs if hasattr(source, 'outputs') else {}
        dest_inputs = dest.inputs if hasattr(dest, 'inputs') else {}

        if not source_outputs:
            raise RuntimeError(f"Source handler {source} has no outputs")
        if not dest_inputs:
            raise RuntimeError(f"Destination handler {dest} has no inputs")

        # Match ports by type
        connected = False
        for out_name, out_port in source_outputs.items():
            for in_name, in_port in dest_inputs.items():
                # Check if types match
                if out_port.port_type == in_port.port_type:
                    self._runtime.connect(out_port, in_port)
                    connected = True
                    break

        if not connected:
            # Get port types for error message
            out_types = {name: port.port_type for name, port in source_outputs.items()}
            in_types = {name: port.port_type for name, port in dest_inputs.items()}
            raise RuntimeError(
                f"No compatible ports between {source} (outputs: {out_types}) "
                f"and {dest} (inputs: {in_types})"
            )

    def _build_split(self, split_node: PipelineNode, prev_handler: 'StreamHandler') -> List['Stream']:
        """
        Build a split/join pattern.

        Args:
            split_node: The split node containing branches
            prev_handler: The handler before the split

        Returns:
            List of streams created for the branches
        """
        streams = []

        # Build each branch
        branch_outputs = {}
        for branch_name, branch_builder in split_node.branches.items():
            # Get the specific output port for this branch
            if branch_name in prev_handler.outputs:
                output_port = prev_handler.outputs[branch_name]

                # Build the branch pipeline
                branch_streams = branch_builder.build()
                streams.extend(branch_streams)

                # Connect the split output to the branch input
                if branch_streams:
                    first_branch_handler = branch_streams[0].handler
                    # Find matching input port
                    for in_name, in_port in first_branch_handler.inputs.items():
                        if output_port.port_type == in_port.port_type:
                            self._runtime.connect(output_port, in_port)
                            break

                    # Track the last handler in this branch
                    branch_outputs[branch_name] = branch_streams[-1].handler

        # If there's a join handler, connect all branches to it
        if split_node.join_handler:
            from .stream import Stream
            join_stream = Stream(split_node.join_handler)
            self._runtime.add_stream(join_stream)
            streams.append(join_stream)

            # Connect each branch output to the join handler
            for branch_name, branch_handler in branch_outputs.items():
                # Try to connect to appropriately named input
                if branch_name in split_node.join_handler.inputs:
                    # Direct name match
                    self._runtime.connect(
                        branch_handler.outputs[list(branch_handler.outputs.keys())[0]],
                        split_node.join_handler.inputs[branch_name]
                    )
                else:
                    # Try type matching
                    self._connect_handlers(branch_handler, split_node.join_handler)

        return streams

    def __repr__(self) -> str:
        node_summary = []
        for node in self._nodes:
            if node.node_type == 'split':
                node_summary.append(f"split({list(node.branches.keys())})")
            else:
                node_summary.append(f"{node.node_type}:{node.handler.handler_id}")
        return f"PipelineBuilder({' → '.join(node_summary)})"


def pipeline(runtime: 'StreamRuntime') -> PipelineBuilder:
    """
    Convenience function to create a pipeline builder.

    Args:
        runtime: StreamRuntime to build pipeline for

    Returns:
        PipelineBuilder instance

    Example:
        from streamlib import pipeline

        p = pipeline(runtime)
            .source(camera)
            .effect(blur)
            .sink(display)
        await p.start()
    """
    return PipelineBuilder(runtime)