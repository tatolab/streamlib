---
active: true
iteration: 18
max_iterations: 100
completion_promise: "DONE"
started_at: "2026-01-11T22:17:07Z"
---

implement true iosurface sharing to resolve this issue, the documentation on osx states that iosurface are shareable across processes, so your job is to create true zero-copy iosurface for this specific case (ipc host and client). In service of that you should make sure trace loogging allows you to know exactly, with 98%+ confidence you understand what the issues are, if you can't tell from logs you should modify the introduce additional logging / address areas where logging isn't correct, such that you have a full image of whats going on. Your goal is to get iosurface sharing working such that a host streamlib and client streamlib can access the same gpu iosurface on osx (please keep in mind that we have an rhi similar to unreal engine), the goal isn't a major abstraction or change, its to find how to get these two talking together. Your trace logging should be sufficient enough to handle this, if processor dieing in python is undiscoverable, then address that for python such that you can get this understood and working
