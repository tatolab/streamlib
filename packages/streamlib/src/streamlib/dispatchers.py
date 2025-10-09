"""
Dispatchers for actor execution contexts.

Different dispatchers optimize for different workload types:
- AsyncioDispatcher: I/O-bound (network, file, events) - DEFAULT
- ThreadPoolDispatcher: CPU-bound (encoding, audio DSP)
- ProcessPoolDispatcher: Heavy compute (multi-pass encoding) - Phase 4
- GPUDispatcher: GPU-accelerated (ML inference, shaders) - Phase 4

Example usage:
    # I/O-bound actor (network, file)
    actor = NetworkActor(dispatcher=AsyncioDispatcher())

    # CPU-bound actor (audio DSP)
    actor = AudioProcessorActor(dispatcher=ThreadPoolDispatcher(workers=4))

    # GPU-bound actor (ML inference)
    actor = FaceDetectorActor(dispatcher=GPUDispatcher(device='cuda:0'))
"""

import asyncio
from abc import ABC, abstractmethod
from concurrent.futures import ThreadPoolExecutor, ProcessPoolExecutor
from typing import Coroutine, Set


class Dispatcher(ABC):
    """
    Abstract dispatcher for actor execution.

    Dispatchers manage where and how actor code runs:
    - Event loop (asyncio)
    - Thread pool (CPU-bound)
    - Process pool (heavy compute)
    - GPU (accelerated compute)
    """

    @abstractmethod
    async def dispatch(self, coro: Coroutine) -> None:
        """
        Execute coroutine in appropriate context.

        Args:
            coro: Coroutine to execute
        """
        pass

    @abstractmethod
    async def shutdown(self) -> None:
        """
        Clean shutdown of dispatcher.

        Waits for all pending work to complete.
        """
        pass


class AsyncioDispatcher(Dispatcher):
    """
    Asyncio event loop dispatcher (I/O-bound tasks).

    Best for:
    - Network I/O (send/receive)
    - File I/O (read/write)
    - Event handling
    - Coordination/orchestration

    Not suitable for:
    - CPU-bound work (blocks event loop)
    - Long-running compute (use ThreadPool or ProcessPool)

    This is the DEFAULT dispatcher - most actors should use this.
    """

    def __init__(self):
        """Initialize asyncio dispatcher."""
        self.tasks: Set[asyncio.Task] = set()

    async def dispatch(self, coro: Coroutine) -> None:
        """
        Execute coroutine in asyncio event loop.

        Args:
            coro: Coroutine to execute

        Note: Task runs concurrently with other asyncio tasks.
        """
        task = asyncio.create_task(coro)
        self.tasks.add(task)
        task.add_done_callback(self.tasks.discard)

    async def shutdown(self) -> None:
        """
        Shutdown dispatcher, wait for all tasks.

        Note: Cancels remaining tasks after graceful wait.
        """
        if not self.tasks:
            return

        # Wait for all tasks to complete
        await asyncio.gather(*self.tasks, return_exceptions=True)


class ThreadPoolDispatcher(Dispatcher):
    """
    Thread pool dispatcher (CPU-bound tasks).

    Best for:
    - Video encoding (H.264, HEVC)
    - Audio DSP (filters, effects)
    - Image processing (CPU-based)
    - Synchronous libraries (no async support)

    Not suitable for:
    - I/O-bound work (use AsyncioDispatcher)
    - Heavy parallel compute (use ProcessPoolDispatcher)
    - GPU work (use GPUDispatcher)

    Note: Python GIL limits true parallelism within a process.
    For CPU-heavy work across multiple cores, use ProcessPoolDispatcher.
    """

    def __init__(self, max_workers: int = 4):
        """
        Initialize thread pool dispatcher.

        Args:
            max_workers: Maximum number of worker threads
        """
        self.executor = ThreadPoolExecutor(max_workers=max_workers)
        self.loop = None

    async def dispatch(self, coro: Coroutine) -> None:
        """
        Execute coroutine in thread pool.

        Args:
            coro: Coroutine to execute

        Note: Each coroutine runs in its own thread with its own event loop.
        """
        if self.loop is None:
            self.loop = asyncio.get_running_loop()

        # Run coroutine in thread
        await self.loop.run_in_executor(self.executor, self._run_coro, coro)

    def _run_coro(self, coro: Coroutine):
        """
        Helper to run coroutine in thread.

        Creates a new event loop for the thread.
        """
        loop = asyncio.new_event_loop()
        try:
            return loop.run_until_complete(coro)
        finally:
            loop.close()

    async def shutdown(self) -> None:
        """
        Shutdown thread pool, wait for all threads.

        Note: Blocks until all threads complete their work.
        """
        self.executor.shutdown(wait=True)


