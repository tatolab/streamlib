---
whoami: amos
name: '@github:tatolab/streamlib#181'
description: Linux — gpu-allocator for Vulkan sub-allocation to prevent memory fragmentation
adapters:
  github: builtin
blocks:
- '@github:tatolab/streamlib#163'
---

@github:tatolab/streamlib#181

Low-priority optimization: replace per-buffer `vkAllocateMemory` with `gpu-allocator` sub-allocation. Evaluate under real workloads first.