class ProcessPoolDispatcher(Dispatcher):
    """
    Process pool dispatcher (heavy compute) - STUB for Phase 4.

    Best for:
    - Multi-pass video encoding
    - Heavy parallel compute
    - CPU-intensive algorithms
    - Bypassing Python GIL

    Not suitable for:
    - I/O-bound work (use AsyncioDispatcher)
    - GPU work (use GPUDispatcher)
    - Shared state (processes don't share memory)

    This is a stub. Real implementation in Phase 4 will:
    - Serialize actor state for IPC
    - Use multiprocessing or similar
    - Handle process lifecycle

    For now, raises NotImplementedError.
    """

    def __init__(self, max_workers: int = 2):
        """
        Initialize process pool dispatcher.

        Args:
            max_workers: Maximum number of worker processes

        Note: Currently stub, not functional.
        """
        self.max_workers = max_workers
        self.executor = ProcessPoolExecutor(max_workers=max_workers)
        print(f"[ProcessPoolDispatcher] Warning: Not implemented, stub only")

    async def dispatch(self, coro: Coroutine) -> None:
        """
        Execute coroutine in process pool (stub).

        Raises:
            NotImplementedError: Phase 4 feature
        """
        raise NotImplementedError(
            "ProcessPoolDispatcher not yet implemented (Phase 4). "
            "Use ThreadPoolDispatcher for CPU-bound work."
        )

    async def shutdown(self) -> None:
        """Shutdown process pool."""
        self.executor.shutdown(wait=True)


class GPUDispatcher(Dispatcher):
    """
    GPU dispatcher (GPU-accelerated compute) - STUB for Phase 4.

    Best for:
    - ML inference (PyTorch, TensorFlow)
    - GPU shaders (CUDA, OpenCL)
    - GPU video encoding (NVENC, VCE)
    - Image processing (GPU-based)

    Not suitable for:
    - I/O-bound work (use AsyncioDispatcher)
    - CPU-bound work (use ThreadPool)
    - Traditional audio DSP (CPU is better)

    This is a stub. Real implementation in Phase 4 will:
    - Manage CUDA streams
    - Handle GPU memory
    - Coordinate GPU operations

    For now, just runs coroutine directly (PyTorch ops are synchronous).
    """

    def __init__(self, device: str = 'cuda:0'):
        """
        Initialize GPU dispatcher.

        Args:
            device: PyTorch device string ('cuda:0', 'cuda:1', etc.)

        Note: Currently stub, minimal functionality.
        """
        self.device = device
        print(f"[GPUDispatcher] Warning: Stub implementation, using device {device}")

    async def dispatch(self, coro: Coroutine) -> None:
        """
        Execute coroutine with GPU context (stub).

        Args:
            coro: Coroutine to execute

        Note: Currently just runs coroutine directly. PyTorch operations
        within the coroutine are synchronous and will block.

        TODO Phase 4: Implement proper GPU stream management.
        """
        # GPU work happens synchronously within coroutine
        # (PyTorch operations are blocking)
        await coro

    async def shutdown(self) -> None:
        """
        Shutdown GPU dispatcher (stub).

        TODO Phase 4: Synchronize CUDA streams.
        """
        pass
