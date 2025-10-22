---

## Translated Report (Full Report Below)

Process: camera_display [84399]
Path: /Users/USER/\*/camera_display
Identifier: camera_display
Version: ???
Code Type: ARM-64 (Native)
Parent Process: Exited process [84391]
Responsible: Electron [91850]
User ID: 501

Date/Time: 2025-10-22 13:33:50.1478 -0400
OS Version: macOS 15.6.1 (24G90)
Report Version: 12
Anonymous UUID: 1EB5C6D5-E9DE-C50D-46A8-1749AC93AF6C

Sleep/Wake UUID: 23414675-5AB9-4663-BCA5-52ACF8E352D4

Time Awake Since Boot: 1700000 seconds
Time Since Wake: 847977 seconds

System Integrity Protection: enabled

Crashed Thread: 15

Exception Type: EXC_BREAKPOINT (SIGTRAP)
Exception Codes: 0x0000000000000001, 0x0000000186223cc0

Termination Reason: Namespace SIGNAL, Code 5 Trace/BPT trap: 5
Terminating Process: exc handler [84399]

Application Specific Information:
Must only be used from the main thread

Thread 0:: main Dispatch queue: com.apple.main-thread
0 libsystem*kernel.dylib 0x1814b5c34 mach_msg2_trap + 8
1 libsystem_kernel.dylib 0x1814c83a0 mach_msg2_internal + 76
2 libsystem_kernel.dylib 0x1814be764 mach_msg_overwrite + 484
3 libsystem_kernel.dylib 0x1814b5fa8 mach_msg + 24
4 libdispatch.dylib 0x18135aee0 \_dispatch_mach_send_and_wait_for_reply + 548
5 libdispatch.dylib 0x18135b280 dispatch_mach_send_with_result_and_wait_for_reply + 60
6 libxpc.dylib 0x1811fc468 xpc_connection_send_message_with_reply_sync + 284
7 LaunchServices 0x181aac0d8 LSClientToServerConnection::sendWithReply(void\*) + 68
8 LaunchServices 0x181aae7b4 \_LSCopyApplicationInformation + 988
9 AE 0x188fd7044 0x188fd0000 + 28740
10 libdispatch.dylib 0x18135985c \_dispatch_client_callout + 16
11 libdispatch.dylib 0x181342a28 \_dispatch_once_callout + 32
12 AE 0x188fd1698 \_AERegisterCurrentApplicationInfomationWithAppleEventsD + 336
13 AE 0x188fd6dcc 0x188fd0000 + 28108
14 libdispatch.dylib 0x18135985c \_dispatch_client_callout + 16
15 libdispatch.dylib 0x181342a28 \_dispatch_once_callout + 32
16 AE 0x188fd1544 AEGetRegisteredMachPort + 72
17 AE 0x188fd64c0 0x188fd0000 + 25792
18 AE 0x188fd5e5c 0x188fd0000 + 24156
19 libdispatch.dylib 0x18135985c \_dispatch_client_callout + 16
20 libdispatch.dylib 0x181342a28 \_dispatch_once_callout + 32
21 AE 0x188fd13ac aeInstallRunLoopDispatcher + 192
22 HIToolbox 0x18d07a9f4 \_FirstEventTime + 144
23 HIToolbox 0x18d083178 RunCurrentEventLoopInMode + 64
24 HIToolbox 0x18d08631c ReceiveNextEventCommon + 216
25 HIToolbox 0x18d211484 \_BlockUntilNextEventMatchingListInModeWithFilter + 76
26 AppKit 0x185505a34 \_DPSNextEvent + 684
27 AppKit 0x185ea4940 -[NSApplication(NSEventRouting) _nextEventMatchingEventMask:untilDate:inMode:dequeue:] + 688
28 camera_display 0x100948330 *$LT$$LP$A$C$B$C$C$C$D$RP$$u20$as$u20$objc2..encode..EncodeArguments$GT$::**invoke::hf996fc9ec8218dd0 + 152
29 camera*display 0x100941498 objc2::runtime::message_receiver::msg_send_primitive::send::h3e375a8fe16fe952 + 72
30 camera_display 0x100949214 objc2::runtime::message_receiver::MessageReceiver::send_message::hc9c7788f07986769 + 208
31 camera_display 0x1009411f8 *$LT$MethodFamily$u20$as$u20$objc2..**macro*helpers..msg_send_retained..MsgSend$LT$Receiver$C$Return$GT$$GT$::send_message::h305fa38f066d40da + 224
32 camera_display 0x1009481a8 streamlib_apple::runtime_ext::configure_macos_event_loop::*$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::h3dab1deb9403f3ac + 644
33 camera_display 0x1009394f4 _$LT$core..pin..Pin$LT$P$GT$$u20$as$u20$core..future..future..Future$GT$::poll::he5b5385ffc88baf7 + 80 (future.rs:133)
34 camera*display 0x100934f04 streamlib_core::runtime::StreamRuntime::run::*$u7b$$u7b$closure$u7d$$u7d$::h131999c1887513fd + 2860 (runtime.rs:347)
35 camera*display 0x100932d24 streamlib::runtime::StreamRuntime::run::*$u7b$$u7b$closure$u7d$$u7d$::h035280339e749049 + 340 (runtime.rs:92)
36 camera*display 0x1009315a0 camera_display::main::*$u7b$$u7b$closure$u7d$$u7d$::h931c4c080d7e5443 + 1860 (camera*display.rs:50)
37 camera_display 0x100933e58 tokio::runtime::park::CachedParkThread::block_on::*$u7b$$u7b$closure$u7d$$u7d$::he652241d9d748cc5 + 64 (park.rs:285)
38 camera*display 0x100933608 tokio::task::coop::with_budget::hdee52a54e383e63e + 88 (mod.rs:167) [inlined]
39 camera_display 0x100933608 tokio::task::coop::budget::h522291a1aa638a35 + 208 (mod.rs:133) [inlined]
40 camera_display 0x100933608 tokio::runtime::park::CachedParkThread::block_on::h74280c42af9ed8f4 + 576 (park.rs:285)
41 camera_display 0x10093bd5c tokio::runtime::context::blocking::BlockingRegionGuard::block_on::h75c8666213e7e72e + 140 (blocking.rs:66)
42 camera_display 0x10093bf74 tokio::runtime::scheduler::multi_thread::MultiThread::block_on::*$u7b$$u7b$closure$u7d$$u7d$::h3e494ff6abba3c0c + 80 (mod.rs:87)
43 camera*display 0x100938b20 tokio::runtime::context::runtime::enter_runtime::h52647cd33246ba11 + 232 (runtime.rs:65)
44 camera_display 0x10093becc tokio::runtime::scheduler::multi_thread::MultiThread::block_on::h2ecce2490b5df586 + 92 (mod.rs:86)
45 camera_display 0x10093b10c tokio::runtime::runtime::Runtime::block_on_inner::h136e71ce595567e7 + 184 (runtime.rs:370)
46 camera_display 0x10093b318 tokio::runtime::runtime::Runtime::block_on::h5e5ba000b05908f2 + 352 (runtime.rs:342)
47 camera_display 0x100939ae0 camera_display::main::h98bf6d72bbfd3aa2 + 232 (camera_display.rs:54)
48 camera_display 0x100931b84 core::ops::function::FnOnce::call_once::h7b4a40b813d1cc1d + 20 (function.rs:253)
49 camera_display 0x10093c200 std::sys::backtrace::\_\_rust_begin_short_backtrace::h675d34eca97f0003 + 24 (backtrace.rs:158)
50 camera_display 0x10093bb10 std::rt::lang_start::*$u7b$$u7b$closure$u7d$$u7d$::h926ec4a5bf2e3bad + 36 (rt.rs:206)
51 camera_display 0x100a9ef88 std::rt::lang_start_internal::h5b2b6e2cac0b4d2b + 140
52 camera_display 0x10093bae0 std::rt::lang_start::hd6ff86ecba9d7b6a + 84 (rt.rs:205)
53 camera_display 0x100939b68 main + 36
54 dyld 0x181156b98 start + 6076

Thread 1:: tokio-runtime-worker
0 libsystem*kernel.dylib 0x1814b93cc \_\_psynch_cvwait + 8
1 libsystem_pthread.dylib 0x1814f80e0 \_pthread_cond_wait + 984
2 camera_display 0x100a6acfc *$LT$parking*lot_core..thread_parker..imp..ThreadParker$u20$as$u20$parking_lot_core..thread_parker..ThreadParkerT$GT$::park::h0e2d0998749974de + 256
3 camera_display 0x100a630e8 parking_lot_core::parking_lot::park::*$u7b$$u7b$closure$u7d$$u7d$::h0b4fc83101e8557f + 748
4 camera*display 0x100a62c10 parking_lot_core::parking_lot::park::hd04bbb424560662a + 296
5 camera_display 0x100a6a268 parking_lot::condvar::Condvar::wait_until_internal::h8640b2ba350582ed + 124
6 camera_display 0x1009b32e0 parking_lot::condvar::Condvar::wait::h6618c4600aa6d512 + 68
7 camera_display 0x1009f2e1c tokio::loom::std::parking_lot::Condvar::wait::hc858434e42aaccb1 + 36
8 camera_display 0x1009ecf04 tokio::runtime::scheduler::multi_thread::park::Inner::park_condvar::h2267f8a57982d137 + 264
9 camera_display 0x1009ecdb0 tokio::runtime::scheduler::multi_thread::park::Inner::park::h3c3dbc8c396a0894 + 168
10 camera_display 0x1009ecae4 tokio::runtime::scheduler::multi_thread::park::Parker::park::h010fa0d10ce96e9b + 40
11 camera_display 0x1009d30d0 tokio::runtime::scheduler::multi_thread::worker::Context::park_timeout::h37500a9fc2ffb756 + 776
12 camera_display 0x1009d2bcc tokio::runtime::scheduler::multi_thread::worker::Context::park::hd6c55373d20068c0 + 968
13 camera_display 0x1009d1688 tokio::runtime::scheduler::multi_thread::worker::Context::run::h541c11fea84a38ec + 1784
14 camera_display 0x1009d0ed4 tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::h60671eb717a34c70 + 104
15 camera_display 0x1009e12c4 tokio::runtime::context::scoped::Scoped$LT$T$GT$::set::h263d3206a13c783c + 148
16 camera_display 0x1009c9a20 tokio::runtime::context::set_scheduler::_$u7b$$u7b$closure$u7d$$u7d$::h74a8e23df56c9882 + 40
17 camera*display 0x1009e84ec std::thread::local::LocalKey$LT$T$GT$::try_with::h2f963d543150c87c + 196
18 camera_display 0x1009e7f50 std::thread::local::LocalKey$LT$T$GT$::with::hbd17f06bbba52edc + 24
19 camera_display 0x1009c99ec tokio::runtime::context::set_scheduler::heb413530e9784dfc + 68
20 camera_display 0x1009d0df8 tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::h79a1aa630e49b68a + 248
21 camera*display 0x1009b8f14 tokio::runtime::context::runtime::enter_runtime::hb50775e529a9a617 + 188
22 camera_display 0x1009d0c58 tokio::runtime::scheduler::multi_thread::worker::run::h79855d48ee1e3c6a + 600
23 camera_display 0x1009d09f4 tokio::runtime::scheduler::multi_thread::worker::Launch::launch::*$u7b$$u7b$closure$u7d$$u7d$::he167c16b6e6899e4 + 24
24 camera*display 0x1009f453c *$LT$tokio..runtime..blocking..task..BlockingTask$LT$T$GT$$u20$as$u20$core..future..future..Future$GT$::poll::h8b9db1efbc4877eb + 136
25 camera*display 0x100992fa8 tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::*$u7b$$u7b$closure$u7d$$u7d$::h904262c7dc2f88b0 + 192
26 camera*display 0x100992d14 tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::h973d3c0095d258c4 + 72
27 camera_display 0x100991ed8 tokio::runtime::task::harness::poll_future::*$u7b$$u7b$closure$u7d$$u7d$::h8fa5ae0829102149 + 64
28 camera*display 0x1009b2ff4 *$LT$core..panic..unwind*safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2974776e7861df5d + 44
29 camera_display 0x1009a61b4 std::panicking::catch_unwind::do_call::h792e9a0912086d16 + 72
30 camera_display 0x1009a5e4c \_\_rust_try + 32
31 camera_display 0x1009a3110 std::panic::catch_unwind::h4474725978f361f4 + 96
32 camera_display 0x100991c70 tokio::runtime::task::harness::poll_future::hcf462599407f235c + 96
33 camera_display 0x1009909a8 tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll_inner::h5e6f168e02291d20 + 160
34 camera_display 0x100990878 tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll::h8a00a85251f71f0c + 28
35 camera_display 0x1009aa090 tokio::runtime::task::raw::poll::hf96de62ba33ad657 + 36
36 camera_display 0x1009a9db0 tokio::runtime::task::raw::RawTask::poll::hde83a8d818768542 + 52
37 camera_display 0x1009b1ec8 tokio::runtime::task::UnownedTask$LT$S$GT$::run::h7ce7c13c00052430 + 64
38 camera_display 0x1009bb178 tokio::runtime::blocking::pool::Task::run::hf56d840fd68c7fdc + 28
39 camera_display 0x1009bcdb0 tokio::runtime::blocking::pool::Inner::run::hd581e97506257439 + 512
40 camera_display 0x1009bcb1c tokio::runtime::blocking::pool::Spawner::spawn_thread::*$u7b$$u7b$closure$u7d$$u7d$::h2a632d4852cdd40e + 144
41 camera*display 0x1009ef10c std::sys::backtrace::\_\_rust_begin_short_backtrace::h51232d61e41dc60c + 16
42 camera_display 0x1009b4600 std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::hbf84c81d58c2715d + 116
43 camera*display 0x1009b302c *$LT$core..panic..unwind*safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2c0e201eb4dddbb7 + 44
44 camera_display 0x1009a6210 std::panicking::catch_unwind::do_call::h94ffcffc7cdce087 + 68
45 camera_display 0x1009b9364 \_\_rust_try + 32
46 camera_display 0x1009b4154 std::thread::Builder::spawn_unchecked*::\_$u7b$$u7b$closure$u7d$$u7d$::haea346fd331ce539 + 728
47 camera_display 0x1009946b4 core::ops::function::FnOnce::call_once$u7b$$u7b$vtable.shim$u7d$$u7d$::h12a95acb393be819 + 24
48 camera_display 0x100a9c560 std::sys::pal::unix::thread::Thread::new::thread_start::h636d41ad3c63ac40 + 56
49 libsystem_pthread.dylib 0x1814f7c0c \_pthread_start + 136
50 libsystem_pthread.dylib 0x1814f2b80 thread_start + 8

Thread 2:: tokio-runtime-worker
0 libsystem*kernel.dylib 0x1814b93cc \_\_psynch_cvwait + 8
1 libsystem_pthread.dylib 0x1814f80e0 \_pthread_cond_wait + 984
2 camera_display 0x100a6acfc *$LT$parking*lot_core..thread_parker..imp..ThreadParker$u20$as$u20$parking_lot_core..thread_parker..ThreadParkerT$GT$::park::h0e2d0998749974de + 256
3 camera_display 0x100a630e8 parking_lot_core::parking_lot::park::*$u7b$$u7b$closure$u7d$$u7d$::h0b4fc83101e8557f + 748
4 camera*display 0x100a62c10 parking_lot_core::parking_lot::park::hd04bbb424560662a + 296
5 camera_display 0x100a6a268 parking_lot::condvar::Condvar::wait_until_internal::h8640b2ba350582ed + 124
6 camera_display 0x1009b32e0 parking_lot::condvar::Condvar::wait::h6618c4600aa6d512 + 68
7 camera_display 0x1009f2e1c tokio::loom::std::parking_lot::Condvar::wait::hc858434e42aaccb1 + 36
8 camera_display 0x1009ecf04 tokio::runtime::scheduler::multi_thread::park::Inner::park_condvar::h2267f8a57982d137 + 264
9 camera_display 0x1009ecdb0 tokio::runtime::scheduler::multi_thread::park::Inner::park::h3c3dbc8c396a0894 + 168
10 camera_display 0x1009ecae4 tokio::runtime::scheduler::multi_thread::park::Parker::park::h010fa0d10ce96e9b + 40
11 camera_display 0x1009d30d0 tokio::runtime::scheduler::multi_thread::worker::Context::park_timeout::h37500a9fc2ffb756 + 776
12 camera_display 0x1009d2bcc tokio::runtime::scheduler::multi_thread::worker::Context::park::hd6c55373d20068c0 + 968
13 camera_display 0x1009d1688 tokio::runtime::scheduler::multi_thread::worker::Context::run::h541c11fea84a38ec + 1784
14 camera_display 0x1009d0ed4 tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::h60671eb717a34c70 + 104
15 camera_display 0x1009e12c4 tokio::runtime::context::scoped::Scoped$LT$T$GT$::set::h263d3206a13c783c + 148
16 camera_display 0x1009c9a20 tokio::runtime::context::set_scheduler::_$u7b$$u7b$closure$u7d$$u7d$::h74a8e23df56c9882 + 40
17 camera*display 0x1009e84ec std::thread::local::LocalKey$LT$T$GT$::try_with::h2f963d543150c87c + 196
18 camera_display 0x1009e7f50 std::thread::local::LocalKey$LT$T$GT$::with::hbd17f06bbba52edc + 24
19 camera_display 0x1009c99ec tokio::runtime::context::set_scheduler::heb413530e9784dfc + 68
20 camera_display 0x1009d0df8 tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::h79a1aa630e49b68a + 248
21 camera*display 0x1009b8f14 tokio::runtime::context::runtime::enter_runtime::hb50775e529a9a617 + 188
22 camera_display 0x1009d0c58 tokio::runtime::scheduler::multi_thread::worker::run::h79855d48ee1e3c6a + 600
23 camera_display 0x1009d09f4 tokio::runtime::scheduler::multi_thread::worker::Launch::launch::*$u7b$$u7b$closure$u7d$$u7d$::he167c16b6e6899e4 + 24
24 camera*display 0x1009f453c *$LT$tokio..runtime..blocking..task..BlockingTask$LT$T$GT$$u20$as$u20$core..future..future..Future$GT$::poll::h8b9db1efbc4877eb + 136
25 camera*display 0x100992fa8 tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::*$u7b$$u7b$closure$u7d$$u7d$::h904262c7dc2f88b0 + 192
26 camera*display 0x100992d14 tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::h973d3c0095d258c4 + 72
27 camera_display 0x100991ed8 tokio::runtime::task::harness::poll_future::*$u7b$$u7b$closure$u7d$$u7d$::h8fa5ae0829102149 + 64
28 camera*display 0x1009b2ff4 *$LT$core..panic..unwind*safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2974776e7861df5d + 44
29 camera_display 0x1009a61b4 std::panicking::catch_unwind::do_call::h792e9a0912086d16 + 72
30 camera_display 0x1009a5e4c \_\_rust_try + 32
31 camera_display 0x1009a3110 std::panic::catch_unwind::h4474725978f361f4 + 96
32 camera_display 0x100991c70 tokio::runtime::task::harness::poll_future::hcf462599407f235c + 96
33 camera_display 0x1009909a8 tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll_inner::h5e6f168e02291d20 + 160
34 camera_display 0x100990878 tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll::h8a00a85251f71f0c + 28
35 camera_display 0x1009aa090 tokio::runtime::task::raw::poll::hf96de62ba33ad657 + 36
36 camera_display 0x1009a9db0 tokio::runtime::task::raw::RawTask::poll::hde83a8d818768542 + 52
37 camera_display 0x1009b1ec8 tokio::runtime::task::UnownedTask$LT$S$GT$::run::h7ce7c13c00052430 + 64
38 camera_display 0x1009bb178 tokio::runtime::blocking::pool::Task::run::hf56d840fd68c7fdc + 28
39 camera_display 0x1009bcdb0 tokio::runtime::blocking::pool::Inner::run::hd581e97506257439 + 512
40 camera_display 0x1009bcb1c tokio::runtime::blocking::pool::Spawner::spawn_thread::*$u7b$$u7b$closure$u7d$$u7d$::h2a632d4852cdd40e + 144
41 camera*display 0x1009ef10c std::sys::backtrace::\_\_rust_begin_short_backtrace::h51232d61e41dc60c + 16
42 camera_display 0x1009b4600 std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::hbf84c81d58c2715d + 116
43 camera*display 0x1009b302c *$LT$core..panic..unwind*safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2c0e201eb4dddbb7 + 44
44 camera_display 0x1009a6210 std::panicking::catch_unwind::do_call::h94ffcffc7cdce087 + 68
45 camera_display 0x1009b9364 \_\_rust_try + 32
46 camera_display 0x1009b4154 std::thread::Builder::spawn_unchecked*::\_$u7b$$u7b$closure$u7d$$u7d$::haea346fd331ce539 + 728
47 camera_display 0x1009946b4 core::ops::function::FnOnce::call_once$u7b$$u7b$vtable.shim$u7d$$u7d$::h12a95acb393be819 + 24
48 camera_display 0x100a9c560 std::sys::pal::unix::thread::Thread::new::thread_start::h636d41ad3c63ac40 + 56
49 libsystem_pthread.dylib 0x1814f7c0c \_pthread_start + 136
50 libsystem_pthread.dylib 0x1814f2b80 thread_start + 8

Thread 3:: tokio-runtime-worker
0 libsystem*kernel.dylib 0x1814b93cc \_\_psynch_cvwait + 8
1 libsystem_pthread.dylib 0x1814f80e0 \_pthread_cond_wait + 984
2 camera_display 0x100a6acfc *$LT$parking*lot_core..thread_parker..imp..ThreadParker$u20$as$u20$parking_lot_core..thread_parker..ThreadParkerT$GT$::park::h0e2d0998749974de + 256
3 camera_display 0x100a630e8 parking_lot_core::parking_lot::park::*$u7b$$u7b$closure$u7d$$u7d$::h0b4fc83101e8557f + 748
4 camera*display 0x100a62c10 parking_lot_core::parking_lot::park::hd04bbb424560662a + 296
5 camera_display 0x100a6a268 parking_lot::condvar::Condvar::wait_until_internal::h8640b2ba350582ed + 124
6 camera_display 0x1009b32e0 parking_lot::condvar::Condvar::wait::h6618c4600aa6d512 + 68
7 camera_display 0x1009f2e1c tokio::loom::std::parking_lot::Condvar::wait::hc858434e42aaccb1 + 36
8 camera_display 0x1009ecf04 tokio::runtime::scheduler::multi_thread::park::Inner::park_condvar::h2267f8a57982d137 + 264
9 camera_display 0x1009ecdb0 tokio::runtime::scheduler::multi_thread::park::Inner::park::h3c3dbc8c396a0894 + 168
10 camera_display 0x1009ecae4 tokio::runtime::scheduler::multi_thread::park::Parker::park::h010fa0d10ce96e9b + 40
11 camera_display 0x1009d30d0 tokio::runtime::scheduler::multi_thread::worker::Context::park_timeout::h37500a9fc2ffb756 + 776
12 camera_display 0x1009d2bcc tokio::runtime::scheduler::multi_thread::worker::Context::park::hd6c55373d20068c0 + 968
13 camera_display 0x1009d1688 tokio::runtime::scheduler::multi_thread::worker::Context::run::h541c11fea84a38ec + 1784
14 camera_display 0x1009d0ed4 tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::h60671eb717a34c70 + 104
15 camera_display 0x1009e12c4 tokio::runtime::context::scoped::Scoped$LT$T$GT$::set::h263d3206a13c783c + 148
16 camera_display 0x1009c9a20 tokio::runtime::context::set_scheduler::_$u7b$$u7b$closure$u7d$$u7d$::h74a8e23df56c9882 + 40
17 camera*display 0x1009e84ec std::thread::local::LocalKey$LT$T$GT$::try_with::h2f963d543150c87c + 196
18 camera_display 0x1009e7f50 std::thread::local::LocalKey$LT$T$GT$::with::hbd17f06bbba52edc + 24
19 camera_display 0x1009c99ec tokio::runtime::context::set_scheduler::heb413530e9784dfc + 68
20 camera_display 0x1009d0df8 tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::h79a1aa630e49b68a + 248
21 camera*display 0x1009b8f14 tokio::runtime::context::runtime::enter_runtime::hb50775e529a9a617 + 188
22 camera_display 0x1009d0c58 tokio::runtime::scheduler::multi_thread::worker::run::h79855d48ee1e3c6a + 600
23 camera_display 0x1009d09f4 tokio::runtime::scheduler::multi_thread::worker::Launch::launch::*$u7b$$u7b$closure$u7d$$u7d$::he167c16b6e6899e4 + 24
24 camera*display 0x1009f453c *$LT$tokio..runtime..blocking..task..BlockingTask$LT$T$GT$$u20$as$u20$core..future..future..Future$GT$::poll::h8b9db1efbc4877eb + 136
25 camera*display 0x100992fa8 tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::*$u7b$$u7b$closure$u7d$$u7d$::h904262c7dc2f88b0 + 192
26 camera*display 0x100992d14 tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::h973d3c0095d258c4 + 72
27 camera_display 0x100991ed8 tokio::runtime::task::harness::poll_future::*$u7b$$u7b$closure$u7d$$u7d$::h8fa5ae0829102149 + 64
28 camera*display 0x1009b2ff4 *$LT$core..panic..unwind*safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2974776e7861df5d + 44
29 camera_display 0x1009a61b4 std::panicking::catch_unwind::do_call::h792e9a0912086d16 + 72
30 camera_display 0x1009a5e4c \_\_rust_try + 32
31 camera_display 0x1009a3110 std::panic::catch_unwind::h4474725978f361f4 + 96
32 camera_display 0x100991c70 tokio::runtime::task::harness::poll_future::hcf462599407f235c + 96
33 camera_display 0x1009909a8 tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll_inner::h5e6f168e02291d20 + 160
34 camera_display 0x100990878 tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll::h8a00a85251f71f0c + 28
35 camera_display 0x1009aa090 tokio::runtime::task::raw::poll::hf96de62ba33ad657 + 36
36 camera_display 0x1009a9db0 tokio::runtime::task::raw::RawTask::poll::hde83a8d818768542 + 52
37 camera_display 0x1009b1ec8 tokio::runtime::task::UnownedTask$LT$S$GT$::run::h7ce7c13c00052430 + 64
38 camera_display 0x1009bb178 tokio::runtime::blocking::pool::Task::run::hf56d840fd68c7fdc + 28
39 camera_display 0x1009bcdb0 tokio::runtime::blocking::pool::Inner::run::hd581e97506257439 + 512
40 camera_display 0x1009bcb1c tokio::runtime::blocking::pool::Spawner::spawn_thread::*$u7b$$u7b$closure$u7d$$u7d$::h2a632d4852cdd40e + 144
41 camera*display 0x1009ef10c std::sys::backtrace::\_\_rust_begin_short_backtrace::h51232d61e41dc60c + 16
42 camera_display 0x1009b4600 std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::hbf84c81d58c2715d + 116
43 camera*display 0x1009b302c *$LT$core..panic..unwind*safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2c0e201eb4dddbb7 + 44
44 camera_display 0x1009a6210 std::panicking::catch_unwind::do_call::h94ffcffc7cdce087 + 68
45 camera_display 0x1009b9364 \_\_rust_try + 32
46 camera_display 0x1009b4154 std::thread::Builder::spawn_unchecked*::\_$u7b$$u7b$closure$u7d$$u7d$::haea346fd331ce539 + 728
47 camera_display 0x1009946b4 core::ops::function::FnOnce::call_once$u7b$$u7b$vtable.shim$u7d$$u7d$::h12a95acb393be819 + 24
48 camera_display 0x100a9c560 std::sys::pal::unix::thread::Thread::new::thread_start::h636d41ad3c63ac40 + 56
49 libsystem_pthread.dylib 0x1814f7c0c \_pthread_start + 136
50 libsystem_pthread.dylib 0x1814f2b80 thread_start + 8

Thread 4:: tokio-runtime-worker
0 libsystem*kernel.dylib 0x1814bbd04 kevent + 8
1 camera_display 0x100a059fc mio::sys::unix::selector::Selector::select::hd28ec6226acb265b + 200
2 camera_display 0x100a08b44 mio::poll::Poll::poll::h98b744e8ad562494 + 80
3 camera_display 0x1009f3fdc tokio::runtime::io::driver::Driver::turn::h2e3afb246fe3dfe1 + 200
4 camera_display 0x1009f3d4c tokio::runtime::io::driver::Driver::park_timeout::h9902b72f5dc913da + 92
5 camera_display 0x1009a1ecc tokio::runtime::signal::Driver::park_timeout::ha8f650a98c982bba + 44
6 camera_display 0x1009a1b48 tokio::runtime::process::Driver::park_timeout::heb0a286fb4921f0c + 40
7 camera_display 0x1009c6328 tokio::runtime::driver::IoStack::park_timeout::ha15f8ffc5cab851b + 136
8 camera_display 0x1009dab84 tokio::runtime::time::Driver::park_thread_timeout::hdaf92e48edbd721d + 40
9 camera_display 0x1009da978 tokio::runtime::time::Driver::park_internal::h7c1fcf65c4343dc7 + 784
10 camera_display 0x1009da5a0 tokio::runtime::time::Driver::park::h0aa3206d3b05a462 + 40
11 camera_display 0x1009c678c tokio::runtime::driver::TimeDriver::park::h2589c02d0bf58b6c + 96
12 camera_display 0x1009c5c00 tokio::runtime::driver::Driver::park::h1bf58f7ae6ac8f16 + 32
13 camera_display 0x1009ed1d0 tokio::runtime::scheduler::multi_thread::park::Inner::park_driver::haaeede11d25027c4 + 120
14 camera_display 0x1009ecde0 tokio::runtime::scheduler::multi_thread::park::Inner::park::h3c3dbc8c396a0894 + 216
15 camera_display 0x1009ecae4 tokio::runtime::scheduler::multi_thread::park::Parker::park::h010fa0d10ce96e9b + 40
16 camera_display 0x1009d30d0 tokio::runtime::scheduler::multi_thread::worker::Context::park_timeout::h37500a9fc2ffb756 + 776
17 camera_display 0x1009d2bcc tokio::runtime::scheduler::multi_thread::worker::Context::park::hd6c55373d20068c0 + 968
18 camera_display 0x1009d1688 tokio::runtime::scheduler::multi_thread::worker::Context::run::h541c11fea84a38ec + 1784
19 camera_display 0x1009d0ed4 tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::h60671eb717a34c70 + 104
20 camera_display 0x1009e12c4 tokio::runtime::context::scoped::Scoped$LT$T$GT$::set::h263d3206a13c783c + 148
21 camera_display 0x1009c9a20 tokio::runtime::context::set_scheduler::_$u7b$$u7b$closure$u7d$$u7d$::h74a8e23df56c9882 + 40
22 camera*display 0x1009e84ec std::thread::local::LocalKey$LT$T$GT$::try_with::h2f963d543150c87c + 196
23 camera_display 0x1009e7f50 std::thread::local::LocalKey$LT$T$GT$::with::hbd17f06bbba52edc + 24
24 camera_display 0x1009c99ec tokio::runtime::context::set_scheduler::heb413530e9784dfc + 68
25 camera_display 0x1009d0df8 tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::h79a1aa630e49b68a + 248
26 camera*display 0x1009b8f14 tokio::runtime::context::runtime::enter_runtime::hb50775e529a9a617 + 188
27 camera_display 0x1009d0c58 tokio::runtime::scheduler::multi_thread::worker::run::h79855d48ee1e3c6a + 600
28 camera_display 0x1009d09f4 tokio::runtime::scheduler::multi_thread::worker::Launch::launch::*$u7b$$u7b$closure$u7d$$u7d$::he167c16b6e6899e4 + 24
29 camera*display 0x1009f453c *$LT$tokio..runtime..blocking..task..BlockingTask$LT$T$GT$$u20$as$u20$core..future..future..Future$GT$::poll::h8b9db1efbc4877eb + 136
30 camera*display 0x100992fa8 tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::*$u7b$$u7b$closure$u7d$$u7d$::h904262c7dc2f88b0 + 192
31 camera*display 0x100992d14 tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::h973d3c0095d258c4 + 72
32 camera_display 0x100991ed8 tokio::runtime::task::harness::poll_future::*$u7b$$u7b$closure$u7d$$u7d$::h8fa5ae0829102149 + 64
33 camera*display 0x1009b2ff4 *$LT$core..panic..unwind*safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2974776e7861df5d + 44
34 camera_display 0x1009a61b4 std::panicking::catch_unwind::do_call::h792e9a0912086d16 + 72
35 camera_display 0x1009a5e4c \_\_rust_try + 32
36 camera_display 0x1009a3110 std::panic::catch_unwind::h4474725978f361f4 + 96
37 camera_display 0x100991c70 tokio::runtime::task::harness::poll_future::hcf462599407f235c + 96
38 camera_display 0x1009909a8 tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll_inner::h5e6f168e02291d20 + 160
39 camera_display 0x100990878 tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll::h8a00a85251f71f0c + 28
40 camera_display 0x1009aa090 tokio::runtime::task::raw::poll::hf96de62ba33ad657 + 36
41 camera_display 0x1009a9db0 tokio::runtime::task::raw::RawTask::poll::hde83a8d818768542 + 52
42 camera_display 0x1009b1ec8 tokio::runtime::task::UnownedTask$LT$S$GT$::run::h7ce7c13c00052430 + 64
43 camera_display 0x1009bb178 tokio::runtime::blocking::pool::Task::run::hf56d840fd68c7fdc + 28
44 camera_display 0x1009bcdb0 tokio::runtime::blocking::pool::Inner::run::hd581e97506257439 + 512
45 camera_display 0x1009bcb1c tokio::runtime::blocking::pool::Spawner::spawn_thread::*$u7b$$u7b$closure$u7d$$u7d$::h2a632d4852cdd40e + 144
46 camera*display 0x1009ef10c std::sys::backtrace::\_\_rust_begin_short_backtrace::h51232d61e41dc60c + 16
47 camera_display 0x1009b4600 std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::hbf84c81d58c2715d + 116
48 camera*display 0x1009b302c *$LT$core..panic..unwind*safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2c0e201eb4dddbb7 + 44
49 camera_display 0x1009a6210 std::panicking::catch_unwind::do_call::h94ffcffc7cdce087 + 68
50 camera_display 0x1009b9364 \_\_rust_try + 32
51 camera_display 0x1009b4154 std::thread::Builder::spawn_unchecked*::\_$u7b$$u7b$closure$u7d$$u7d$::haea346fd331ce539 + 728
52 camera_display 0x1009946b4 core::ops::function::FnOnce::call_once$u7b$$u7b$vtable.shim$u7d$$u7d$::h12a95acb393be819 + 24
53 camera_display 0x100a9c560 std::sys::pal::unix::thread::Thread::new::thread_start::h636d41ad3c63ac40 + 56
54 libsystem_pthread.dylib 0x1814f7c0c \_pthread_start + 136
55 libsystem_pthread.dylib 0x1814f2b80 thread_start + 8

Thread 5:: tokio-runtime-worker
0 libsystem*kernel.dylib 0x1814b93cc \_\_psynch_cvwait + 8
1 libsystem_pthread.dylib 0x1814f80e0 \_pthread_cond_wait + 984
2 camera_display 0x100a6acfc *$LT$parking*lot_core..thread_parker..imp..ThreadParker$u20$as$u20$parking_lot_core..thread_parker..ThreadParkerT$GT$::park::h0e2d0998749974de + 256
3 camera_display 0x100a630e8 parking_lot_core::parking_lot::park::*$u7b$$u7b$closure$u7d$$u7d$::h0b4fc83101e8557f + 748
4 camera*display 0x100a62c10 parking_lot_core::parking_lot::park::hd04bbb424560662a + 296
5 camera_display 0x100a6a268 parking_lot::condvar::Condvar::wait_until_internal::h8640b2ba350582ed + 124
6 camera_display 0x1009b32e0 parking_lot::condvar::Condvar::wait::h6618c4600aa6d512 + 68
7 camera_display 0x1009f2e1c tokio::loom::std::parking_lot::Condvar::wait::hc858434e42aaccb1 + 36
8 camera_display 0x1009ecf04 tokio::runtime::scheduler::multi_thread::park::Inner::park_condvar::h2267f8a57982d137 + 264
9 camera_display 0x1009ecdb0 tokio::runtime::scheduler::multi_thread::park::Inner::park::h3c3dbc8c396a0894 + 168
10 camera_display 0x1009ecae4 tokio::runtime::scheduler::multi_thread::park::Parker::park::h010fa0d10ce96e9b + 40
11 camera_display 0x1009d30d0 tokio::runtime::scheduler::multi_thread::worker::Context::park_timeout::h37500a9fc2ffb756 + 776
12 camera_display 0x1009d2bcc tokio::runtime::scheduler::multi_thread::worker::Context::park::hd6c55373d20068c0 + 968
13 camera_display 0x1009d1688 tokio::runtime::scheduler::multi_thread::worker::Context::run::h541c11fea84a38ec + 1784
14 camera_display 0x1009d0ed4 tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::h60671eb717a34c70 + 104
15 camera_display 0x1009e12c4 tokio::runtime::context::scoped::Scoped$LT$T$GT$::set::h263d3206a13c783c + 148
16 camera_display 0x1009c9a20 tokio::runtime::context::set_scheduler::_$u7b$$u7b$closure$u7d$$u7d$::h74a8e23df56c9882 + 40
17 camera*display 0x1009e84ec std::thread::local::LocalKey$LT$T$GT$::try_with::h2f963d543150c87c + 196
18 camera_display 0x1009e7f50 std::thread::local::LocalKey$LT$T$GT$::with::hbd17f06bbba52edc + 24
19 camera_display 0x1009c99ec tokio::runtime::context::set_scheduler::heb413530e9784dfc + 68
20 camera_display 0x1009d0df8 tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::h79a1aa630e49b68a + 248
21 camera*display 0x1009b8f14 tokio::runtime::context::runtime::enter_runtime::hb50775e529a9a617 + 188
22 camera_display 0x1009d0c58 tokio::runtime::scheduler::multi_thread::worker::run::h79855d48ee1e3c6a + 600
23 camera_display 0x1009d09f4 tokio::runtime::scheduler::multi_thread::worker::Launch::launch::*$u7b$$u7b$closure$u7d$$u7d$::he167c16b6e6899e4 + 24
24 camera*display 0x1009f453c *$LT$tokio..runtime..blocking..task..BlockingTask$LT$T$GT$$u20$as$u20$core..future..future..Future$GT$::poll::h8b9db1efbc4877eb + 136
25 camera*display 0x100992fa8 tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::*$u7b$$u7b$closure$u7d$$u7d$::h904262c7dc2f88b0 + 192
26 camera*display 0x100992d14 tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::h973d3c0095d258c4 + 72
27 camera_display 0x100991ed8 tokio::runtime::task::harness::poll_future::*$u7b$$u7b$closure$u7d$$u7d$::h8fa5ae0829102149 + 64
28 camera*display 0x1009b2ff4 *$LT$core..panic..unwind*safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2974776e7861df5d + 44
29 camera_display 0x1009a61b4 std::panicking::catch_unwind::do_call::h792e9a0912086d16 + 72
30 camera_display 0x1009a5e4c \_\_rust_try + 32
31 camera_display 0x1009a3110 std::panic::catch_unwind::h4474725978f361f4 + 96
32 camera_display 0x100991c70 tokio::runtime::task::harness::poll_future::hcf462599407f235c + 96
33 camera_display 0x1009909a8 tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll_inner::h5e6f168e02291d20 + 160
34 camera_display 0x100990878 tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll::h8a00a85251f71f0c + 28
35 camera_display 0x1009aa090 tokio::runtime::task::raw::poll::hf96de62ba33ad657 + 36
36 camera_display 0x1009a9db0 tokio::runtime::task::raw::RawTask::poll::hde83a8d818768542 + 52
37 camera_display 0x1009b1ec8 tokio::runtime::task::UnownedTask$LT$S$GT$::run::h7ce7c13c00052430 + 64
38 camera_display 0x1009bb178 tokio::runtime::blocking::pool::Task::run::hf56d840fd68c7fdc + 28
39 camera_display 0x1009bcdb0 tokio::runtime::blocking::pool::Inner::run::hd581e97506257439 + 512
40 camera_display 0x1009bcb1c tokio::runtime::blocking::pool::Spawner::spawn_thread::*$u7b$$u7b$closure$u7d$$u7d$::h2a632d4852cdd40e + 144
41 camera*display 0x1009ef10c std::sys::backtrace::\_\_rust_begin_short_backtrace::h51232d61e41dc60c + 16
42 camera_display 0x1009b4600 std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::hbf84c81d58c2715d + 116
43 camera*display 0x1009b302c *$LT$core..panic..unwind*safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2c0e201eb4dddbb7 + 44
44 camera_display 0x1009a6210 std::panicking::catch_unwind::do_call::h94ffcffc7cdce087 + 68
45 camera_display 0x1009b9364 \_\_rust_try + 32
46 camera_display 0x1009b4154 std::thread::Builder::spawn_unchecked*::\_$u7b$$u7b$closure$u7d$$u7d$::haea346fd331ce539 + 728
47 camera_display 0x1009946b4 core::ops::function::FnOnce::call_once$u7b$$u7b$vtable.shim$u7d$$u7d$::h12a95acb393be819 + 24
48 camera_display 0x100a9c560 std::sys::pal::unix::thread::Thread::new::thread_start::h636d41ad3c63ac40 + 56
49 libsystem_pthread.dylib 0x1814f7c0c \_pthread_start + 136
50 libsystem_pthread.dylib 0x1814f2b80 thread_start + 8

Thread 6:: tokio-runtime-worker
0 libsystem*kernel.dylib 0x1814b93cc \_\_psynch_cvwait + 8
1 libsystem_pthread.dylib 0x1814f80e0 \_pthread_cond_wait + 984
2 camera_display 0x100a6acfc *$LT$parking*lot_core..thread_parker..imp..ThreadParker$u20$as$u20$parking_lot_core..thread_parker..ThreadParkerT$GT$::park::h0e2d0998749974de + 256
3 camera_display 0x100a630e8 parking_lot_core::parking_lot::park::*$u7b$$u7b$closure$u7d$$u7d$::h0b4fc83101e8557f + 748
4 camera*display 0x100a62c10 parking_lot_core::parking_lot::park::hd04bbb424560662a + 296
5 camera_display 0x100a6a268 parking_lot::condvar::Condvar::wait_until_internal::h8640b2ba350582ed + 124
6 camera_display 0x1009b32e0 parking_lot::condvar::Condvar::wait::h6618c4600aa6d512 + 68
7 camera_display 0x1009f2e1c tokio::loom::std::parking_lot::Condvar::wait::hc858434e42aaccb1 + 36
8 camera_display 0x1009ecf04 tokio::runtime::scheduler::multi_thread::park::Inner::park_condvar::h2267f8a57982d137 + 264
9 camera_display 0x1009ecdb0 tokio::runtime::scheduler::multi_thread::park::Inner::park::h3c3dbc8c396a0894 + 168
10 camera_display 0x1009ecae4 tokio::runtime::scheduler::multi_thread::park::Parker::park::h010fa0d10ce96e9b + 40
11 camera_display 0x1009d30d0 tokio::runtime::scheduler::multi_thread::worker::Context::park_timeout::h37500a9fc2ffb756 + 776
12 camera_display 0x1009d2bcc tokio::runtime::scheduler::multi_thread::worker::Context::park::hd6c55373d20068c0 + 968
13 camera_display 0x1009d1688 tokio::runtime::scheduler::multi_thread::worker::Context::run::h541c11fea84a38ec + 1784
14 camera_display 0x1009d0ed4 tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::h60671eb717a34c70 + 104
15 camera_display 0x1009e12c4 tokio::runtime::context::scoped::Scoped$LT$T$GT$::set::h263d3206a13c783c + 148
16 camera_display 0x1009c9a20 tokio::runtime::context::set_scheduler::_$u7b$$u7b$closure$u7d$$u7d$::h74a8e23df56c9882 + 40
17 camera*display 0x1009e84ec std::thread::local::LocalKey$LT$T$GT$::try_with::h2f963d543150c87c + 196
18 camera_display 0x1009e7f50 std::thread::local::LocalKey$LT$T$GT$::with::hbd17f06bbba52edc + 24
19 camera_display 0x1009c99ec tokio::runtime::context::set_scheduler::heb413530e9784dfc + 68
20 camera_display 0x1009d0df8 tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::h79a1aa630e49b68a + 248
21 camera*display 0x1009b8f14 tokio::runtime::context::runtime::enter_runtime::hb50775e529a9a617 + 188
22 camera_display 0x1009d0c58 tokio::runtime::scheduler::multi_thread::worker::run::h79855d48ee1e3c6a + 600
23 camera_display 0x1009d09f4 tokio::runtime::scheduler::multi_thread::worker::Launch::launch::*$u7b$$u7b$closure$u7d$$u7d$::he167c16b6e6899e4 + 24
24 camera*display 0x1009f453c *$LT$tokio..runtime..blocking..task..BlockingTask$LT$T$GT$$u20$as$u20$core..future..future..Future$GT$::poll::h8b9db1efbc4877eb + 136
25 camera*display 0x100992fa8 tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::*$u7b$$u7b$closure$u7d$$u7d$::h904262c7dc2f88b0 + 192
26 camera*display 0x100992d14 tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::h973d3c0095d258c4 + 72
27 camera_display 0x100991ed8 tokio::runtime::task::harness::poll_future::*$u7b$$u7b$closure$u7d$$u7d$::h8fa5ae0829102149 + 64
28 camera*display 0x1009b2ff4 *$LT$core..panic..unwind*safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2974776e7861df5d + 44
29 camera_display 0x1009a61b4 std::panicking::catch_unwind::do_call::h792e9a0912086d16 + 72
30 camera_display 0x1009a5e4c \_\_rust_try + 32
31 camera_display 0x1009a3110 std::panic::catch_unwind::h4474725978f361f4 + 96
32 camera_display 0x100991c70 tokio::runtime::task::harness::poll_future::hcf462599407f235c + 96
33 camera_display 0x1009909a8 tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll_inner::h5e6f168e02291d20 + 160
34 camera_display 0x100990878 tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll::h8a00a85251f71f0c + 28
35 camera_display 0x1009aa090 tokio::runtime::task::raw::poll::hf96de62ba33ad657 + 36
36 camera_display 0x1009a9db0 tokio::runtime::task::raw::RawTask::poll::hde83a8d818768542 + 52
37 camera_display 0x1009b1ec8 tokio::runtime::task::UnownedTask$LT$S$GT$::run::h7ce7c13c00052430 + 64
38 camera_display 0x1009bb178 tokio::runtime::blocking::pool::Task::run::hf56d840fd68c7fdc + 28
39 camera_display 0x1009bcdb0 tokio::runtime::blocking::pool::Inner::run::hd581e97506257439 + 512
40 camera_display 0x1009bcb1c tokio::runtime::blocking::pool::Spawner::spawn_thread::*$u7b$$u7b$closure$u7d$$u7d$::h2a632d4852cdd40e + 144
41 camera*display 0x1009ef10c std::sys::backtrace::\_\_rust_begin_short_backtrace::h51232d61e41dc60c + 16
42 camera_display 0x1009b4600 std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::hbf84c81d58c2715d + 116
43 camera*display 0x1009b302c *$LT$core..panic..unwind*safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2c0e201eb4dddbb7 + 44
44 camera_display 0x1009a6210 std::panicking::catch_unwind::do_call::h94ffcffc7cdce087 + 68
45 camera_display 0x1009b9364 \_\_rust_try + 32
46 camera_display 0x1009b4154 std::thread::Builder::spawn_unchecked*::\_$u7b$$u7b$closure$u7d$$u7d$::haea346fd331ce539 + 728
47 camera_display 0x1009946b4 core::ops::function::FnOnce::call_once$u7b$$u7b$vtable.shim$u7d$$u7d$::h12a95acb393be819 + 24
48 camera_display 0x100a9c560 std::sys::pal::unix::thread::Thread::new::thread_start::h636d41ad3c63ac40 + 56
49 libsystem_pthread.dylib 0x1814f7c0c \_pthread_start + 136
50 libsystem_pthread.dylib 0x1814f2b80 thread_start + 8

Thread 7:: tokio-runtime-worker
0 libsystem*kernel.dylib 0x1814b93cc \_\_psynch_cvwait + 8
1 libsystem_pthread.dylib 0x1814f80e0 \_pthread_cond_wait + 984
2 camera_display 0x100a6acfc *$LT$parking*lot_core..thread_parker..imp..ThreadParker$u20$as$u20$parking_lot_core..thread_parker..ThreadParkerT$GT$::park::h0e2d0998749974de + 256
3 camera_display 0x100a630e8 parking_lot_core::parking_lot::park::*$u7b$$u7b$closure$u7d$$u7d$::h0b4fc83101e8557f + 748
4 camera*display 0x100a62c10 parking_lot_core::parking_lot::park::hd04bbb424560662a + 296
5 camera_display 0x100a6a268 parking_lot::condvar::Condvar::wait_until_internal::h8640b2ba350582ed + 124
6 camera_display 0x1009b32e0 parking_lot::condvar::Condvar::wait::h6618c4600aa6d512 + 68
7 camera_display 0x1009f2e1c tokio::loom::std::parking_lot::Condvar::wait::hc858434e42aaccb1 + 36
8 camera_display 0x1009ecf04 tokio::runtime::scheduler::multi_thread::park::Inner::park_condvar::h2267f8a57982d137 + 264
9 camera_display 0x1009ecdb0 tokio::runtime::scheduler::multi_thread::park::Inner::park::h3c3dbc8c396a0894 + 168
10 camera_display 0x1009ecae4 tokio::runtime::scheduler::multi_thread::park::Parker::park::h010fa0d10ce96e9b + 40
11 camera_display 0x1009d30d0 tokio::runtime::scheduler::multi_thread::worker::Context::park_timeout::h37500a9fc2ffb756 + 776
12 camera_display 0x1009d2bcc tokio::runtime::scheduler::multi_thread::worker::Context::park::hd6c55373d20068c0 + 968
13 camera_display 0x1009d1688 tokio::runtime::scheduler::multi_thread::worker::Context::run::h541c11fea84a38ec + 1784
14 camera_display 0x1009d0ed4 tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::h60671eb717a34c70 + 104
15 camera_display 0x1009e12c4 tokio::runtime::context::scoped::Scoped$LT$T$GT$::set::h263d3206a13c783c + 148
16 camera_display 0x1009c9a20 tokio::runtime::context::set_scheduler::_$u7b$$u7b$closure$u7d$$u7d$::h74a8e23df56c9882 + 40
17 camera*display 0x1009e84ec std::thread::local::LocalKey$LT$T$GT$::try_with::h2f963d543150c87c + 196
18 camera_display 0x1009e7f50 std::thread::local::LocalKey$LT$T$GT$::with::hbd17f06bbba52edc + 24
19 camera_display 0x1009c99ec tokio::runtime::context::set_scheduler::heb413530e9784dfc + 68
20 camera_display 0x1009d0df8 tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::h79a1aa630e49b68a + 248
21 camera*display 0x1009b8f14 tokio::runtime::context::runtime::enter_runtime::hb50775e529a9a617 + 188
22 camera_display 0x1009d0c58 tokio::runtime::scheduler::multi_thread::worker::run::h79855d48ee1e3c6a + 600
23 camera_display 0x1009d09f4 tokio::runtime::scheduler::multi_thread::worker::Launch::launch::*$u7b$$u7b$closure$u7d$$u7d$::he167c16b6e6899e4 + 24
24 camera*display 0x1009f453c *$LT$tokio..runtime..blocking..task..BlockingTask$LT$T$GT$$u20$as$u20$core..future..future..Future$GT$::poll::h8b9db1efbc4877eb + 136
25 camera*display 0x100992fa8 tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::*$u7b$$u7b$closure$u7d$$u7d$::h904262c7dc2f88b0 + 192
26 camera*display 0x100992d14 tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::h973d3c0095d258c4 + 72
27 camera_display 0x100991ed8 tokio::runtime::task::harness::poll_future::*$u7b$$u7b$closure$u7d$$u7d$::h8fa5ae0829102149 + 64
28 camera*display 0x1009b2ff4 *$LT$core..panic..unwind*safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2974776e7861df5d + 44
29 camera_display 0x1009a61b4 std::panicking::catch_unwind::do_call::h792e9a0912086d16 + 72
30 camera_display 0x1009a5e4c \_\_rust_try + 32
31 camera_display 0x1009a3110 std::panic::catch_unwind::h4474725978f361f4 + 96
32 camera_display 0x100991c70 tokio::runtime::task::harness::poll_future::hcf462599407f235c + 96
33 camera_display 0x1009909a8 tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll_inner::h5e6f168e02291d20 + 160
34 camera_display 0x100990878 tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll::h8a00a85251f71f0c + 28
35 camera_display 0x1009aa090 tokio::runtime::task::raw::poll::hf96de62ba33ad657 + 36
36 camera_display 0x1009a9db0 tokio::runtime::task::raw::RawTask::poll::hde83a8d818768542 + 52
37 camera_display 0x1009b1ec8 tokio::runtime::task::UnownedTask$LT$S$GT$::run::h7ce7c13c00052430 + 64
38 camera_display 0x1009bb178 tokio::runtime::blocking::pool::Task::run::hf56d840fd68c7fdc + 28
39 camera_display 0x1009bcdb0 tokio::runtime::blocking::pool::Inner::run::hd581e97506257439 + 512
40 camera_display 0x1009bcb1c tokio::runtime::blocking::pool::Spawner::spawn_thread::*$u7b$$u7b$closure$u7d$$u7d$::h2a632d4852cdd40e + 144
41 camera*display 0x1009ef10c std::sys::backtrace::\_\_rust_begin_short_backtrace::h51232d61e41dc60c + 16
42 camera_display 0x1009b4600 std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::hbf84c81d58c2715d + 116
43 camera*display 0x1009b302c *$LT$core..panic..unwind*safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2c0e201eb4dddbb7 + 44
44 camera_display 0x1009a6210 std::panicking::catch_unwind::do_call::h94ffcffc7cdce087 + 68
45 camera_display 0x1009b9364 \_\_rust_try + 32
46 camera_display 0x1009b4154 std::thread::Builder::spawn_unchecked*::\_$u7b$$u7b$closure$u7d$$u7d$::haea346fd331ce539 + 728
47 camera_display 0x1009946b4 core::ops::function::FnOnce::call_once$u7b$$u7b$vtable.shim$u7d$$u7d$::h12a95acb393be819 + 24
48 camera_display 0x100a9c560 std::sys::pal::unix::thread::Thread::new::thread_start::h636d41ad3c63ac40 + 56
49 libsystem_pthread.dylib 0x1814f7c0c \_pthread_start + 136
50 libsystem_pthread.dylib 0x1814f2b80 thread_start + 8

Thread 8:: tokio-runtime-worker
0 libsystem*kernel.dylib 0x1814b93cc \_\_psynch_cvwait + 8
1 libsystem_pthread.dylib 0x1814f80e0 \_pthread_cond_wait + 984
2 camera_display 0x100a6acfc *$LT$parking*lot_core..thread_parker..imp..ThreadParker$u20$as$u20$parking_lot_core..thread_parker..ThreadParkerT$GT$::park::h0e2d0998749974de + 256
3 camera_display 0x100a630e8 parking_lot_core::parking_lot::park::*$u7b$$u7b$closure$u7d$$u7d$::h0b4fc83101e8557f + 748
4 camera*display 0x100a62c10 parking_lot_core::parking_lot::park::hd04bbb424560662a + 296
5 camera_display 0x100a6a268 parking_lot::condvar::Condvar::wait_until_internal::h8640b2ba350582ed + 124
6 camera_display 0x1009b32e0 parking_lot::condvar::Condvar::wait::h6618c4600aa6d512 + 68
7 camera_display 0x1009f2e1c tokio::loom::std::parking_lot::Condvar::wait::hc858434e42aaccb1 + 36
8 camera_display 0x1009ecf04 tokio::runtime::scheduler::multi_thread::park::Inner::park_condvar::h2267f8a57982d137 + 264
9 camera_display 0x1009ecdb0 tokio::runtime::scheduler::multi_thread::park::Inner::park::h3c3dbc8c396a0894 + 168
10 camera_display 0x1009ecae4 tokio::runtime::scheduler::multi_thread::park::Parker::park::h010fa0d10ce96e9b + 40
11 camera_display 0x1009d30d0 tokio::runtime::scheduler::multi_thread::worker::Context::park_timeout::h37500a9fc2ffb756 + 776
12 camera_display 0x1009d2bcc tokio::runtime::scheduler::multi_thread::worker::Context::park::hd6c55373d20068c0 + 968
13 camera_display 0x1009d1688 tokio::runtime::scheduler::multi_thread::worker::Context::run::h541c11fea84a38ec + 1784
14 camera_display 0x1009d0ed4 tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::h60671eb717a34c70 + 104
15 camera_display 0x1009e12c4 tokio::runtime::context::scoped::Scoped$LT$T$GT$::set::h263d3206a13c783c + 148
16 camera_display 0x1009c9a20 tokio::runtime::context::set_scheduler::_$u7b$$u7b$closure$u7d$$u7d$::h74a8e23df56c9882 + 40
17 camera*display 0x1009e84ec std::thread::local::LocalKey$LT$T$GT$::try_with::h2f963d543150c87c + 196
18 camera_display 0x1009e7f50 std::thread::local::LocalKey$LT$T$GT$::with::hbd17f06bbba52edc + 24
19 camera_display 0x1009c99ec tokio::runtime::context::set_scheduler::heb413530e9784dfc + 68
20 camera_display 0x1009d0df8 tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::h79a1aa630e49b68a + 248
21 camera*display 0x1009b8f14 tokio::runtime::context::runtime::enter_runtime::hb50775e529a9a617 + 188
22 camera_display 0x1009d0c58 tokio::runtime::scheduler::multi_thread::worker::run::h79855d48ee1e3c6a + 600
23 camera_display 0x1009d09f4 tokio::runtime::scheduler::multi_thread::worker::Launch::launch::*$u7b$$u7b$closure$u7d$$u7d$::he167c16b6e6899e4 + 24
24 camera*display 0x1009f453c *$LT$tokio..runtime..blocking..task..BlockingTask$LT$T$GT$$u20$as$u20$core..future..future..Future$GT$::poll::h8b9db1efbc4877eb + 136
25 camera*display 0x100992fa8 tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::*$u7b$$u7b$closure$u7d$$u7d$::h904262c7dc2f88b0 + 192
26 camera*display 0x100992d14 tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::h973d3c0095d258c4 + 72
27 camera_display 0x100991ed8 tokio::runtime::task::harness::poll_future::*$u7b$$u7b$closure$u7d$$u7d$::h8fa5ae0829102149 + 64
28 camera*display 0x1009b2ff4 *$LT$core..panic..unwind*safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2974776e7861df5d + 44
29 camera_display 0x1009a61b4 std::panicking::catch_unwind::do_call::h792e9a0912086d16 + 72
30 camera_display 0x1009a5e4c \_\_rust_try + 32
31 camera_display 0x1009a3110 std::panic::catch_unwind::h4474725978f361f4 + 96
32 camera_display 0x100991c70 tokio::runtime::task::harness::poll_future::hcf462599407f235c + 96
33 camera_display 0x1009909a8 tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll_inner::h5e6f168e02291d20 + 160
34 camera_display 0x100990878 tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll::h8a00a85251f71f0c + 28
35 camera_display 0x1009aa090 tokio::runtime::task::raw::poll::hf96de62ba33ad657 + 36
36 camera_display 0x1009a9db0 tokio::runtime::task::raw::RawTask::poll::hde83a8d818768542 + 52
37 camera_display 0x1009b1ec8 tokio::runtime::task::UnownedTask$LT$S$GT$::run::h7ce7c13c00052430 + 64
38 camera_display 0x1009bb178 tokio::runtime::blocking::pool::Task::run::hf56d840fd68c7fdc + 28
39 camera_display 0x1009bcdb0 tokio::runtime::blocking::pool::Inner::run::hd581e97506257439 + 512
40 camera_display 0x1009bcb1c tokio::runtime::blocking::pool::Spawner::spawn_thread::*$u7b$$u7b$closure$u7d$$u7d$::h2a632d4852cdd40e + 144
41 camera*display 0x1009ef10c std::sys::backtrace::\_\_rust_begin_short_backtrace::h51232d61e41dc60c + 16
42 camera_display 0x1009b4600 std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::hbf84c81d58c2715d + 116
43 camera*display 0x1009b302c *$LT$core..panic..unwind*safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2c0e201eb4dddbb7 + 44
44 camera_display 0x1009a6210 std::panicking::catch_unwind::do_call::h94ffcffc7cdce087 + 68
45 camera_display 0x1009b9364 \_\_rust_try + 32
46 camera_display 0x1009b4154 std::thread::Builder::spawn_unchecked*::\_$u7b$$u7b$closure$u7d$$u7d$::haea346fd331ce539 + 728
47 camera_display 0x1009946b4 core::ops::function::FnOnce::call_once$u7b$$u7b$vtable.shim$u7d$$u7d$::h12a95acb393be819 + 24
48 camera_display 0x100a9c560 std::sys::pal::unix::thread::Thread::new::thread_start::h636d41ad3c63ac40 + 56
49 libsystem_pthread.dylib 0x1814f7c0c \_pthread_start + 136
50 libsystem_pthread.dylib 0x1814f2b80 thread_start + 8

Thread 9:: tokio-runtime-worker
0 libsystem*kernel.dylib 0x1814b93cc \_\_psynch_cvwait + 8
1 libsystem_pthread.dylib 0x1814f80e0 \_pthread_cond_wait + 984
2 camera_display 0x100a6acfc *$LT$parking*lot_core..thread_parker..imp..ThreadParker$u20$as$u20$parking_lot_core..thread_parker..ThreadParkerT$GT$::park::h0e2d0998749974de + 256
3 camera_display 0x100a630e8 parking_lot_core::parking_lot::park::*$u7b$$u7b$closure$u7d$$u7d$::h0b4fc83101e8557f + 748
4 camera*display 0x100a62c10 parking_lot_core::parking_lot::park::hd04bbb424560662a + 296
5 camera_display 0x100a6a268 parking_lot::condvar::Condvar::wait_until_internal::h8640b2ba350582ed + 124
6 camera_display 0x1009b32e0 parking_lot::condvar::Condvar::wait::h6618c4600aa6d512 + 68
7 camera_display 0x1009f2e1c tokio::loom::std::parking_lot::Condvar::wait::hc858434e42aaccb1 + 36
8 camera_display 0x1009ecf04 tokio::runtime::scheduler::multi_thread::park::Inner::park_condvar::h2267f8a57982d137 + 264
9 camera_display 0x1009ecdb0 tokio::runtime::scheduler::multi_thread::park::Inner::park::h3c3dbc8c396a0894 + 168
10 camera_display 0x1009ecae4 tokio::runtime::scheduler::multi_thread::park::Parker::park::h010fa0d10ce96e9b + 40
11 camera_display 0x1009d30d0 tokio::runtime::scheduler::multi_thread::worker::Context::park_timeout::h37500a9fc2ffb756 + 776
12 camera_display 0x1009d2bcc tokio::runtime::scheduler::multi_thread::worker::Context::park::hd6c55373d20068c0 + 968
13 camera_display 0x1009d1688 tokio::runtime::scheduler::multi_thread::worker::Context::run::h541c11fea84a38ec + 1784
14 camera_display 0x1009d0ed4 tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::h60671eb717a34c70 + 104
15 camera_display 0x1009e12c4 tokio::runtime::context::scoped::Scoped$LT$T$GT$::set::h263d3206a13c783c + 148
16 camera_display 0x1009c9a20 tokio::runtime::context::set_scheduler::_$u7b$$u7b$closure$u7d$$u7d$::h74a8e23df56c9882 + 40
17 camera*display 0x1009e84ec std::thread::local::LocalKey$LT$T$GT$::try_with::h2f963d543150c87c + 196
18 camera_display 0x1009e7f50 std::thread::local::LocalKey$LT$T$GT$::with::hbd17f06bbba52edc + 24
19 camera_display 0x1009c99ec tokio::runtime::context::set_scheduler::heb413530e9784dfc + 68
20 camera_display 0x1009d0df8 tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::h79a1aa630e49b68a + 248
21 camera*display 0x1009b8f14 tokio::runtime::context::runtime::enter_runtime::hb50775e529a9a617 + 188
22 camera_display 0x1009d0c58 tokio::runtime::scheduler::multi_thread::worker::run::h79855d48ee1e3c6a + 600
23 camera_display 0x1009d09f4 tokio::runtime::scheduler::multi_thread::worker::Launch::launch::*$u7b$$u7b$closure$u7d$$u7d$::he167c16b6e6899e4 + 24
24 camera*display 0x1009f453c *$LT$tokio..runtime..blocking..task..BlockingTask$LT$T$GT$$u20$as$u20$core..future..future..Future$GT$::poll::h8b9db1efbc4877eb + 136
25 camera*display 0x100992fa8 tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::*$u7b$$u7b$closure$u7d$$u7d$::h904262c7dc2f88b0 + 192
26 camera*display 0x100992d14 tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::h973d3c0095d258c4 + 72
27 camera_display 0x100991ed8 tokio::runtime::task::harness::poll_future::*$u7b$$u7b$closure$u7d$$u7d$::h8fa5ae0829102149 + 64
28 camera*display 0x1009b2ff4 *$LT$core..panic..unwind*safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2974776e7861df5d + 44
29 camera_display 0x1009a61b4 std::panicking::catch_unwind::do_call::h792e9a0912086d16 + 72
30 camera_display 0x1009a5e4c \_\_rust_try + 32
31 camera_display 0x1009a3110 std::panic::catch_unwind::h4474725978f361f4 + 96
32 camera_display 0x100991c70 tokio::runtime::task::harness::poll_future::hcf462599407f235c + 96
33 camera_display 0x1009909a8 tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll_inner::h5e6f168e02291d20 + 160
34 camera_display 0x100990878 tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll::h8a00a85251f71f0c + 28
35 camera_display 0x1009aa090 tokio::runtime::task::raw::poll::hf96de62ba33ad657 + 36
36 camera_display 0x1009a9db0 tokio::runtime::task::raw::RawTask::poll::hde83a8d818768542 + 52
37 camera_display 0x1009b1ec8 tokio::runtime::task::UnownedTask$LT$S$GT$::run::h7ce7c13c00052430 + 64
38 camera_display 0x1009bb178 tokio::runtime::blocking::pool::Task::run::hf56d840fd68c7fdc + 28
39 camera_display 0x1009bcdb0 tokio::runtime::blocking::pool::Inner::run::hd581e97506257439 + 512
40 camera_display 0x1009bcb1c tokio::runtime::blocking::pool::Spawner::spawn_thread::*$u7b$$u7b$closure$u7d$$u7d$::h2a632d4852cdd40e + 144
41 camera*display 0x1009ef10c std::sys::backtrace::\_\_rust_begin_short_backtrace::h51232d61e41dc60c + 16
42 camera_display 0x1009b4600 std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::hbf84c81d58c2715d + 116
43 camera*display 0x1009b302c *$LT$core..panic..unwind*safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2c0e201eb4dddbb7 + 44
44 camera_display 0x1009a6210 std::panicking::catch_unwind::do_call::h94ffcffc7cdce087 + 68
45 camera_display 0x1009b9364 \_\_rust_try + 32
46 camera_display 0x1009b4154 std::thread::Builder::spawn_unchecked*::\_$u7b$$u7b$closure$u7d$$u7d$::haea346fd331ce539 + 728
47 camera_display 0x1009946b4 core::ops::function::FnOnce::call_once$u7b$$u7b$vtable.shim$u7d$$u7d$::h12a95acb393be819 + 24
48 camera_display 0x100a9c560 std::sys::pal::unix::thread::Thread::new::thread_start::h636d41ad3c63ac40 + 56
49 libsystem_pthread.dylib 0x1814f7c0c \_pthread_start + 136
50 libsystem_pthread.dylib 0x1814f2b80 thread_start + 8

Thread 10:: tokio-runtime-worker
0 libsystem*kernel.dylib 0x1814b93cc \_\_psynch_cvwait + 8
1 libsystem_pthread.dylib 0x1814f80e0 \_pthread_cond_wait + 984
2 camera_display 0x100a6acfc *$LT$parking*lot_core..thread_parker..imp..ThreadParker$u20$as$u20$parking_lot_core..thread_parker..ThreadParkerT$GT$::park::h0e2d0998749974de + 256
3 camera_display 0x100a630e8 parking_lot_core::parking_lot::park::*$u7b$$u7b$closure$u7d$$u7d$::h0b4fc83101e8557f + 748
4 camera*display 0x100a62c10 parking_lot_core::parking_lot::park::hd04bbb424560662a + 296
5 camera_display 0x100a6a268 parking_lot::condvar::Condvar::wait_until_internal::h8640b2ba350582ed + 124
6 camera_display 0x1009b32e0 parking_lot::condvar::Condvar::wait::h6618c4600aa6d512 + 68
7 camera_display 0x1009f2e1c tokio::loom::std::parking_lot::Condvar::wait::hc858434e42aaccb1 + 36
8 camera_display 0x1009ecf04 tokio::runtime::scheduler::multi_thread::park::Inner::park_condvar::h2267f8a57982d137 + 264
9 camera_display 0x1009ecdb0 tokio::runtime::scheduler::multi_thread::park::Inner::park::h3c3dbc8c396a0894 + 168
10 camera_display 0x1009ecae4 tokio::runtime::scheduler::multi_thread::park::Parker::park::h010fa0d10ce96e9b + 40
11 camera_display 0x1009d30d0 tokio::runtime::scheduler::multi_thread::worker::Context::park_timeout::h37500a9fc2ffb756 + 776
12 camera_display 0x1009d2bcc tokio::runtime::scheduler::multi_thread::worker::Context::park::hd6c55373d20068c0 + 968
13 camera_display 0x1009d1688 tokio::runtime::scheduler::multi_thread::worker::Context::run::h541c11fea84a38ec + 1784
14 camera_display 0x1009d0ed4 tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::h60671eb717a34c70 + 104
15 camera_display 0x1009e12c4 tokio::runtime::context::scoped::Scoped$LT$T$GT$::set::h263d3206a13c783c + 148
16 camera_display 0x1009c9a20 tokio::runtime::context::set_scheduler::_$u7b$$u7b$closure$u7d$$u7d$::h74a8e23df56c9882 + 40
17 camera*display 0x1009e84ec std::thread::local::LocalKey$LT$T$GT$::try_with::h2f963d543150c87c + 196
18 camera_display 0x1009e7f50 std::thread::local::LocalKey$LT$T$GT$::with::hbd17f06bbba52edc + 24
19 camera_display 0x1009c99ec tokio::runtime::context::set_scheduler::heb413530e9784dfc + 68
20 camera_display 0x1009d0df8 tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::h79a1aa630e49b68a + 248
21 camera*display 0x1009b8f14 tokio::runtime::context::runtime::enter_runtime::hb50775e529a9a617 + 188
22 camera_display 0x1009d0c58 tokio::runtime::scheduler::multi_thread::worker::run::h79855d48ee1e3c6a + 600
23 camera_display 0x1009d09f4 tokio::runtime::scheduler::multi_thread::worker::Launch::launch::*$u7b$$u7b$closure$u7d$$u7d$::he167c16b6e6899e4 + 24
24 camera*display 0x1009f453c *$LT$tokio..runtime..blocking..task..BlockingTask$LT$T$GT$$u20$as$u20$core..future..future..Future$GT$::poll::h8b9db1efbc4877eb + 136
25 camera*display 0x100992fa8 tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::*$u7b$$u7b$closure$u7d$$u7d$::h904262c7dc2f88b0 + 192
26 camera*display 0x100992d14 tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::h973d3c0095d258c4 + 72
27 camera_display 0x100991ed8 tokio::runtime::task::harness::poll_future::*$u7b$$u7b$closure$u7d$$u7d$::h8fa5ae0829102149 + 64
28 camera*display 0x1009b2ff4 *$LT$core..panic..unwind*safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2974776e7861df5d + 44
29 camera_display 0x1009a61b4 std::panicking::catch_unwind::do_call::h792e9a0912086d16 + 72
30 camera_display 0x1009a5e4c \_\_rust_try + 32
31 camera_display 0x1009a3110 std::panic::catch_unwind::h4474725978f361f4 + 96
32 camera_display 0x100991c70 tokio::runtime::task::harness::poll_future::hcf462599407f235c + 96
33 camera_display 0x1009909a8 tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll_inner::h5e6f168e02291d20 + 160
34 camera_display 0x100990878 tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll::h8a00a85251f71f0c + 28
35 camera_display 0x1009aa090 tokio::runtime::task::raw::poll::hf96de62ba33ad657 + 36
36 camera_display 0x1009a9db0 tokio::runtime::task::raw::RawTask::poll::hde83a8d818768542 + 52
37 camera_display 0x1009b1ec8 tokio::runtime::task::UnownedTask$LT$S$GT$::run::h7ce7c13c00052430 + 64
38 camera_display 0x1009bb178 tokio::runtime::blocking::pool::Task::run::hf56d840fd68c7fdc + 28
39 camera_display 0x1009bcdb0 tokio::runtime::blocking::pool::Inner::run::hd581e97506257439 + 512
40 camera_display 0x1009bcb1c tokio::runtime::blocking::pool::Spawner::spawn_thread::*$u7b$$u7b$closure$u7d$$u7d$::h2a632d4852cdd40e + 144
41 camera*display 0x1009ef10c std::sys::backtrace::\_\_rust_begin_short_backtrace::h51232d61e41dc60c + 16
42 camera_display 0x1009b4600 std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::hbf84c81d58c2715d + 116
43 camera*display 0x1009b302c *$LT$core..panic..unwind*safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2c0e201eb4dddbb7 + 44
44 camera_display 0x1009a6210 std::panicking::catch_unwind::do_call::h94ffcffc7cdce087 + 68
45 camera_display 0x1009b9364 \_\_rust_try + 32
46 camera_display 0x1009b4154 std::thread::Builder::spawn_unchecked*::\_$u7b$$u7b$closure$u7d$$u7d$::haea346fd331ce539 + 728
47 camera_display 0x1009946b4 core::ops::function::FnOnce::call_once$u7b$$u7b$vtable.shim$u7d$$u7d$::h12a95acb393be819 + 24
48 camera_display 0x100a9c560 std::sys::pal::unix::thread::Thread::new::thread_start::h636d41ad3c63ac40 + 56
49 libsystem_pthread.dylib 0x1814f7c0c \_pthread_start + 136
50 libsystem_pthread.dylib 0x1814f2b80 thread_start + 8

Thread 11:
0 libsystem_pthread.dylib 0x1814f2b6c start_wqthread + 0

Thread 12:: Dispatch queue: com.apple.dock.fullscreen
0 dyld 0x18116e2d0 dyld4::Loader::LoaderRef::loader(dyld4::RuntimeState const&) const + 12
1 dyld 0x18117cf84 dyld4::PrebuiltLoader::dependent(dyld4::RuntimeState const&, unsigned int, mach_o::LinkedDylibAttributes*) const + 128
2 dyld 0x181174eb0 dyld4::Loader::hasExportedSymbol(Diagnostics&, dyld4::RuntimeState&, char const*, dyld4::Loader::ExportedSymbolMode, dyld4::Loader::ResolverMode, dyld4::Loader::ResolvedSymbol*, dyld3::Array<dyld4::Loader const*>_) const + 1280
3 dyld 0x181174ee8 dyld4::Loader::hasExportedSymbol(Diagnostics&, dyld4::RuntimeState&, char const_, dyld4::Loader::ExportedSymbolMode, dyld4::Loader::ResolverMode, dyld4::Loader::ResolvedSymbol*, dyld3::Array<dyld4::Loader const*>_) const + 1336
4 dyld 0x181174ee8 dyld4::Loader::hasExportedSymbol(Diagnostics&, dyld4::RuntimeState&, char const_, dyld4::Loader::ExportedSymbolMode, dyld4::Loader::ResolverMode, dyld4::Loader::ResolvedSymbol*, dyld3::Array<dyld4::Loader const*>_) const + 1336
5 dyld 0x181174ee8 dyld4::Loader::hasExportedSymbol(Diagnostics&, dyld4::RuntimeState&, char const_, dyld4::Loader::ExportedSymbolMode, dyld4::Loader::ResolverMode, dyld4::Loader::ResolvedSymbol*, dyld3::Array<dyld4::Loader const*>_) const + 1336
6 dyld 0x181174ee8 dyld4::Loader::hasExportedSymbol(Diagnostics&, dyld4::RuntimeState&, char const_, dyld4::Loader::ExportedSymbolMode, dyld4::Loader::ResolverMode, dyld4::Loader::ResolvedSymbol*, dyld3::Array<dyld4::Loader const*>_) const + 1336
7 dyld 0x181174ee8 dyld4::Loader::hasExportedSymbol(Diagnostics&, dyld4::RuntimeState&, char const_, dyld4::Loader::ExportedSymbolMode, dyld4::Loader::ResolverMode, dyld4::Loader::ResolvedSymbol*, dyld3::Array<dyld4::Loader const*>_) const + 1336
8 dyld 0x18118ccac dyld4::APIs::dlsym(void_, char const\*) + 1464
9 SkyLight 0x1878d1a70 **loginframework_vtable_block_invoke + 564
10 libdispatch.dylib 0x18135985c \_dispatch_client_callout + 16
11 libdispatch.dylib 0x181342a28 \_dispatch_once_callout + 32
12 SkyLight 0x1878d33b8 SLSCopyCurrentSessionPropertiesInternalBridge + 384
13 SkyLight 0x1876a494c SLSessionCopyCurrentDictionary + 20
14 AppKit 0x1855f94b4 -[NSDockConnection _makeConnectionIfNeededWithRetryInterval:onDemand:] + 64
15 AppKit 0x1860de190 **35-[NSDockConnection startConnection]\_block_invoke.13 + 44
16 libdispatch.dylib 0x18133fb2c \_dispatch_call_block_and_release + 32
17 libdispatch.dylib 0x18135985c \_dispatch_client_callout + 16
18 libdispatch.dylib 0x181348350 \_dispatch_lane_serial_drain + 740
19 libdispatch.dylib 0x181348e2c \_dispatch_lane_invoke + 388
20 libdispatch.dylib 0x181353264 \_dispatch_root_queue_drain_deferred_wlh + 292
21 libdispatch.dylib 0x181352ae8 \_dispatch_workloop_worker_thread + 540
22 libsystem_pthread.dylib 0x1814f3e64 \_pthread_wqthread + 292
23 libsystem_pthread.dylib 0x1814f2b74 start_wqthread + 8

Thread 13:
0 libsystem_pthread.dylib 0x1814f2b6c start_wqthread + 0

Thread 14:
0 libsystem*kernel.dylib 0x1814b5bb0 semaphore_wait_trap + 8
1 libdispatch.dylib 0x181341960 \_dispatch_sema4_wait + 28
2 libdispatch.dylib 0x181341f10 \_dispatch_semaphore_wait_slow + 132
3 camera_display 0x100a99f18 std::thread::park::h7819486c310a61ee + 96
4 camera_display 0x100969cac crossbeam_channel::context::Context::wait_until::hfe8d919652164e11 + 208
5 camera_display 0x10097912c crossbeam_channel::flavors::array::Channel$LT$T$GT$::recv::*$u7b$$u7b$closure$u7d$$u7d$::h100e5fadd7ac726e + 152
6 camera*display 0x10096b0d8 crossbeam_channel::context::Context::with::*$u7b$$u7b$closure$u7d$$u7d$::h800801b973582feb + 100
7 camera*display 0x10096ad28 crossbeam_channel::context::Context::with::*$u7b$$u7b$closure$u7d$$u7d$::h32d36c01f3997fab + 232
8 camera*display 0x10097986c std::thread::local::LocalKey$LT$T$GT$::try_with::h6aa4f6c96a05adf1 + 172
9 camera_display 0x10096aa64 crossbeam_channel::context::Context::with::h30d2c8f0eb1cc28b + 52
10 camera_display 0x10097906c crossbeam_channel::flavors::array::Channel$LT$T$GT$::recv::hbb39635dcd84a0c0 + 280
11 camera_display 0x1009870d8 crossbeam_channel::channel::Receiver$LT$T$GT$::recv::h07b397b671ed012a + 160
12 camera_display 0x100986e80 *$LT$crossbeam*channel..channel..IntoIter$LT$T$GT$$u20$as$u20$core..iter..traits..iterator..Iterator$GT$::next::h50472e68cf682da0 + 36
13 camera_display 0x1009684e4 streamlib_core::runtime::StreamRuntime::spawn_handler_threads::*$u7b$$u7b$closure$u7d$$u7d$::hd0c273a3f900621f + 2448
14 camera*display 0x100982e64 std::sys::backtrace::\_\_rust_begin_short_backtrace::hbe487fc5f40b97b3 + 16
15 camera_display 0x1009640d4 std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::h36a738c24e545fe9 + 120
16 camera*display 0x100973cdc *$LT$core..panic..unwind*safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::hf3b8dc80fdc96ede + 44
17 camera_display 0x10097c8d0 std::panicking::catch_unwind::do_call::h6c915fdd9e40d569 + 68
18 camera_display 0x100964324 \_\_rust_try + 32
19 camera_display 0x100963f1c std::thread::Builder::spawn_unchecked*::\_$u7b$$u7b$closure$u7d$$u7d$::heeb9bfa0dbf47564 + 748
20 camera_display 0x10096e840 core::ops::function::FnOnce::call_once$u7b$$u7b$vtable.shim$u7d$$u7d$::h308f6798985fc9cf + 24
21 camera_display 0x100a9c560 std::sys::pal::unix::thread::Thread::new::thread_start::h636d41ad3c63ac40 + 56
22 libsystem_pthread.dylib 0x1814f7c0c \_pthread_start + 136
23 libsystem_pthread.dylib 0x1814f2b80 thread_start + 8

Thread 15 Crashed:
0 AppKit 0x186223cc0 -[NSWMWindowCoordinator performTransactionUsingBlock:] + 752
1 AppKit 0x186052a00 -[NSWindow(NSWMWindowManagement) window:didUpdateWithChangedProperties:] + 104
2 WindowManagement 0x262046720 -[_WMWindow performUpdatesUsingBlock:] + 100
3 WindowManagement 0x262045714 -[_WMWindow applyTags:mask:] + 712
4 WindowManagement 0x262045cf8 -[_WMWindow setVisible:] + 84
5 AppKit 0x1855fb064 -[NSWindow _setVisible:] + 268
6 AppKit 0x1855faf24 -[NSWindow _makeKeyRegardlessOfVisibility] + 40
7 AppKit 0x1855f37e8 -[NSWindow makeKeyAndOrderFront:] + 24
8 camera*display 0x1009508ac *$LT$$LP$A$C$$RP$$u20$as$u20$objc2..encode..EncodeArguments$GT$::**invoke::hb60909703740adfa + 88
9 camera*display 0x10095167c objc2::runtime::message_receiver::msg_send_primitive::send::h20a306aff6203842 + 80
10 camera_display 0x100950770 objc2::runtime::message_receiver::MessageReceiver::send_message::he8b25aabb8931882 + 192
11 camera_display 0x10094b5ec *$LT$MethodFamily$u20$as$u20$objc2..**macro*helpers..msg_send_retained..MsgSend$LT$Receiver$C$Return$GT$$GT$::send_message::he02307f0ece5f831 + 180
12 camera_display 0x10094d20c objc2_app_kit::generated::\_\_NSWindow::NSWindow::makeKeyAndOrderFront::hacb345b4ac9a7f1e + 80
13 camera_display 0x100943cec *$LT$streamlib*apple..processors..display..AppleDisplayProcessor$u20$as$u20$streamlib_core..stream_processor..StreamProcessor$GT$::on_start::hbfc20c3368bc77fe + 1080
14 camera_display 0x100967e9c streamlib_core::runtime::StreamRuntime::spawn_handler_threads::*$u7b$$u7b$closure$u7d$$u7d$::hd0c273a3f900621f + 840
15 camera*display 0x100982e64 std::sys::backtrace::\_\_rust_begin_short_backtrace::hbe487fc5f40b97b3 + 16
16 camera_display 0x1009640d4 std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::h36a738c24e545fe9 + 120
17 camera*display 0x100973cdc *$LT$core..panic..unwind*safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::hf3b8dc80fdc96ede + 44
18 camera_display 0x10097c8d0 std::panicking::catch_unwind::do_call::h6c915fdd9e40d569 + 68
19 camera_display 0x100964324 \_\_rust_try + 32
20 camera_display 0x100963f1c std::thread::Builder::spawn_unchecked*::\_$u7b$$u7b$closure$u7d$$u7d$::heeb9bfa0dbf47564 + 748
21 camera_display 0x10096e840 core::ops::function::FnOnce::call_once$u7b$$u7b$vtable.shim$u7d$$u7d$::h308f6798985fc9cf + 24
22 camera_display 0x100a9c560 std::sys::pal::unix::thread::Thread::new::thread_start::h636d41ad3c63ac40 + 56
23 libsystem_pthread.dylib 0x1814f7c0c \_pthread_start + 136
24 libsystem_pthread.dylib 0x1814f2b80 thread_start + 8

Thread 15 crashed with ARM Thread State (64-bit):
x0: 0x000000018655e493 x1: 0x0000000000000000 x2: 0x0000000181188e40 x3: 0x00000001f04ffc38
x4: 0x000000018199ddcf x5: 0x000000018655e4f8 x6: 0x0000000000000000 x7: 0x0000000000000000
x8: 0x00000001ece81000 x9: 0x0000000000000002 x10: 0x007ffffffffffff8 x11: 0x0000000000000000
x12: 0x0000000000000005 x13: 0x0000000138022a00 x14: 0x00000001ef4a4710 x15: 0x00000001ef4a4710
x16: 0x00000001813ff5e8 x17: 0x00000001f04dfb18 x18: 0x0000000000000000 x19: 0x0000000170efcf90
x20: 0x0000000137f23850 x21: 0x0000000137e46480 x22: 0x00000002866be958 x23: 0x0000000000000000
x24: 0x00000002866be7c0 x25: 0x0000000000000001 x26: 0x000000027ae9d000 x27: 0x0000000000000001
x28: 0x0000000000000001 fp: 0x0000000170efcf80 lr: 0x0000000186223cc0
sp: 0x0000000170efcec0 pc: 0x0000000186223cc0 cpsr: 0x40001000
far: 0x0000000000000000 esr: 0xf2000001 (Breakpoint) brk 1

Binary Images:
0x100930000 - 0x100b8bfff camera_display (_) <3256a5ab-b430-3345-82d3-9f5c54959345> _/camera_display
0x1079c0000 - 0x108057fff com.apple.AGXMetalG13X (329.2) <6b497f3b-6583-398c-9d05-5f30a1c1bae5> /System/Library/Extensions/AGXMetalG13X.bundle/Contents/MacOS/AGXMetalG13X
0x107790000 - 0x10779bfff libobjc-trampolines.dylib (_) <a3faee04-0f8b-3428-9497-560c97eca6fb> /usr/lib/libobjc-trampolines.dylib
0x1814b5000 - 0x1814f0653 libsystem_kernel.dylib (_) <6e4a96ad-04b8-3e8a-b91d-087e62306246> /usr/lib/system/libsystem_kernel.dylib
0x18133e000 - 0x18138475f libdispatch.dylib (_) <24ce0d89-4114-30c2-a81a-3db1f5931cff> /usr/lib/system/libdispatch.dylib
0x1811f0000 - 0x18123a8ff libxpc.dylib (_) <1b8951eb-68db-37ba-9581-240b796aa872> /usr/lib/system/libxpc.dylib
0x181aa5000 - 0x181d9713f com.apple.LaunchServices (1141.1) <1e58a116-8211-3368-8f5a-5df00d6f8971> /System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/LaunchServices.framework/Versions/A/LaunchServices
0x188fd0000 - 0x189043787 com.apple.AE (944) <c410f3f4-79c0-3066-92bd-3b8ab89ade37> /System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/AE.framework/Versions/A/AE
0x18cfc0000 - 0x18d2c6fdf com.apple.HIToolbox (2.1.1) <1a037942-11e0-3fc8-aad2-20b11e7ae1a4> /System/Library/Frameworks/Carbon.framework/Versions/A/Frameworks/HIToolbox.framework/Versions/A/HIToolbox
0x1854cb000 - 0x18695be3f com.apple.AppKit (6.9) <860c164c-d04c-30ff-8c6f-e672b74caf11> /System/Library/Frameworks/AppKit.framework/Versions/C/AppKit
0x181150000 - 0x1811eb577 dyld (_) <3247e185-ced2-36ff-9e29-47a77c23e004> /usr/lib/dyld
0x0 - 0xffffffffffffffff ??? (_) <00000000-0000-0000-0000-000000000000> ???
0x1814f1000 - 0x1814fda47 libsystem_pthread.dylib (_) <d6494ba9-171e-39fc-b1aa-28ecf87975d1> /usr/lib/system/libsystem_pthread.dylib
0x187533000 - 0x187a5905f com.apple.SkyLight (1.600.0) <4e052846-80c2-38af-85bf-1482e070a32b> /System/Library/PrivateFrameworks/SkyLight.framework/Versions/A/SkyLight
0x26203e000 - 0x26205cf1f com.apple.WindowManagement (_) <70ba6e1a-afb1-39d8-b8a8-8d6246ac6064> /System/Library/PrivateFrameworks/WindowManagement.framework/Versions/A/WindowManagement
0x181566000 - 0x181aa4fff com.apple.CoreFoundation (6.9) <8d45baee-6cc0-3b89-93fd-ea1c8e15c6d7> /System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation
0x181388000 - 0x181409243 libsystem_c.dylib (\*) <dfea8794-80ce-37c3-8f6a-108aa1d0b1b0> /usr/lib/system/libsystem_c.dylib

External Modification Summary:
Calls made by other processes targeting this process:
task_for_pid: 0
thread_create: 0
thread_set_state: 0
Calls made by this process:
task_for_pid: 0
thread_create: 0
thread_set_state: 0
Calls made by all processes on this machine:
task_for_pid: 0
thread_create: 0
thread_set_state: 0

---

## Full Report

{"app*name":"camera_display","timestamp":"2025-10-22 13:33:50.00 -0400","app_version":"","slice_uuid":"3256a5ab-b430-3345-82d3-9f5c54959345","build_version":"","platform":1,"share_with_app_devs":0,"is_first_party":1,"bug_type":"309","os_version":"macOS 15.6.1 (24G90)","roots_installed":0,"incident_id":"0497C718-C829-4473-BEA8-D3DA8F382992","name":"camera_display"}
{
"uptime" : 1700000,
"procRole" : "Background",
"version" : 2,
"userID" : 501,
"deployVersion" : 210,
"modelCode" : "MacBookPro18,2",
"coalitionID" : 126960,
"osVersion" : {
"train" : "macOS 15.6.1",
"build" : "24G90",
"releaseType" : "User"
},
"captureTime" : "2025-10-22 13:33:50.1478 -0400",
"codeSigningMonitor" : 1,
"incident" : "0497C718-C829-4473-BEA8-D3DA8F382992",
"pid" : 84399,
"translated" : false,
"cpuType" : "ARM-64",
"roots_installed" : 0,
"bug_type" : "309",
"procLaunch" : "2025-10-22 13:33:48.0161 -0400",
"procStartAbsTime" : 42862550728412,
"procExitAbsTime" : 42862601804288,
"procName" : "camera_display",
"procPath" : "\/Users\/USER\/*\/camera_display",
"parentProc" : "Exited process",
"parentPid" : 84391,
"coalitionName" : "com.microsoft.VSCode",
"crashReporterKey" : "1EB5C6D5-E9DE-C50D-46A8-1749AC93AF6C",
"appleIntelligenceStatus" : {"state":"available"},
"responsiblePid" : 91850,
"responsibleProc" : "Electron",
"codeSigningID" : "camera_display-6f54f0b5527224ea",
"codeSigningTeamID" : "",
"codeSigningFlags" : 570556929,
"codeSigningValidationCategory" : 10,
"codeSigningTrustLevel" : 4294967295,
"codeSigningAuxiliaryInfo" : 0,
"instructionByteStream" : {"beforePC":"EAAA8BCyNZHwI8Ha4wMQqgEAgFJJscqXoO7\/NcAZAPAATBKRNJ4GlA==","atPC":"IAAg1MAZAPAA6BKRMJ4GlCAAINQWAIDSwFU18ADgFpHilTSwQgQwkQ=="},
"bootSessionUUID" : "4B204DE0-F1E9-49D0-8F46-74D3601B7360",
"wakeTime" : 847977,
"sleepWakeUUID" : "23414675-5AB9-4663-BCA5-52ACF8E352D4",
"sip" : "enabled",
"exception" : {"codes":"0x0000000000000001, 0x0000000186223cc0","rawCodes":[1,6545358016],"type":"EXC_BREAKPOINT","signal":"SIGTRAP"},
"termination" : {"flags":0,"code":5,"namespace":"SIGNAL","indicator":"Trace\/BPT trap: 5","byProc":"exc handler","byPid":84399},
"os_fault" : {"process":"camera_display"},
"asi" : {"libsystem_c.dylib":["Must only be used from the main thread"]},
"extMods" : {"caller":{"thread_create":0,"thread_set_state":0,"task_for_pid":0},"system":{"thread_create":0,"thread_set_state":0,"task_for_pid":0},"targeted":{"thread_create":0,"thread_set_state":0,"task_for_pid":0},"warnings":0},
"faultingThread" : 15,
"threads" : [{"threadState":{"x":[{"value":0},{"value":17297326606},{"value":0},{"value":20739},{"value":0},{"value":17605070946304},{"value":16384},{"value":0},{"value":0},{"value":17179869184},{"value":16384},{"value":0},{"value":0},{"value":0},{"value":4099},{"value":1},{"value":18446744073709551569},{"value":8326618216},{"value":0},{"value":0},{"value":16384},{"value":17605070946304},{"value":0},{"value":20739},{"value":6162240496},{"value":0},{"value":17297326606},{"value":18446744073709550527},{"value":117457422}],"flavor":"ARM_THREAD_STATE64","lr":{"value":6464242592},"cpsr":{"value":4096},"fp":{"value":6162240160},"sp":{"value":6162240080},"esr":{"value":1442840704,"description":" Address size fault"},"pc":{"value":6464166964},"far":{"value":0}},"id":65463663,"name":"main","queue":"com.apple.main-thread","frames":[{"imageOffset":3124,"symbol":"mach_msg2_trap","symbolLocation":8,"imageIndex":3},{"imageOffset":78752,"symbol":"mach_msg2_internal","symbolLocation":76,"imageIndex":3},{"imageOffset":38756,"symbol":"mach_msg_overwrite","symbolLocation":484,"imageIndex":3},{"imageOffset":4008,"symbol":"mach_msg","symbolLocation":24,"imageIndex":3},{"imageOffset":118496,"symbol":"\_dispatch_mach_send_and_wait_for_reply","symbolLocation":548,"imageIndex":4},{"imageOffset":119424,"symbol":"dispatch_mach_send_with_result_and_wait_for_reply","symbolLocation":60,"imageIndex":4},{"imageOffset":50280,"symbol":"xpc_connection_send_message_with_reply_sync","symbolLocation":284,"imageIndex":5},{"imageOffset":28888,"symbol":"LSClientToServerConnection::sendWithReply(void*)","symbolLocation":68,"imageIndex":6},{"imageOffset":38836,"symbol":"\_LSCopyApplicationInformation","symbolLocation":988,"imageIndex":6},{"imageOffset":28740,"imageIndex":7},{"imageOffset":112732,"symbol":"\_dispatch_client_callout","symbolLocation":16,"imageIndex":4},{"imageOffset":18984,"symbol":"\_dispatch_once_callout","symbolLocation":32,"imageIndex":4},{"imageOffset":5784,"symbol":"\_AERegisterCurrentApplicationInfomationWithAppleEventsD","symbolLocation":336,"imageIndex":7},{"imageOffset":28108,"imageIndex":7},{"imageOffset":112732,"symbol":"\_dispatch_client_callout","symbolLocation":16,"imageIndex":4},{"imageOffset":18984,"symbol":"\_dispatch_once_callout","symbolLocation":32,"imageIndex":4},{"imageOffset":5444,"symbol":"AEGetRegisteredMachPort","symbolLocation":72,"imageIndex":7},{"imageOffset":25792,"imageIndex":7},{"imageOffset":24156,"imageIndex":7},{"imageOffset":112732,"symbol":"\_dispatch_client_callout","symbolLocation":16,"imageIndex":4},{"imageOffset":18984,"symbol":"\_dispatch_once_callout","symbolLocation":32,"imageIndex":4},{"imageOffset":5036,"symbol":"aeInstallRunLoopDispatcher","symbolLocation":192,"imageIndex":7},{"imageOffset":764404,"symbol":"\_FirstEventTime","symbolLocation":144,"imageIndex":8},{"imageOffset":799096,"symbol":"RunCurrentEventLoopInMode","symbolLocation":64,"imageIndex":8},{"imageOffset":811804,"symbol":"ReceiveNextEventCommon","symbolLocation":216,"imageIndex":8},{"imageOffset":2430084,"symbol":"\_BlockUntilNextEventMatchingListInModeWithFilter","symbolLocation":76,"imageIndex":8},{"imageOffset":240180,"symbol":"\_DPSNextEvent","symbolLocation":684,"imageIndex":9},{"imageOffset":10328384,"symbol":"-[NSApplication(NSEventRouting) _nextEventMatchingEventMask:untilDate:inMode:dequeue:]","symbolLocation":688,"imageIndex":9},{"imageOffset":99120,"symbol":"*$LT$$LP$A$C$B$C$C$C$D$RP$$u20$as$u20$objc2..encode..EncodeArguments$GT$::**invoke::hf996fc9ec8218dd0","symbolLocation":152,"imageIndex":0},{"imageOffset":70808,"symbol":"objc2::runtime::message*receiver::msg_send_primitive::send::h3e375a8fe16fe952","symbolLocation":72,"imageIndex":0},{"imageOffset":102932,"symbol":"objc2::runtime::message_receiver::MessageReceiver::send_message::hc9c7788f07986769","symbolLocation":208,"imageIndex":0},{"imageOffset":70136,"symbol":"*$LT$MethodFamily$u20$as$u20$objc2..**macro*helpers..msg_send_retained..MsgSend$LT$Receiver$C$Return$GT$$GT$::send_message::h305fa38f066d40da","symbolLocation":224,"imageIndex":0},{"imageOffset":98728,"symbol":"streamlib_apple::runtime_ext::configure_macos_event_loop::*$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::h3dab1deb9403f3ac","symbolLocation":644,"imageIndex":0},{"imageOffset":38132,"sourceLine":133,"sourceFile":"future.rs","symbol":"_$LT$core..pin..Pin$LT$P$GT$$u20$as$u20$core..future..future..Future$GT$::poll::he5b5385ffc88baf7","imageIndex":0,"symbolLocation":80},{"imageOffset":20228,"sourceLine":347,"sourceFile":"runtime.rs","symbol":"streamlib*core::runtime::StreamRuntime::run::*$u7b$$u7b$closure$u7d$$u7d$::h131999c1887513fd","imageIndex":0,"symbolLocation":2860},{"imageOffset":11556,"sourceLine":92,"sourceFile":"runtime.rs","symbol":"streamlib::runtime::StreamRuntime::run::_$u7b$$u7b$closure$u7d$$u7d$::h035280339e749049","imageIndex":0,"symbolLocation":340},{"imageOffset":5536,"sourceLine":50,"sourceFile":"camera_display.rs","symbol":"camera_display::main::_$u7b$$u7b$closure$u7d$$u7d$::h931c4c080d7e5443","imageIndex":0,"symbolLocation":1860},{"imageOffset":15960,"sourceLine":285,"sourceFile":"park.rs","symbol":"tokio::runtime::park::CachedParkThread::block*on::*$u7b$$u7b$closure$u7d$$u7d$::he652241d9d748cc5","imageIndex":0,"symbolLocation":64},{"symbol":"tokio::task::coop::with*budget::hdee52a54e383e63e","inline":true,"imageIndex":0,"imageOffset":13832,"symbolLocation":88,"sourceLine":167,"sourceFile":"mod.rs"},{"symbol":"tokio::task::coop::budget::h522291a1aa638a35","inline":true,"imageIndex":0,"imageOffset":13832,"symbolLocation":208,"sourceLine":133,"sourceFile":"mod.rs"},{"imageOffset":13832,"sourceLine":285,"sourceFile":"park.rs","symbol":"tokio::runtime::park::CachedParkThread::block_on::h74280c42af9ed8f4","imageIndex":0,"symbolLocation":576},{"imageOffset":48476,"sourceLine":66,"sourceFile":"blocking.rs","symbol":"tokio::runtime::context::blocking::BlockingRegionGuard::block_on::h75c8666213e7e72e","imageIndex":0,"symbolLocation":140},{"imageOffset":49012,"sourceLine":87,"sourceFile":"mod.rs","symbol":"tokio::runtime::scheduler::multi_thread::MultiThread::block_on::*$u7b$$u7b$closure$u7d$$u7d$::h3e494ff6abba3c0c","imageIndex":0,"symbolLocation":80},{"imageOffset":35616,"sourceLine":65,"sourceFile":"runtime.rs","symbol":"tokio::runtime::context::runtime::enter*runtime::h52647cd33246ba11","imageIndex":0,"symbolLocation":232},{"imageOffset":48844,"sourceLine":86,"sourceFile":"mod.rs","symbol":"tokio::runtime::scheduler::multi_thread::MultiThread::block_on::h2ecce2490b5df586","imageIndex":0,"symbolLocation":92},{"imageOffset":45324,"sourceLine":370,"sourceFile":"runtime.rs","symbol":"tokio::runtime::runtime::Runtime::block_on_inner::h136e71ce595567e7","imageIndex":0,"symbolLocation":184},{"imageOffset":45848,"sourceLine":342,"sourceFile":"runtime.rs","symbol":"tokio::runtime::runtime::Runtime::block_on::h5e5ba000b05908f2","imageIndex":0,"symbolLocation":352},{"imageOffset":39648,"sourceLine":54,"sourceFile":"camera_display.rs","symbol":"camera_display::main::h98bf6d72bbfd3aa2","imageIndex":0,"symbolLocation":232},{"imageOffset":7044,"sourceLine":253,"sourceFile":"function.rs","symbol":"core::ops::function::FnOnce::call_once::h7b4a40b813d1cc1d","imageIndex":0,"symbolLocation":20},{"imageOffset":49664,"sourceLine":158,"sourceFile":"backtrace.rs","symbol":"std::sys::backtrace::\_\_rust_begin_short_backtrace::h675d34eca97f0003","imageIndex":0,"symbolLocation":24},{"imageOffset":47888,"sourceLine":206,"sourceFile":"rt.rs","symbol":"std::rt::lang_start::*$u7b$$u7b$closure$u7d$$u7d$::h926ec4a5bf2e3bad","imageIndex":0,"symbolLocation":36},{"imageOffset":1503112,"symbol":"std::rt::lang*start_internal::h5b2b6e2cac0b4d2b","symbolLocation":140,"imageIndex":0},{"imageOffset":47840,"sourceLine":205,"sourceFile":"rt.rs","symbol":"std::rt::lang_start::hd6ff86ecba9d7b6a","imageIndex":0,"symbolLocation":84},{"imageOffset":39784,"symbol":"main","symbolLocation":36,"imageIndex":0},{"imageOffset":27544,"symbol":"start","symbolLocation":6076,"imageIndex":10}]},{"id":65463672,"name":"tokio-runtime-worker","threadState":{"x":[{"value":260},{"value":0},{"value":256},{"value":0},{"value":0},{"value":160},{"value":0},{"value":0},{"value":6164401320},{"value":0},{"value":0},{"value":2},{"value":2},{"value":0},{"value":0},{"value":0},{"value":305},{"value":8326616336},{"value":0},{"value":5233459864},{"value":5233459928},{"value":6164410592},{"value":0},{"value":0},{"value":256},{"value":257},{"value":512},{"value":0},{"value":0}],"flavor":"ARM_THREAD_STATE64","lr":{"value":6464438496},"cpsr":{"value":1610616832},"fp":{"value":6164401440},"sp":{"value":6164401296},"esr":{"value":1442840704,"description":" Address size fault"},"pc":{"value":6464181196},"far":{"value":0}},"frames":[{"imageOffset":17356,"symbol":"\_\_psynch_cvwait","symbolLocation":8,"imageIndex":3},{"imageOffset":28896,"symbol":"\_pthread_cond_wait","symbolLocation":984,"imageIndex":12},{"imageOffset":1289468,"symbol":"*$LT$parking*lot_core..thread_parker..imp..ThreadParker$u20$as$u20$parking_lot_core..thread_parker..ThreadParkerT$GT$::park::h0e2d0998749974de","symbolLocation":256,"imageIndex":0},{"imageOffset":1257704,"symbol":"parking_lot_core::parking_lot::park::*$u7b$$u7b$closure$u7d$$u7d$::h0b4fc83101e8557f","symbolLocation":748,"imageIndex":0},{"imageOffset":1256464,"symbol":"parking*lot_core::parking_lot::park::hd04bbb424560662a","symbolLocation":296,"imageIndex":0},{"imageOffset":1286760,"symbol":"parking_lot::condvar::Condvar::wait_until_internal::h8640b2ba350582ed","symbolLocation":124,"imageIndex":0},{"imageOffset":537312,"symbol":"parking_lot::condvar::Condvar::wait::h6618c4600aa6d512","symbolLocation":68,"imageIndex":0},{"imageOffset":798236,"symbol":"tokio::loom::std::parking_lot::Condvar::wait::hc858434e42aaccb1","symbolLocation":36,"imageIndex":0},{"imageOffset":773892,"symbol":"tokio::runtime::scheduler::multi_thread::park::Inner::park_condvar::h2267f8a57982d137","symbolLocation":264,"imageIndex":0},{"imageOffset":773552,"symbol":"tokio::runtime::scheduler::multi_thread::park::Inner::park::h3c3dbc8c396a0894","symbolLocation":168,"imageIndex":0},{"imageOffset":772836,"symbol":"tokio::runtime::scheduler::multi_thread::park::Parker::park::h010fa0d10ce96e9b","symbolLocation":40,"imageIndex":0},{"imageOffset":667856,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::park_timeout::h37500a9fc2ffb756","symbolLocation":776,"imageIndex":0},{"imageOffset":666572,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::park::hd6c55373d20068c0","symbolLocation":968,"imageIndex":0},{"imageOffset":661128,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::run::h541c11fea84a38ec","symbolLocation":1784,"imageIndex":0},{"imageOffset":659156,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::h60671eb717a34c70","symbolLocation":104,"imageIndex":0},{"imageOffset":725700,"symbol":"tokio::runtime::context::scoped::Scoped$LT$T$GT$::set::h263d3206a13c783c","symbolLocation":148,"imageIndex":0},{"imageOffset":629280,"symbol":"tokio::runtime::context::set_scheduler::_$u7b$$u7b$closure$u7d$$u7d$::h74a8e23df56c9882","symbolLocation":40,"imageIndex":0},{"imageOffset":754924,"symbol":"std::thread::local::LocalKey$LT$T$GT$::try*with::h2f963d543150c87c","symbolLocation":196,"imageIndex":0},{"imageOffset":753488,"symbol":"std::thread::local::LocalKey$LT$T$GT$::with::hbd17f06bbba52edc","symbolLocation":24,"imageIndex":0},{"imageOffset":629228,"symbol":"tokio::runtime::context::set_scheduler::heb413530e9784dfc","symbolLocation":68,"imageIndex":0},{"imageOffset":658936,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::h79a1aa630e49b68a","symbolLocation":248,"imageIndex":0},{"imageOffset":560916,"symbol":"tokio::runtime::context::runtime::enter*runtime::hb50775e529a9a617","symbolLocation":188,"imageIndex":0},{"imageOffset":658520,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::h79855d48ee1e3c6a","symbolLocation":600,"imageIndex":0},{"imageOffset":657908,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Launch::launch::*$u7b$$u7b$closure$u7d$$u7d$::he167c16b6e6899e4","symbolLocation":24,"imageIndex":0},{"imageOffset":804156,"symbol":"_$LT$tokio..runtime..blocking..task..BlockingTask$LT$T$GT$$u20$as$u20$core..future..future..Future$GT$::poll::h8b9db1efbc4877eb","symbolLocation":136,"imageIndex":0},{"imageOffset":405416,"symbol":"tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::_$u7b$$u7b$closure$u7d$$u7d$::h904262c7dc2f88b0","symbolLocation":192,"imageIndex":0},{"imageOffset":404756,"symbol":"tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::h973d3c0095d258c4","symbolLocation":72,"imageIndex":0},{"imageOffset":401112,"symbol":"tokio::runtime::task::harness::poll*future::*$u7b$$u7b$closure$u7d$$u7d$::h8fa5ae0829102149","symbolLocation":64,"imageIndex":0},{"imageOffset":536564,"symbol":"_$LT$core..panic..unwind_safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2974776e7861df5d","symbolLocation":44,"imageIndex":0},{"imageOffset":483764,"symbol":"std::panicking::catch_unwind::do_call::h792e9a0912086d16","symbolLocation":72,"imageIndex":0},{"imageOffset":482892,"symbol":"\_\_rust_try","symbolLocation":32,"imageIndex":0},{"imageOffset":471312,"symbol":"std::panic::catch_unwind::h4474725978f361f4","symbolLocation":96,"imageIndex":0},{"imageOffset":400496,"symbol":"tokio::runtime::task::harness::poll_future::hcf462599407f235c","symbolLocation":96,"imageIndex":0},{"imageOffset":395688,"symbol":"tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll_inner::h5e6f168e02291d20","symbolLocation":160,"imageIndex":0},{"imageOffset":395384,"symbol":"tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll::h8a00a85251f71f0c","symbolLocation":28,"imageIndex":0},{"imageOffset":499856,"symbol":"tokio::runtime::task::raw::poll::hf96de62ba33ad657","symbolLocation":36,"imageIndex":0},{"imageOffset":499120,"symbol":"tokio::runtime::task::raw::RawTask::poll::hde83a8d818768542","symbolLocation":52,"imageIndex":0},{"imageOffset":532168,"symbol":"tokio::runtime::task::UnownedTask$LT$S$GT$::run::h7ce7c13c00052430","symbolLocation":64,"imageIndex":0},{"imageOffset":569720,"symbol":"tokio::runtime::blocking::pool::Task::run::hf56d840fd68c7fdc","symbolLocation":28,"imageIndex":0},{"imageOffset":576944,"symbol":"tokio::runtime::blocking::pool::Inner::run::hd581e97506257439","symbolLocation":512,"imageIndex":0},{"imageOffset":576284,"symbol":"tokio::runtime::blocking::pool::Spawner::spawn_thread::_$u7b$$u7b$closure$u7d$$u7d$::h2a632d4852cdd40e","symbolLocation":144,"imageIndex":0},{"imageOffset":782604,"symbol":"std::sys::backtrace::**rust*begin_short_backtrace::h51232d61e41dc60c","symbolLocation":16,"imageIndex":0},{"imageOffset":542208,"symbol":"std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::hbf84c81d58c2715d","symbolLocation":116,"imageIndex":0},{"imageOffset":536620,"symbol":"\_$LT$core..panic..unwind_safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2c0e201eb4dddbb7","symbolLocation":44,"imageIndex":0},{"imageOffset":483856,"symbol":"std::panicking::catch_unwind::do_call::h94ffcffc7cdce087","symbolLocation":68,"imageIndex":0},{"imageOffset":562020,"symbol":"**rust*try","symbolLocation":32,"imageIndex":0},{"imageOffset":541012,"symbol":"std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::haea346fd331ce539","symbolLocation":728,"imageIndex":0},{"imageOffset":411316,"symbol":"core::ops::function::FnOnce::call_once$u7b$$u7b$vtable.shim$u7d$$u7d$::h12a95acb393be819","symbolLocation":24,"imageIndex":0},{"imageOffset":1492320,"symbol":"std::sys::pal::unix::thread::Thread::new::thread_start::h636d41ad3c63ac40","symbolLocation":56,"imageIndex":0},{"imageOffset":27660,"symbol":"\_pthread_start","symbolLocation":136,"imageIndex":12},{"imageOffset":7040,"symbol":"thread_start","symbolLocation":8,"imageIndex":12}]},{"id":65463673,"name":"tokio-runtime-worker","threadState":{"x":[{"value":260},{"value":0},{"value":0},{"value":0},{"value":0},{"value":160},{"value":0},{"value":0},{"value":6166547624},{"value":0},{"value":0},{"value":2},{"value":2},{"value":0},{"value":0},{"value":0},{"value":305},{"value":8326616336},{"value":0},{"value":5242897048},{"value":5242897112},{"value":6166556896},{"value":0},{"value":0},{"value":0},{"value":1},{"value":256},{"value":0},{"value":0}],"flavor":"ARM_THREAD_STATE64","lr":{"value":6464438496},"cpsr":{"value":1610616832},"fp":{"value":6166547744},"sp":{"value":6166547600},"esr":{"value":1442840704,"description":" Address size fault"},"pc":{"value":6464181196},"far":{"value":0}},"frames":[{"imageOffset":17356,"symbol":"\_\_psynch_cvwait","symbolLocation":8,"imageIndex":3},{"imageOffset":28896,"symbol":"\_pthread_cond_wait","symbolLocation":984,"imageIndex":12},{"imageOffset":1289468,"symbol":"_$LT$parking*lot_core..thread_parker..imp..ThreadParker$u20$as$u20$parking_lot_core..thread_parker..ThreadParkerT$GT$::park::h0e2d0998749974de","symbolLocation":256,"imageIndex":0},{"imageOffset":1257704,"symbol":"parking_lot_core::parking_lot::park::*$u7b$$u7b$closure$u7d$$u7d$::h0b4fc83101e8557f","symbolLocation":748,"imageIndex":0},{"imageOffset":1256464,"symbol":"parking*lot_core::parking_lot::park::hd04bbb424560662a","symbolLocation":296,"imageIndex":0},{"imageOffset":1286760,"symbol":"parking_lot::condvar::Condvar::wait_until_internal::h8640b2ba350582ed","symbolLocation":124,"imageIndex":0},{"imageOffset":537312,"symbol":"parking_lot::condvar::Condvar::wait::h6618c4600aa6d512","symbolLocation":68,"imageIndex":0},{"imageOffset":798236,"symbol":"tokio::loom::std::parking_lot::Condvar::wait::hc858434e42aaccb1","symbolLocation":36,"imageIndex":0},{"imageOffset":773892,"symbol":"tokio::runtime::scheduler::multi_thread::park::Inner::park_condvar::h2267f8a57982d137","symbolLocation":264,"imageIndex":0},{"imageOffset":773552,"symbol":"tokio::runtime::scheduler::multi_thread::park::Inner::park::h3c3dbc8c396a0894","symbolLocation":168,"imageIndex":0},{"imageOffset":772836,"symbol":"tokio::runtime::scheduler::multi_thread::park::Parker::park::h010fa0d10ce96e9b","symbolLocation":40,"imageIndex":0},{"imageOffset":667856,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::park_timeout::h37500a9fc2ffb756","symbolLocation":776,"imageIndex":0},{"imageOffset":666572,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::park::hd6c55373d20068c0","symbolLocation":968,"imageIndex":0},{"imageOffset":661128,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::run::h541c11fea84a38ec","symbolLocation":1784,"imageIndex":0},{"imageOffset":659156,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::h60671eb717a34c70","symbolLocation":104,"imageIndex":0},{"imageOffset":725700,"symbol":"tokio::runtime::context::scoped::Scoped$LT$T$GT$::set::h263d3206a13c783c","symbolLocation":148,"imageIndex":0},{"imageOffset":629280,"symbol":"tokio::runtime::context::set_scheduler::_$u7b$$u7b$closure$u7d$$u7d$::h74a8e23df56c9882","symbolLocation":40,"imageIndex":0},{"imageOffset":754924,"symbol":"std::thread::local::LocalKey$LT$T$GT$::try*with::h2f963d543150c87c","symbolLocation":196,"imageIndex":0},{"imageOffset":753488,"symbol":"std::thread::local::LocalKey$LT$T$GT$::with::hbd17f06bbba52edc","symbolLocation":24,"imageIndex":0},{"imageOffset":629228,"symbol":"tokio::runtime::context::set_scheduler::heb413530e9784dfc","symbolLocation":68,"imageIndex":0},{"imageOffset":658936,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::h79a1aa630e49b68a","symbolLocation":248,"imageIndex":0},{"imageOffset":560916,"symbol":"tokio::runtime::context::runtime::enter*runtime::hb50775e529a9a617","symbolLocation":188,"imageIndex":0},{"imageOffset":658520,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::h79855d48ee1e3c6a","symbolLocation":600,"imageIndex":0},{"imageOffset":657908,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Launch::launch::*$u7b$$u7b$closure$u7d$$u7d$::he167c16b6e6899e4","symbolLocation":24,"imageIndex":0},{"imageOffset":804156,"symbol":"_$LT$tokio..runtime..blocking..task..BlockingTask$LT$T$GT$$u20$as$u20$core..future..future..Future$GT$::poll::h8b9db1efbc4877eb","symbolLocation":136,"imageIndex":0},{"imageOffset":405416,"symbol":"tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::_$u7b$$u7b$closure$u7d$$u7d$::h904262c7dc2f88b0","symbolLocation":192,"imageIndex":0},{"imageOffset":404756,"symbol":"tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::h973d3c0095d258c4","symbolLocation":72,"imageIndex":0},{"imageOffset":401112,"symbol":"tokio::runtime::task::harness::poll*future::*$u7b$$u7b$closure$u7d$$u7d$::h8fa5ae0829102149","symbolLocation":64,"imageIndex":0},{"imageOffset":536564,"symbol":"_$LT$core..panic..unwind_safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2974776e7861df5d","symbolLocation":44,"imageIndex":0},{"imageOffset":483764,"symbol":"std::panicking::catch_unwind::do_call::h792e9a0912086d16","symbolLocation":72,"imageIndex":0},{"imageOffset":482892,"symbol":"\_\_rust_try","symbolLocation":32,"imageIndex":0},{"imageOffset":471312,"symbol":"std::panic::catch_unwind::h4474725978f361f4","symbolLocation":96,"imageIndex":0},{"imageOffset":400496,"symbol":"tokio::runtime::task::harness::poll_future::hcf462599407f235c","symbolLocation":96,"imageIndex":0},{"imageOffset":395688,"symbol":"tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll_inner::h5e6f168e02291d20","symbolLocation":160,"imageIndex":0},{"imageOffset":395384,"symbol":"tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll::h8a00a85251f71f0c","symbolLocation":28,"imageIndex":0},{"imageOffset":499856,"symbol":"tokio::runtime::task::raw::poll::hf96de62ba33ad657","symbolLocation":36,"imageIndex":0},{"imageOffset":499120,"symbol":"tokio::runtime::task::raw::RawTask::poll::hde83a8d818768542","symbolLocation":52,"imageIndex":0},{"imageOffset":532168,"symbol":"tokio::runtime::task::UnownedTask$LT$S$GT$::run::h7ce7c13c00052430","symbolLocation":64,"imageIndex":0},{"imageOffset":569720,"symbol":"tokio::runtime::blocking::pool::Task::run::hf56d840fd68c7fdc","symbolLocation":28,"imageIndex":0},{"imageOffset":576944,"symbol":"tokio::runtime::blocking::pool::Inner::run::hd581e97506257439","symbolLocation":512,"imageIndex":0},{"imageOffset":576284,"symbol":"tokio::runtime::blocking::pool::Spawner::spawn_thread::_$u7b$$u7b$closure$u7d$$u7d$::h2a632d4852cdd40e","symbolLocation":144,"imageIndex":0},{"imageOffset":782604,"symbol":"std::sys::backtrace::**rust*begin_short_backtrace::h51232d61e41dc60c","symbolLocation":16,"imageIndex":0},{"imageOffset":542208,"symbol":"std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::hbf84c81d58c2715d","symbolLocation":116,"imageIndex":0},{"imageOffset":536620,"symbol":"\_$LT$core..panic..unwind_safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2c0e201eb4dddbb7","symbolLocation":44,"imageIndex":0},{"imageOffset":483856,"symbol":"std::panicking::catch_unwind::do_call::h94ffcffc7cdce087","symbolLocation":68,"imageIndex":0},{"imageOffset":562020,"symbol":"**rust*try","symbolLocation":32,"imageIndex":0},{"imageOffset":541012,"symbol":"std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::haea346fd331ce539","symbolLocation":728,"imageIndex":0},{"imageOffset":411316,"symbol":"core::ops::function::FnOnce::call_once$u7b$$u7b$vtable.shim$u7d$$u7d$::h12a95acb393be819","symbolLocation":24,"imageIndex":0},{"imageOffset":1492320,"symbol":"std::sys::pal::unix::thread::Thread::new::thread_start::h636d41ad3c63ac40","symbolLocation":56,"imageIndex":0},{"imageOffset":27660,"symbol":"\_pthread_start","symbolLocation":136,"imageIndex":12},{"imageOffset":7040,"symbol":"thread_start","symbolLocation":8,"imageIndex":12}]},{"id":65463674,"name":"tokio-runtime-worker","threadState":{"x":[{"value":260},{"value":0},{"value":256},{"value":0},{"value":0},{"value":160},{"value":0},{"value":0},{"value":6168693928},{"value":0},{"value":0},{"value":2},{"value":2},{"value":0},{"value":0},{"value":0},{"value":305},{"value":8326616336},{"value":0},{"value":5244994200},{"value":5244994264},{"value":6168703200},{"value":0},{"value":0},{"value":256},{"value":257},{"value":512},{"value":0},{"value":0}],"flavor":"ARM_THREAD_STATE64","lr":{"value":6464438496},"cpsr":{"value":1610616832},"fp":{"value":6168694048},"sp":{"value":6168693904},"esr":{"value":1442840704,"description":" Address size fault"},"pc":{"value":6464181196},"far":{"value":0}},"frames":[{"imageOffset":17356,"symbol":"\_\_psynch_cvwait","symbolLocation":8,"imageIndex":3},{"imageOffset":28896,"symbol":"\_pthread_cond_wait","symbolLocation":984,"imageIndex":12},{"imageOffset":1289468,"symbol":"_$LT$parking*lot_core..thread_parker..imp..ThreadParker$u20$as$u20$parking_lot_core..thread_parker..ThreadParkerT$GT$::park::h0e2d0998749974de","symbolLocation":256,"imageIndex":0},{"imageOffset":1257704,"symbol":"parking_lot_core::parking_lot::park::*$u7b$$u7b$closure$u7d$$u7d$::h0b4fc83101e8557f","symbolLocation":748,"imageIndex":0},{"imageOffset":1256464,"symbol":"parking*lot_core::parking_lot::park::hd04bbb424560662a","symbolLocation":296,"imageIndex":0},{"imageOffset":1286760,"symbol":"parking_lot::condvar::Condvar::wait_until_internal::h8640b2ba350582ed","symbolLocation":124,"imageIndex":0},{"imageOffset":537312,"symbol":"parking_lot::condvar::Condvar::wait::h6618c4600aa6d512","symbolLocation":68,"imageIndex":0},{"imageOffset":798236,"symbol":"tokio::loom::std::parking_lot::Condvar::wait::hc858434e42aaccb1","symbolLocation":36,"imageIndex":0},{"imageOffset":773892,"symbol":"tokio::runtime::scheduler::multi_thread::park::Inner::park_condvar::h2267f8a57982d137","symbolLocation":264,"imageIndex":0},{"imageOffset":773552,"symbol":"tokio::runtime::scheduler::multi_thread::park::Inner::park::h3c3dbc8c396a0894","symbolLocation":168,"imageIndex":0},{"imageOffset":772836,"symbol":"tokio::runtime::scheduler::multi_thread::park::Parker::park::h010fa0d10ce96e9b","symbolLocation":40,"imageIndex":0},{"imageOffset":667856,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::park_timeout::h37500a9fc2ffb756","symbolLocation":776,"imageIndex":0},{"imageOffset":666572,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::park::hd6c55373d20068c0","symbolLocation":968,"imageIndex":0},{"imageOffset":661128,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::run::h541c11fea84a38ec","symbolLocation":1784,"imageIndex":0},{"imageOffset":659156,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::h60671eb717a34c70","symbolLocation":104,"imageIndex":0},{"imageOffset":725700,"symbol":"tokio::runtime::context::scoped::Scoped$LT$T$GT$::set::h263d3206a13c783c","symbolLocation":148,"imageIndex":0},{"imageOffset":629280,"symbol":"tokio::runtime::context::set_scheduler::_$u7b$$u7b$closure$u7d$$u7d$::h74a8e23df56c9882","symbolLocation":40,"imageIndex":0},{"imageOffset":754924,"symbol":"std::thread::local::LocalKey$LT$T$GT$::try*with::h2f963d543150c87c","symbolLocation":196,"imageIndex":0},{"imageOffset":753488,"symbol":"std::thread::local::LocalKey$LT$T$GT$::with::hbd17f06bbba52edc","symbolLocation":24,"imageIndex":0},{"imageOffset":629228,"symbol":"tokio::runtime::context::set_scheduler::heb413530e9784dfc","symbolLocation":68,"imageIndex":0},{"imageOffset":658936,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::h79a1aa630e49b68a","symbolLocation":248,"imageIndex":0},{"imageOffset":560916,"symbol":"tokio::runtime::context::runtime::enter*runtime::hb50775e529a9a617","symbolLocation":188,"imageIndex":0},{"imageOffset":658520,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::h79855d48ee1e3c6a","symbolLocation":600,"imageIndex":0},{"imageOffset":657908,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Launch::launch::*$u7b$$u7b$closure$u7d$$u7d$::he167c16b6e6899e4","symbolLocation":24,"imageIndex":0},{"imageOffset":804156,"symbol":"_$LT$tokio..runtime..blocking..task..BlockingTask$LT$T$GT$$u20$as$u20$core..future..future..Future$GT$::poll::h8b9db1efbc4877eb","symbolLocation":136,"imageIndex":0},{"imageOffset":405416,"symbol":"tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::_$u7b$$u7b$closure$u7d$$u7d$::h904262c7dc2f88b0","symbolLocation":192,"imageIndex":0},{"imageOffset":404756,"symbol":"tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::h973d3c0095d258c4","symbolLocation":72,"imageIndex":0},{"imageOffset":401112,"symbol":"tokio::runtime::task::harness::poll*future::*$u7b$$u7b$closure$u7d$$u7d$::h8fa5ae0829102149","symbolLocation":64,"imageIndex":0},{"imageOffset":536564,"symbol":"_$LT$core..panic..unwind_safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2974776e7861df5d","symbolLocation":44,"imageIndex":0},{"imageOffset":483764,"symbol":"std::panicking::catch_unwind::do_call::h792e9a0912086d16","symbolLocation":72,"imageIndex":0},{"imageOffset":482892,"symbol":"\_\_rust_try","symbolLocation":32,"imageIndex":0},{"imageOffset":471312,"symbol":"std::panic::catch_unwind::h4474725978f361f4","symbolLocation":96,"imageIndex":0},{"imageOffset":400496,"symbol":"tokio::runtime::task::harness::poll_future::hcf462599407f235c","symbolLocation":96,"imageIndex":0},{"imageOffset":395688,"symbol":"tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll_inner::h5e6f168e02291d20","symbolLocation":160,"imageIndex":0},{"imageOffset":395384,"symbol":"tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll::h8a00a85251f71f0c","symbolLocation":28,"imageIndex":0},{"imageOffset":499856,"symbol":"tokio::runtime::task::raw::poll::hf96de62ba33ad657","symbolLocation":36,"imageIndex":0},{"imageOffset":499120,"symbol":"tokio::runtime::task::raw::RawTask::poll::hde83a8d818768542","symbolLocation":52,"imageIndex":0},{"imageOffset":532168,"symbol":"tokio::runtime::task::UnownedTask$LT$S$GT$::run::h7ce7c13c00052430","symbolLocation":64,"imageIndex":0},{"imageOffset":569720,"symbol":"tokio::runtime::blocking::pool::Task::run::hf56d840fd68c7fdc","symbolLocation":28,"imageIndex":0},{"imageOffset":576944,"symbol":"tokio::runtime::blocking::pool::Inner::run::hd581e97506257439","symbolLocation":512,"imageIndex":0},{"imageOffset":576284,"symbol":"tokio::runtime::blocking::pool::Spawner::spawn_thread::_$u7b$$u7b$closure$u7d$$u7d$::h2a632d4852cdd40e","symbolLocation":144,"imageIndex":0},{"imageOffset":782604,"symbol":"std::sys::backtrace::**rust*begin_short_backtrace::h51232d61e41dc60c","symbolLocation":16,"imageIndex":0},{"imageOffset":542208,"symbol":"std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::hbf84c81d58c2715d","symbolLocation":116,"imageIndex":0},{"imageOffset":536620,"symbol":"\_$LT$core..panic..unwind_safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2c0e201eb4dddbb7","symbolLocation":44,"imageIndex":0},{"imageOffset":483856,"symbol":"std::panicking::catch_unwind::do_call::h94ffcffc7cdce087","symbolLocation":68,"imageIndex":0},{"imageOffset":562020,"symbol":"**rust*try","symbolLocation":32,"imageIndex":0},{"imageOffset":541012,"symbol":"std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::haea346fd331ce539","symbolLocation":728,"imageIndex":0},{"imageOffset":411316,"symbol":"core::ops::function::FnOnce::call_once$u7b$$u7b$vtable.shim$u7d$$u7d$::h12a95acb393be819","symbolLocation":24,"imageIndex":0},{"imageOffset":1492320,"symbol":"std::sys::pal::unix::thread::Thread::new::thread_start::h636d41ad3c63ac40","symbolLocation":56,"imageIndex":0},{"imageOffset":27660,"symbol":"\_pthread_start","symbolLocation":136,"imageIndex":12},{"imageOffset":7040,"symbol":"thread_start","symbolLocation":8,"imageIndex":12}]},{"id":65463675,"name":"tokio-runtime-worker","threadState":{"x":[{"value":0},{"value":0},{"value":0},{"value":5234548224},{"value":1024},{"value":6170840632},{"value":0},{"value":6170842784},{"value":1024},{"value":1},{"value":5234548224},{"value":5232495000},{"value":0},{"value":0},{"value":18446744073707765675},{"value":74},{"value":363},{"value":8326618056},{"value":0},{"value":5232498624},{"value":4310777856},{"value":5232498512},{"value":4307103936,"symbolLocation":2096,"symbol":"tokio::runtime::task::waker::WAKER_VTABLE::hbf31b22039158b82"},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0}],"flavor":"ARM_THREAD_STATE64","lr":{"value":4305476092},"cpsr":{"value":2147487744},"fp":{"value":6170840720},"sp":{"value":6170840560},"esr":{"value":1442840704,"description":" Address size fault"},"pc":{"value":6464191748},"far":{"value":0}},"frames":[{"imageOffset":27908,"symbol":"kevent","symbolLocation":8,"imageIndex":3},{"imageOffset":875004,"symbol":"mio::sys::unix::selector::Selector::select::hd28ec6226acb265b","symbolLocation":200,"imageIndex":0},{"imageOffset":887620,"symbol":"mio::poll::Poll::poll::h98b744e8ad562494","symbolLocation":80,"imageIndex":0},{"imageOffset":802780,"symbol":"tokio::runtime::io::driver::Driver::turn::h2e3afb246fe3dfe1","symbolLocation":200,"imageIndex":0},{"imageOffset":802124,"symbol":"tokio::runtime::io::driver::Driver::park_timeout::h9902b72f5dc913da","symbolLocation":92,"imageIndex":0},{"imageOffset":466636,"symbol":"tokio::runtime::signal::Driver::park_timeout::ha8f650a98c982bba","symbolLocation":44,"imageIndex":0},{"imageOffset":465736,"symbol":"tokio::runtime::process::Driver::park_timeout::heb0a286fb4921f0c","symbolLocation":40,"imageIndex":0},{"imageOffset":615208,"symbol":"tokio::runtime::driver::IoStack::park_timeout::ha15f8ffc5cab851b","symbolLocation":136,"imageIndex":0},{"imageOffset":699268,"symbol":"tokio::runtime::time::Driver::park_thread_timeout::hdaf92e48edbd721d","symbolLocation":40,"imageIndex":0},{"imageOffset":698744,"symbol":"tokio::runtime::time::Driver::park_internal::h7c1fcf65c4343dc7","symbolLocation":784,"imageIndex":0},{"imageOffset":697760,"symbol":"tokio::runtime::time::Driver::park::h0aa3206d3b05a462","symbolLocation":40,"imageIndex":0},{"imageOffset":616332,"symbol":"tokio::runtime::driver::TimeDriver::park::h2589c02d0bf58b6c","symbolLocation":96,"imageIndex":0},{"imageOffset":613376,"symbol":"tokio::runtime::driver::Driver::park::h1bf58f7ae6ac8f16","symbolLocation":32,"imageIndex":0},{"imageOffset":774608,"symbol":"tokio::runtime::scheduler::multi_thread::park::Inner::park_driver::haaeede11d25027c4","symbolLocation":120,"imageIndex":0},{"imageOffset":773600,"symbol":"tokio::runtime::scheduler::multi_thread::park::Inner::park::h3c3dbc8c396a0894","symbolLocation":216,"imageIndex":0},{"imageOffset":772836,"symbol":"tokio::runtime::scheduler::multi_thread::park::Parker::park::h010fa0d10ce96e9b","symbolLocation":40,"imageIndex":0},{"imageOffset":667856,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::park_timeout::h37500a9fc2ffb756","symbolLocation":776,"imageIndex":0},{"imageOffset":666572,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::park::hd6c55373d20068c0","symbolLocation":968,"imageIndex":0},{"imageOffset":661128,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::run::h541c11fea84a38ec","symbolLocation":1784,"imageIndex":0},{"imageOffset":659156,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::_$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::h60671eb717a34c70","symbolLocation":104,"imageIndex":0},{"imageOffset":725700,"symbol":"tokio::runtime::context::scoped::Scoped$LT$T$GT$::set::h263d3206a13c783c","symbolLocation":148,"imageIndex":0},{"imageOffset":629280,"symbol":"tokio::runtime::context::set_scheduler::_$u7b$$u7b$closure$u7d$$u7d$::h74a8e23df56c9882","symbolLocation":40,"imageIndex":0},{"imageOffset":754924,"symbol":"std::thread::local::LocalKey$LT$T$GT$::try*with::h2f963d543150c87c","symbolLocation":196,"imageIndex":0},{"imageOffset":753488,"symbol":"std::thread::local::LocalKey$LT$T$GT$::with::hbd17f06bbba52edc","symbolLocation":24,"imageIndex":0},{"imageOffset":629228,"symbol":"tokio::runtime::context::set_scheduler::heb413530e9784dfc","symbolLocation":68,"imageIndex":0},{"imageOffset":658936,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::h79a1aa630e49b68a","symbolLocation":248,"imageIndex":0},{"imageOffset":560916,"symbol":"tokio::runtime::context::runtime::enter*runtime::hb50775e529a9a617","symbolLocation":188,"imageIndex":0},{"imageOffset":658520,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::h79855d48ee1e3c6a","symbolLocation":600,"imageIndex":0},{"imageOffset":657908,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Launch::launch::*$u7b$$u7b$closure$u7d$$u7d$::he167c16b6e6899e4","symbolLocation":24,"imageIndex":0},{"imageOffset":804156,"symbol":"_$LT$tokio..runtime..blocking..task..BlockingTask$LT$T$GT$$u20$as$u20$core..future..future..Future$GT$::poll::h8b9db1efbc4877eb","symbolLocation":136,"imageIndex":0},{"imageOffset":405416,"symbol":"tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::_$u7b$$u7b$closure$u7d$$u7d$::h904262c7dc2f88b0","symbolLocation":192,"imageIndex":0},{"imageOffset":404756,"symbol":"tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::h973d3c0095d258c4","symbolLocation":72,"imageIndex":0},{"imageOffset":401112,"symbol":"tokio::runtime::task::harness::poll*future::*$u7b$$u7b$closure$u7d$$u7d$::h8fa5ae0829102149","symbolLocation":64,"imageIndex":0},{"imageOffset":536564,"symbol":"_$LT$core..panic..unwind_safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2974776e7861df5d","symbolLocation":44,"imageIndex":0},{"imageOffset":483764,"symbol":"std::panicking::catch_unwind::do_call::h792e9a0912086d16","symbolLocation":72,"imageIndex":0},{"imageOffset":482892,"symbol":"\_\_rust_try","symbolLocation":32,"imageIndex":0},{"imageOffset":471312,"symbol":"std::panic::catch_unwind::h4474725978f361f4","symbolLocation":96,"imageIndex":0},{"imageOffset":400496,"symbol":"tokio::runtime::task::harness::poll_future::hcf462599407f235c","symbolLocation":96,"imageIndex":0},{"imageOffset":395688,"symbol":"tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll_inner::h5e6f168e02291d20","symbolLocation":160,"imageIndex":0},{"imageOffset":395384,"symbol":"tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll::h8a00a85251f71f0c","symbolLocation":28,"imageIndex":0},{"imageOffset":499856,"symbol":"tokio::runtime::task::raw::poll::hf96de62ba33ad657","symbolLocation":36,"imageIndex":0},{"imageOffset":499120,"symbol":"tokio::runtime::task::raw::RawTask::poll::hde83a8d818768542","symbolLocation":52,"imageIndex":0},{"imageOffset":532168,"symbol":"tokio::runtime::task::UnownedTask$LT$S$GT$::run::h7ce7c13c00052430","symbolLocation":64,"imageIndex":0},{"imageOffset":569720,"symbol":"tokio::runtime::blocking::pool::Task::run::hf56d840fd68c7fdc","symbolLocation":28,"imageIndex":0},{"imageOffset":576944,"symbol":"tokio::runtime::blocking::pool::Inner::run::hd581e97506257439","symbolLocation":512,"imageIndex":0},{"imageOffset":576284,"symbol":"tokio::runtime::blocking::pool::Spawner::spawn_thread::_$u7b$$u7b$closure$u7d$$u7d$::h2a632d4852cdd40e","symbolLocation":144,"imageIndex":0},{"imageOffset":782604,"symbol":"std::sys::backtrace::**rust*begin_short_backtrace::h51232d61e41dc60c","symbolLocation":16,"imageIndex":0},{"imageOffset":542208,"symbol":"std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::hbf84c81d58c2715d","symbolLocation":116,"imageIndex":0},{"imageOffset":536620,"symbol":"\_$LT$core..panic..unwind_safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2c0e201eb4dddbb7","symbolLocation":44,"imageIndex":0},{"imageOffset":483856,"symbol":"std::panicking::catch_unwind::do_call::h94ffcffc7cdce087","symbolLocation":68,"imageIndex":0},{"imageOffset":562020,"symbol":"**rust*try","symbolLocation":32,"imageIndex":0},{"imageOffset":541012,"symbol":"std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::haea346fd331ce539","symbolLocation":728,"imageIndex":0},{"imageOffset":411316,"symbol":"core::ops::function::FnOnce::call_once$u7b$$u7b$vtable.shim$u7d$$u7d$::h12a95acb393be819","symbolLocation":24,"imageIndex":0},{"imageOffset":1492320,"symbol":"std::sys::pal::unix::thread::Thread::new::thread_start::h636d41ad3c63ac40","symbolLocation":56,"imageIndex":0},{"imageOffset":27660,"symbol":"\_pthread_start","symbolLocation":136,"imageIndex":12},{"imageOffset":7040,"symbol":"thread_start","symbolLocation":8,"imageIndex":12}]},{"id":65463676,"name":"tokio-runtime-worker","threadState":{"x":[{"value":260},{"value":0},{"value":256},{"value":0},{"value":0},{"value":160},{"value":0},{"value":0},{"value":6172986536},{"value":0},{"value":0},{"value":2},{"value":2},{"value":0},{"value":0},{"value":0},{"value":305},{"value":8326616336},{"value":0},{"value":5233461784},{"value":5233461848},{"value":6172995808},{"value":0},{"value":0},{"value":256},{"value":257},{"value":512},{"value":0},{"value":0}],"flavor":"ARM_THREAD_STATE64","lr":{"value":6464438496},"cpsr":{"value":1610616832},"fp":{"value":6172986656},"sp":{"value":6172986512},"esr":{"value":1442840704,"description":" Address size fault"},"pc":{"value":6464181196},"far":{"value":0}},"frames":[{"imageOffset":17356,"symbol":"\_\_psynch_cvwait","symbolLocation":8,"imageIndex":3},{"imageOffset":28896,"symbol":"\_pthread_cond_wait","symbolLocation":984,"imageIndex":12},{"imageOffset":1289468,"symbol":"_$LT$parking*lot_core..thread_parker..imp..ThreadParker$u20$as$u20$parking_lot_core..thread_parker..ThreadParkerT$GT$::park::h0e2d0998749974de","symbolLocation":256,"imageIndex":0},{"imageOffset":1257704,"symbol":"parking_lot_core::parking_lot::park::*$u7b$$u7b$closure$u7d$$u7d$::h0b4fc83101e8557f","symbolLocation":748,"imageIndex":0},{"imageOffset":1256464,"symbol":"parking*lot_core::parking_lot::park::hd04bbb424560662a","symbolLocation":296,"imageIndex":0},{"imageOffset":1286760,"symbol":"parking_lot::condvar::Condvar::wait_until_internal::h8640b2ba350582ed","symbolLocation":124,"imageIndex":0},{"imageOffset":537312,"symbol":"parking_lot::condvar::Condvar::wait::h6618c4600aa6d512","symbolLocation":68,"imageIndex":0},{"imageOffset":798236,"symbol":"tokio::loom::std::parking_lot::Condvar::wait::hc858434e42aaccb1","symbolLocation":36,"imageIndex":0},{"imageOffset":773892,"symbol":"tokio::runtime::scheduler::multi_thread::park::Inner::park_condvar::h2267f8a57982d137","symbolLocation":264,"imageIndex":0},{"imageOffset":773552,"symbol":"tokio::runtime::scheduler::multi_thread::park::Inner::park::h3c3dbc8c396a0894","symbolLocation":168,"imageIndex":0},{"imageOffset":772836,"symbol":"tokio::runtime::scheduler::multi_thread::park::Parker::park::h010fa0d10ce96e9b","symbolLocation":40,"imageIndex":0},{"imageOffset":667856,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::park_timeout::h37500a9fc2ffb756","symbolLocation":776,"imageIndex":0},{"imageOffset":666572,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::park::hd6c55373d20068c0","symbolLocation":968,"imageIndex":0},{"imageOffset":661128,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::run::h541c11fea84a38ec","symbolLocation":1784,"imageIndex":0},{"imageOffset":659156,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::h60671eb717a34c70","symbolLocation":104,"imageIndex":0},{"imageOffset":725700,"symbol":"tokio::runtime::context::scoped::Scoped$LT$T$GT$::set::h263d3206a13c783c","symbolLocation":148,"imageIndex":0},{"imageOffset":629280,"symbol":"tokio::runtime::context::set_scheduler::_$u7b$$u7b$closure$u7d$$u7d$::h74a8e23df56c9882","symbolLocation":40,"imageIndex":0},{"imageOffset":754924,"symbol":"std::thread::local::LocalKey$LT$T$GT$::try*with::h2f963d543150c87c","symbolLocation":196,"imageIndex":0},{"imageOffset":753488,"symbol":"std::thread::local::LocalKey$LT$T$GT$::with::hbd17f06bbba52edc","symbolLocation":24,"imageIndex":0},{"imageOffset":629228,"symbol":"tokio::runtime::context::set_scheduler::heb413530e9784dfc","symbolLocation":68,"imageIndex":0},{"imageOffset":658936,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::h79a1aa630e49b68a","symbolLocation":248,"imageIndex":0},{"imageOffset":560916,"symbol":"tokio::runtime::context::runtime::enter*runtime::hb50775e529a9a617","symbolLocation":188,"imageIndex":0},{"imageOffset":658520,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::h79855d48ee1e3c6a","symbolLocation":600,"imageIndex":0},{"imageOffset":657908,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Launch::launch::*$u7b$$u7b$closure$u7d$$u7d$::he167c16b6e6899e4","symbolLocation":24,"imageIndex":0},{"imageOffset":804156,"symbol":"_$LT$tokio..runtime..blocking..task..BlockingTask$LT$T$GT$$u20$as$u20$core..future..future..Future$GT$::poll::h8b9db1efbc4877eb","symbolLocation":136,"imageIndex":0},{"imageOffset":405416,"symbol":"tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::_$u7b$$u7b$closure$u7d$$u7d$::h904262c7dc2f88b0","symbolLocation":192,"imageIndex":0},{"imageOffset":404756,"symbol":"tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::h973d3c0095d258c4","symbolLocation":72,"imageIndex":0},{"imageOffset":401112,"symbol":"tokio::runtime::task::harness::poll*future::*$u7b$$u7b$closure$u7d$$u7d$::h8fa5ae0829102149","symbolLocation":64,"imageIndex":0},{"imageOffset":536564,"symbol":"_$LT$core..panic..unwind_safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2974776e7861df5d","symbolLocation":44,"imageIndex":0},{"imageOffset":483764,"symbol":"std::panicking::catch_unwind::do_call::h792e9a0912086d16","symbolLocation":72,"imageIndex":0},{"imageOffset":482892,"symbol":"\_\_rust_try","symbolLocation":32,"imageIndex":0},{"imageOffset":471312,"symbol":"std::panic::catch_unwind::h4474725978f361f4","symbolLocation":96,"imageIndex":0},{"imageOffset":400496,"symbol":"tokio::runtime::task::harness::poll_future::hcf462599407f235c","symbolLocation":96,"imageIndex":0},{"imageOffset":395688,"symbol":"tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll_inner::h5e6f168e02291d20","symbolLocation":160,"imageIndex":0},{"imageOffset":395384,"symbol":"tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll::h8a00a85251f71f0c","symbolLocation":28,"imageIndex":0},{"imageOffset":499856,"symbol":"tokio::runtime::task::raw::poll::hf96de62ba33ad657","symbolLocation":36,"imageIndex":0},{"imageOffset":499120,"symbol":"tokio::runtime::task::raw::RawTask::poll::hde83a8d818768542","symbolLocation":52,"imageIndex":0},{"imageOffset":532168,"symbol":"tokio::runtime::task::UnownedTask$LT$S$GT$::run::h7ce7c13c00052430","symbolLocation":64,"imageIndex":0},{"imageOffset":569720,"symbol":"tokio::runtime::blocking::pool::Task::run::hf56d840fd68c7fdc","symbolLocation":28,"imageIndex":0},{"imageOffset":576944,"symbol":"tokio::runtime::blocking::pool::Inner::run::hd581e97506257439","symbolLocation":512,"imageIndex":0},{"imageOffset":576284,"symbol":"tokio::runtime::blocking::pool::Spawner::spawn_thread::_$u7b$$u7b$closure$u7d$$u7d$::h2a632d4852cdd40e","symbolLocation":144,"imageIndex":0},{"imageOffset":782604,"symbol":"std::sys::backtrace::**rust*begin_short_backtrace::h51232d61e41dc60c","symbolLocation":16,"imageIndex":0},{"imageOffset":542208,"symbol":"std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::hbf84c81d58c2715d","symbolLocation":116,"imageIndex":0},{"imageOffset":536620,"symbol":"\_$LT$core..panic..unwind_safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2c0e201eb4dddbb7","symbolLocation":44,"imageIndex":0},{"imageOffset":483856,"symbol":"std::panicking::catch_unwind::do_call::h94ffcffc7cdce087","symbolLocation":68,"imageIndex":0},{"imageOffset":562020,"symbol":"**rust*try","symbolLocation":32,"imageIndex":0},{"imageOffset":541012,"symbol":"std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::haea346fd331ce539","symbolLocation":728,"imageIndex":0},{"imageOffset":411316,"symbol":"core::ops::function::FnOnce::call_once$u7b$$u7b$vtable.shim$u7d$$u7d$::h12a95acb393be819","symbolLocation":24,"imageIndex":0},{"imageOffset":1492320,"symbol":"std::sys::pal::unix::thread::Thread::new::thread_start::h636d41ad3c63ac40","symbolLocation":56,"imageIndex":0},{"imageOffset":27660,"symbol":"\_pthread_start","symbolLocation":136,"imageIndex":12},{"imageOffset":7040,"symbol":"thread_start","symbolLocation":8,"imageIndex":12}]},{"id":65463677,"name":"tokio-runtime-worker","threadState":{"x":[{"value":260},{"value":0},{"value":256},{"value":0},{"value":0},{"value":160},{"value":0},{"value":0},{"value":6175132840},{"value":0},{"value":0},{"value":2},{"value":2},{"value":0},{"value":0},{"value":0},{"value":305},{"value":8326616336},{"value":0},{"value":5233462680},{"value":5233462744},{"value":6175142112},{"value":0},{"value":0},{"value":256},{"value":257},{"value":512},{"value":0},{"value":0}],"flavor":"ARM_THREAD_STATE64","lr":{"value":6464438496},"cpsr":{"value":1610616832},"fp":{"value":6175132960},"sp":{"value":6175132816},"esr":{"value":1442840704,"description":" Address size fault"},"pc":{"value":6464181196},"far":{"value":0}},"frames":[{"imageOffset":17356,"symbol":"\_\_psynch_cvwait","symbolLocation":8,"imageIndex":3},{"imageOffset":28896,"symbol":"\_pthread_cond_wait","symbolLocation":984,"imageIndex":12},{"imageOffset":1289468,"symbol":"_$LT$parking*lot_core..thread_parker..imp..ThreadParker$u20$as$u20$parking_lot_core..thread_parker..ThreadParkerT$GT$::park::h0e2d0998749974de","symbolLocation":256,"imageIndex":0},{"imageOffset":1257704,"symbol":"parking_lot_core::parking_lot::park::*$u7b$$u7b$closure$u7d$$u7d$::h0b4fc83101e8557f","symbolLocation":748,"imageIndex":0},{"imageOffset":1256464,"symbol":"parking*lot_core::parking_lot::park::hd04bbb424560662a","symbolLocation":296,"imageIndex":0},{"imageOffset":1286760,"symbol":"parking_lot::condvar::Condvar::wait_until_internal::h8640b2ba350582ed","symbolLocation":124,"imageIndex":0},{"imageOffset":537312,"symbol":"parking_lot::condvar::Condvar::wait::h6618c4600aa6d512","symbolLocation":68,"imageIndex":0},{"imageOffset":798236,"symbol":"tokio::loom::std::parking_lot::Condvar::wait::hc858434e42aaccb1","symbolLocation":36,"imageIndex":0},{"imageOffset":773892,"symbol":"tokio::runtime::scheduler::multi_thread::park::Inner::park_condvar::h2267f8a57982d137","symbolLocation":264,"imageIndex":0},{"imageOffset":773552,"symbol":"tokio::runtime::scheduler::multi_thread::park::Inner::park::h3c3dbc8c396a0894","symbolLocation":168,"imageIndex":0},{"imageOffset":772836,"symbol":"tokio::runtime::scheduler::multi_thread::park::Parker::park::h010fa0d10ce96e9b","symbolLocation":40,"imageIndex":0},{"imageOffset":667856,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::park_timeout::h37500a9fc2ffb756","symbolLocation":776,"imageIndex":0},{"imageOffset":666572,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::park::hd6c55373d20068c0","symbolLocation":968,"imageIndex":0},{"imageOffset":661128,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::run::h541c11fea84a38ec","symbolLocation":1784,"imageIndex":0},{"imageOffset":659156,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::h60671eb717a34c70","symbolLocation":104,"imageIndex":0},{"imageOffset":725700,"symbol":"tokio::runtime::context::scoped::Scoped$LT$T$GT$::set::h263d3206a13c783c","symbolLocation":148,"imageIndex":0},{"imageOffset":629280,"symbol":"tokio::runtime::context::set_scheduler::_$u7b$$u7b$closure$u7d$$u7d$::h74a8e23df56c9882","symbolLocation":40,"imageIndex":0},{"imageOffset":754924,"symbol":"std::thread::local::LocalKey$LT$T$GT$::try*with::h2f963d543150c87c","symbolLocation":196,"imageIndex":0},{"imageOffset":753488,"symbol":"std::thread::local::LocalKey$LT$T$GT$::with::hbd17f06bbba52edc","symbolLocation":24,"imageIndex":0},{"imageOffset":629228,"symbol":"tokio::runtime::context::set_scheduler::heb413530e9784dfc","symbolLocation":68,"imageIndex":0},{"imageOffset":658936,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::h79a1aa630e49b68a","symbolLocation":248,"imageIndex":0},{"imageOffset":560916,"symbol":"tokio::runtime::context::runtime::enter*runtime::hb50775e529a9a617","symbolLocation":188,"imageIndex":0},{"imageOffset":658520,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::h79855d48ee1e3c6a","symbolLocation":600,"imageIndex":0},{"imageOffset":657908,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Launch::launch::*$u7b$$u7b$closure$u7d$$u7d$::he167c16b6e6899e4","symbolLocation":24,"imageIndex":0},{"imageOffset":804156,"symbol":"_$LT$tokio..runtime..blocking..task..BlockingTask$LT$T$GT$$u20$as$u20$core..future..future..Future$GT$::poll::h8b9db1efbc4877eb","symbolLocation":136,"imageIndex":0},{"imageOffset":405416,"symbol":"tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::_$u7b$$u7b$closure$u7d$$u7d$::h904262c7dc2f88b0","symbolLocation":192,"imageIndex":0},{"imageOffset":404756,"symbol":"tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::h973d3c0095d258c4","symbolLocation":72,"imageIndex":0},{"imageOffset":401112,"symbol":"tokio::runtime::task::harness::poll*future::*$u7b$$u7b$closure$u7d$$u7d$::h8fa5ae0829102149","symbolLocation":64,"imageIndex":0},{"imageOffset":536564,"symbol":"_$LT$core..panic..unwind_safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2974776e7861df5d","symbolLocation":44,"imageIndex":0},{"imageOffset":483764,"symbol":"std::panicking::catch_unwind::do_call::h792e9a0912086d16","symbolLocation":72,"imageIndex":0},{"imageOffset":482892,"symbol":"\_\_rust_try","symbolLocation":32,"imageIndex":0},{"imageOffset":471312,"symbol":"std::panic::catch_unwind::h4474725978f361f4","symbolLocation":96,"imageIndex":0},{"imageOffset":400496,"symbol":"tokio::runtime::task::harness::poll_future::hcf462599407f235c","symbolLocation":96,"imageIndex":0},{"imageOffset":395688,"symbol":"tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll_inner::h5e6f168e02291d20","symbolLocation":160,"imageIndex":0},{"imageOffset":395384,"symbol":"tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll::h8a00a85251f71f0c","symbolLocation":28,"imageIndex":0},{"imageOffset":499856,"symbol":"tokio::runtime::task::raw::poll::hf96de62ba33ad657","symbolLocation":36,"imageIndex":0},{"imageOffset":499120,"symbol":"tokio::runtime::task::raw::RawTask::poll::hde83a8d818768542","symbolLocation":52,"imageIndex":0},{"imageOffset":532168,"symbol":"tokio::runtime::task::UnownedTask$LT$S$GT$::run::h7ce7c13c00052430","symbolLocation":64,"imageIndex":0},{"imageOffset":569720,"symbol":"tokio::runtime::blocking::pool::Task::run::hf56d840fd68c7fdc","symbolLocation":28,"imageIndex":0},{"imageOffset":576944,"symbol":"tokio::runtime::blocking::pool::Inner::run::hd581e97506257439","symbolLocation":512,"imageIndex":0},{"imageOffset":576284,"symbol":"tokio::runtime::blocking::pool::Spawner::spawn_thread::_$u7b$$u7b$closure$u7d$$u7d$::h2a632d4852cdd40e","symbolLocation":144,"imageIndex":0},{"imageOffset":782604,"symbol":"std::sys::backtrace::**rust*begin_short_backtrace::h51232d61e41dc60c","symbolLocation":16,"imageIndex":0},{"imageOffset":542208,"symbol":"std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::hbf84c81d58c2715d","symbolLocation":116,"imageIndex":0},{"imageOffset":536620,"symbol":"\_$LT$core..panic..unwind_safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2c0e201eb4dddbb7","symbolLocation":44,"imageIndex":0},{"imageOffset":483856,"symbol":"std::panicking::catch_unwind::do_call::h94ffcffc7cdce087","symbolLocation":68,"imageIndex":0},{"imageOffset":562020,"symbol":"**rust*try","symbolLocation":32,"imageIndex":0},{"imageOffset":541012,"symbol":"std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::haea346fd331ce539","symbolLocation":728,"imageIndex":0},{"imageOffset":411316,"symbol":"core::ops::function::FnOnce::call_once$u7b$$u7b$vtable.shim$u7d$$u7d$::h12a95acb393be819","symbolLocation":24,"imageIndex":0},{"imageOffset":1492320,"symbol":"std::sys::pal::unix::thread::Thread::new::thread_start::h636d41ad3c63ac40","symbolLocation":56,"imageIndex":0},{"imageOffset":27660,"symbol":"\_pthread_start","symbolLocation":136,"imageIndex":12},{"imageOffset":7040,"symbol":"thread_start","symbolLocation":8,"imageIndex":12}]},{"id":65463678,"name":"tokio-runtime-worker","threadState":{"x":[{"value":260},{"value":0},{"value":256},{"value":0},{"value":0},{"value":160},{"value":0},{"value":0},{"value":6177279144},{"value":0},{"value":0},{"value":2},{"value":2},{"value":0},{"value":0},{"value":0},{"value":305},{"value":8326616336},{"value":0},{"value":5232502008},{"value":5232502072},{"value":6177288416},{"value":0},{"value":0},{"value":256},{"value":257},{"value":512},{"value":0},{"value":0}],"flavor":"ARM_THREAD_STATE64","lr":{"value":6464438496},"cpsr":{"value":1610616832},"fp":{"value":6177279264},"sp":{"value":6177279120},"esr":{"value":1442840704,"description":" Address size fault"},"pc":{"value":6464181196},"far":{"value":0}},"frames":[{"imageOffset":17356,"symbol":"\_\_psynch_cvwait","symbolLocation":8,"imageIndex":3},{"imageOffset":28896,"symbol":"\_pthread_cond_wait","symbolLocation":984,"imageIndex":12},{"imageOffset":1289468,"symbol":"_$LT$parking*lot_core..thread_parker..imp..ThreadParker$u20$as$u20$parking_lot_core..thread_parker..ThreadParkerT$GT$::park::h0e2d0998749974de","symbolLocation":256,"imageIndex":0},{"imageOffset":1257704,"symbol":"parking_lot_core::parking_lot::park::*$u7b$$u7b$closure$u7d$$u7d$::h0b4fc83101e8557f","symbolLocation":748,"imageIndex":0},{"imageOffset":1256464,"symbol":"parking*lot_core::parking_lot::park::hd04bbb424560662a","symbolLocation":296,"imageIndex":0},{"imageOffset":1286760,"symbol":"parking_lot::condvar::Condvar::wait_until_internal::h8640b2ba350582ed","symbolLocation":124,"imageIndex":0},{"imageOffset":537312,"symbol":"parking_lot::condvar::Condvar::wait::h6618c4600aa6d512","symbolLocation":68,"imageIndex":0},{"imageOffset":798236,"symbol":"tokio::loom::std::parking_lot::Condvar::wait::hc858434e42aaccb1","symbolLocation":36,"imageIndex":0},{"imageOffset":773892,"symbol":"tokio::runtime::scheduler::multi_thread::park::Inner::park_condvar::h2267f8a57982d137","symbolLocation":264,"imageIndex":0},{"imageOffset":773552,"symbol":"tokio::runtime::scheduler::multi_thread::park::Inner::park::h3c3dbc8c396a0894","symbolLocation":168,"imageIndex":0},{"imageOffset":772836,"symbol":"tokio::runtime::scheduler::multi_thread::park::Parker::park::h010fa0d10ce96e9b","symbolLocation":40,"imageIndex":0},{"imageOffset":667856,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::park_timeout::h37500a9fc2ffb756","symbolLocation":776,"imageIndex":0},{"imageOffset":666572,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::park::hd6c55373d20068c0","symbolLocation":968,"imageIndex":0},{"imageOffset":661128,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::run::h541c11fea84a38ec","symbolLocation":1784,"imageIndex":0},{"imageOffset":659156,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::h60671eb717a34c70","symbolLocation":104,"imageIndex":0},{"imageOffset":725700,"symbol":"tokio::runtime::context::scoped::Scoped$LT$T$GT$::set::h263d3206a13c783c","symbolLocation":148,"imageIndex":0},{"imageOffset":629280,"symbol":"tokio::runtime::context::set_scheduler::_$u7b$$u7b$closure$u7d$$u7d$::h74a8e23df56c9882","symbolLocation":40,"imageIndex":0},{"imageOffset":754924,"symbol":"std::thread::local::LocalKey$LT$T$GT$::try*with::h2f963d543150c87c","symbolLocation":196,"imageIndex":0},{"imageOffset":753488,"symbol":"std::thread::local::LocalKey$LT$T$GT$::with::hbd17f06bbba52edc","symbolLocation":24,"imageIndex":0},{"imageOffset":629228,"symbol":"tokio::runtime::context::set_scheduler::heb413530e9784dfc","symbolLocation":68,"imageIndex":0},{"imageOffset":658936,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::h79a1aa630e49b68a","symbolLocation":248,"imageIndex":0},{"imageOffset":560916,"symbol":"tokio::runtime::context::runtime::enter*runtime::hb50775e529a9a617","symbolLocation":188,"imageIndex":0},{"imageOffset":658520,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::h79855d48ee1e3c6a","symbolLocation":600,"imageIndex":0},{"imageOffset":657908,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Launch::launch::*$u7b$$u7b$closure$u7d$$u7d$::he167c16b6e6899e4","symbolLocation":24,"imageIndex":0},{"imageOffset":804156,"symbol":"_$LT$tokio..runtime..blocking..task..BlockingTask$LT$T$GT$$u20$as$u20$core..future..future..Future$GT$::poll::h8b9db1efbc4877eb","symbolLocation":136,"imageIndex":0},{"imageOffset":405416,"symbol":"tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::_$u7b$$u7b$closure$u7d$$u7d$::h904262c7dc2f88b0","symbolLocation":192,"imageIndex":0},{"imageOffset":404756,"symbol":"tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::h973d3c0095d258c4","symbolLocation":72,"imageIndex":0},{"imageOffset":401112,"symbol":"tokio::runtime::task::harness::poll*future::*$u7b$$u7b$closure$u7d$$u7d$::h8fa5ae0829102149","symbolLocation":64,"imageIndex":0},{"imageOffset":536564,"symbol":"_$LT$core..panic..unwind_safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2974776e7861df5d","symbolLocation":44,"imageIndex":0},{"imageOffset":483764,"symbol":"std::panicking::catch_unwind::do_call::h792e9a0912086d16","symbolLocation":72,"imageIndex":0},{"imageOffset":482892,"symbol":"\_\_rust_try","symbolLocation":32,"imageIndex":0},{"imageOffset":471312,"symbol":"std::panic::catch_unwind::h4474725978f361f4","symbolLocation":96,"imageIndex":0},{"imageOffset":400496,"symbol":"tokio::runtime::task::harness::poll_future::hcf462599407f235c","symbolLocation":96,"imageIndex":0},{"imageOffset":395688,"symbol":"tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll_inner::h5e6f168e02291d20","symbolLocation":160,"imageIndex":0},{"imageOffset":395384,"symbol":"tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll::h8a00a85251f71f0c","symbolLocation":28,"imageIndex":0},{"imageOffset":499856,"symbol":"tokio::runtime::task::raw::poll::hf96de62ba33ad657","symbolLocation":36,"imageIndex":0},{"imageOffset":499120,"symbol":"tokio::runtime::task::raw::RawTask::poll::hde83a8d818768542","symbolLocation":52,"imageIndex":0},{"imageOffset":532168,"symbol":"tokio::runtime::task::UnownedTask$LT$S$GT$::run::h7ce7c13c00052430","symbolLocation":64,"imageIndex":0},{"imageOffset":569720,"symbol":"tokio::runtime::blocking::pool::Task::run::hf56d840fd68c7fdc","symbolLocation":28,"imageIndex":0},{"imageOffset":576944,"symbol":"tokio::runtime::blocking::pool::Inner::run::hd581e97506257439","symbolLocation":512,"imageIndex":0},{"imageOffset":576284,"symbol":"tokio::runtime::blocking::pool::Spawner::spawn_thread::_$u7b$$u7b$closure$u7d$$u7d$::h2a632d4852cdd40e","symbolLocation":144,"imageIndex":0},{"imageOffset":782604,"symbol":"std::sys::backtrace::**rust*begin_short_backtrace::h51232d61e41dc60c","symbolLocation":16,"imageIndex":0},{"imageOffset":542208,"symbol":"std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::hbf84c81d58c2715d","symbolLocation":116,"imageIndex":0},{"imageOffset":536620,"symbol":"\_$LT$core..panic..unwind_safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2c0e201eb4dddbb7","symbolLocation":44,"imageIndex":0},{"imageOffset":483856,"symbol":"std::panicking::catch_unwind::do_call::h94ffcffc7cdce087","symbolLocation":68,"imageIndex":0},{"imageOffset":562020,"symbol":"**rust*try","symbolLocation":32,"imageIndex":0},{"imageOffset":541012,"symbol":"std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::haea346fd331ce539","symbolLocation":728,"imageIndex":0},{"imageOffset":411316,"symbol":"core::ops::function::FnOnce::call_once$u7b$$u7b$vtable.shim$u7d$$u7d$::h12a95acb393be819","symbolLocation":24,"imageIndex":0},{"imageOffset":1492320,"symbol":"std::sys::pal::unix::thread::Thread::new::thread_start::h636d41ad3c63ac40","symbolLocation":56,"imageIndex":0},{"imageOffset":27660,"symbol":"\_pthread_start","symbolLocation":136,"imageIndex":12},{"imageOffset":7040,"symbol":"thread_start","symbolLocation":8,"imageIndex":12}]},{"id":65463679,"name":"tokio-runtime-worker","threadState":{"x":[{"value":260},{"value":0},{"value":256},{"value":0},{"value":0},{"value":160},{"value":0},{"value":0},{"value":6179425448},{"value":0},{"value":0},{"value":2},{"value":2},{"value":0},{"value":0},{"value":0},{"value":305},{"value":8326616336},{"value":0},{"value":5233463608},{"value":5233463672},{"value":6179434720},{"value":0},{"value":0},{"value":256},{"value":257},{"value":512},{"value":0},{"value":0}],"flavor":"ARM_THREAD_STATE64","lr":{"value":6464438496},"cpsr":{"value":1610616832},"fp":{"value":6179425568},"sp":{"value":6179425424},"esr":{"value":1442840704,"description":" Address size fault"},"pc":{"value":6464181196},"far":{"value":0}},"frames":[{"imageOffset":17356,"symbol":"\_\_psynch_cvwait","symbolLocation":8,"imageIndex":3},{"imageOffset":28896,"symbol":"\_pthread_cond_wait","symbolLocation":984,"imageIndex":12},{"imageOffset":1289468,"symbol":"_$LT$parking*lot_core..thread_parker..imp..ThreadParker$u20$as$u20$parking_lot_core..thread_parker..ThreadParkerT$GT$::park::h0e2d0998749974de","symbolLocation":256,"imageIndex":0},{"imageOffset":1257704,"symbol":"parking_lot_core::parking_lot::park::*$u7b$$u7b$closure$u7d$$u7d$::h0b4fc83101e8557f","symbolLocation":748,"imageIndex":0},{"imageOffset":1256464,"symbol":"parking*lot_core::parking_lot::park::hd04bbb424560662a","symbolLocation":296,"imageIndex":0},{"imageOffset":1286760,"symbol":"parking_lot::condvar::Condvar::wait_until_internal::h8640b2ba350582ed","symbolLocation":124,"imageIndex":0},{"imageOffset":537312,"symbol":"parking_lot::condvar::Condvar::wait::h6618c4600aa6d512","symbolLocation":68,"imageIndex":0},{"imageOffset":798236,"symbol":"tokio::loom::std::parking_lot::Condvar::wait::hc858434e42aaccb1","symbolLocation":36,"imageIndex":0},{"imageOffset":773892,"symbol":"tokio::runtime::scheduler::multi_thread::park::Inner::park_condvar::h2267f8a57982d137","symbolLocation":264,"imageIndex":0},{"imageOffset":773552,"symbol":"tokio::runtime::scheduler::multi_thread::park::Inner::park::h3c3dbc8c396a0894","symbolLocation":168,"imageIndex":0},{"imageOffset":772836,"symbol":"tokio::runtime::scheduler::multi_thread::park::Parker::park::h010fa0d10ce96e9b","symbolLocation":40,"imageIndex":0},{"imageOffset":667856,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::park_timeout::h37500a9fc2ffb756","symbolLocation":776,"imageIndex":0},{"imageOffset":666572,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::park::hd6c55373d20068c0","symbolLocation":968,"imageIndex":0},{"imageOffset":661128,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::run::h541c11fea84a38ec","symbolLocation":1784,"imageIndex":0},{"imageOffset":659156,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::h60671eb717a34c70","symbolLocation":104,"imageIndex":0},{"imageOffset":725700,"symbol":"tokio::runtime::context::scoped::Scoped$LT$T$GT$::set::h263d3206a13c783c","symbolLocation":148,"imageIndex":0},{"imageOffset":629280,"symbol":"tokio::runtime::context::set_scheduler::_$u7b$$u7b$closure$u7d$$u7d$::h74a8e23df56c9882","symbolLocation":40,"imageIndex":0},{"imageOffset":754924,"symbol":"std::thread::local::LocalKey$LT$T$GT$::try*with::h2f963d543150c87c","symbolLocation":196,"imageIndex":0},{"imageOffset":753488,"symbol":"std::thread::local::LocalKey$LT$T$GT$::with::hbd17f06bbba52edc","symbolLocation":24,"imageIndex":0},{"imageOffset":629228,"symbol":"tokio::runtime::context::set_scheduler::heb413530e9784dfc","symbolLocation":68,"imageIndex":0},{"imageOffset":658936,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::h79a1aa630e49b68a","symbolLocation":248,"imageIndex":0},{"imageOffset":560916,"symbol":"tokio::runtime::context::runtime::enter*runtime::hb50775e529a9a617","symbolLocation":188,"imageIndex":0},{"imageOffset":658520,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::h79855d48ee1e3c6a","symbolLocation":600,"imageIndex":0},{"imageOffset":657908,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Launch::launch::*$u7b$$u7b$closure$u7d$$u7d$::he167c16b6e6899e4","symbolLocation":24,"imageIndex":0},{"imageOffset":804156,"symbol":"_$LT$tokio..runtime..blocking..task..BlockingTask$LT$T$GT$$u20$as$u20$core..future..future..Future$GT$::poll::h8b9db1efbc4877eb","symbolLocation":136,"imageIndex":0},{"imageOffset":405416,"symbol":"tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::_$u7b$$u7b$closure$u7d$$u7d$::h904262c7dc2f88b0","symbolLocation":192,"imageIndex":0},{"imageOffset":404756,"symbol":"tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::h973d3c0095d258c4","symbolLocation":72,"imageIndex":0},{"imageOffset":401112,"symbol":"tokio::runtime::task::harness::poll*future::*$u7b$$u7b$closure$u7d$$u7d$::h8fa5ae0829102149","symbolLocation":64,"imageIndex":0},{"imageOffset":536564,"symbol":"_$LT$core..panic..unwind_safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2974776e7861df5d","symbolLocation":44,"imageIndex":0},{"imageOffset":483764,"symbol":"std::panicking::catch_unwind::do_call::h792e9a0912086d16","symbolLocation":72,"imageIndex":0},{"imageOffset":482892,"symbol":"\_\_rust_try","symbolLocation":32,"imageIndex":0},{"imageOffset":471312,"symbol":"std::panic::catch_unwind::h4474725978f361f4","symbolLocation":96,"imageIndex":0},{"imageOffset":400496,"symbol":"tokio::runtime::task::harness::poll_future::hcf462599407f235c","symbolLocation":96,"imageIndex":0},{"imageOffset":395688,"symbol":"tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll_inner::h5e6f168e02291d20","symbolLocation":160,"imageIndex":0},{"imageOffset":395384,"symbol":"tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll::h8a00a85251f71f0c","symbolLocation":28,"imageIndex":0},{"imageOffset":499856,"symbol":"tokio::runtime::task::raw::poll::hf96de62ba33ad657","symbolLocation":36,"imageIndex":0},{"imageOffset":499120,"symbol":"tokio::runtime::task::raw::RawTask::poll::hde83a8d818768542","symbolLocation":52,"imageIndex":0},{"imageOffset":532168,"symbol":"tokio::runtime::task::UnownedTask$LT$S$GT$::run::h7ce7c13c00052430","symbolLocation":64,"imageIndex":0},{"imageOffset":569720,"symbol":"tokio::runtime::blocking::pool::Task::run::hf56d840fd68c7fdc","symbolLocation":28,"imageIndex":0},{"imageOffset":576944,"symbol":"tokio::runtime::blocking::pool::Inner::run::hd581e97506257439","symbolLocation":512,"imageIndex":0},{"imageOffset":576284,"symbol":"tokio::runtime::blocking::pool::Spawner::spawn_thread::_$u7b$$u7b$closure$u7d$$u7d$::h2a632d4852cdd40e","symbolLocation":144,"imageIndex":0},{"imageOffset":782604,"symbol":"std::sys::backtrace::**rust*begin_short_backtrace::h51232d61e41dc60c","symbolLocation":16,"imageIndex":0},{"imageOffset":542208,"symbol":"std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::hbf84c81d58c2715d","symbolLocation":116,"imageIndex":0},{"imageOffset":536620,"symbol":"\_$LT$core..panic..unwind_safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2c0e201eb4dddbb7","symbolLocation":44,"imageIndex":0},{"imageOffset":483856,"symbol":"std::panicking::catch_unwind::do_call::h94ffcffc7cdce087","symbolLocation":68,"imageIndex":0},{"imageOffset":562020,"symbol":"**rust*try","symbolLocation":32,"imageIndex":0},{"imageOffset":541012,"symbol":"std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::haea346fd331ce539","symbolLocation":728,"imageIndex":0},{"imageOffset":411316,"symbol":"core::ops::function::FnOnce::call_once$u7b$$u7b$vtable.shim$u7d$$u7d$::h12a95acb393be819","symbolLocation":24,"imageIndex":0},{"imageOffset":1492320,"symbol":"std::sys::pal::unix::thread::Thread::new::thread_start::h636d41ad3c63ac40","symbolLocation":56,"imageIndex":0},{"imageOffset":27660,"symbol":"\_pthread_start","symbolLocation":136,"imageIndex":12},{"imageOffset":7040,"symbol":"thread_start","symbolLocation":8,"imageIndex":12}]},{"id":65463680,"name":"tokio-runtime-worker","threadState":{"x":[{"value":260},{"value":0},{"value":512},{"value":0},{"value":0},{"value":160},{"value":0},{"value":0},{"value":6181571752},{"value":0},{"value":0},{"value":2},{"value":2},{"value":0},{"value":0},{"value":0},{"value":305},{"value":8326616336},{"value":0},{"value":5232503832},{"value":5232503896},{"value":6181581024},{"value":0},{"value":0},{"value":512},{"value":513},{"value":768},{"value":0},{"value":0}],"flavor":"ARM_THREAD_STATE64","lr":{"value":6464438496},"cpsr":{"value":1610616832},"fp":{"value":6181571872},"sp":{"value":6181571728},"esr":{"value":1442840704,"description":" Address size fault"},"pc":{"value":6464181196},"far":{"value":0}},"frames":[{"imageOffset":17356,"symbol":"\_\_psynch_cvwait","symbolLocation":8,"imageIndex":3},{"imageOffset":28896,"symbol":"\_pthread_cond_wait","symbolLocation":984,"imageIndex":12},{"imageOffset":1289468,"symbol":"_$LT$parking*lot_core..thread_parker..imp..ThreadParker$u20$as$u20$parking_lot_core..thread_parker..ThreadParkerT$GT$::park::h0e2d0998749974de","symbolLocation":256,"imageIndex":0},{"imageOffset":1257704,"symbol":"parking_lot_core::parking_lot::park::*$u7b$$u7b$closure$u7d$$u7d$::h0b4fc83101e8557f","symbolLocation":748,"imageIndex":0},{"imageOffset":1256464,"symbol":"parking*lot_core::parking_lot::park::hd04bbb424560662a","symbolLocation":296,"imageIndex":0},{"imageOffset":1286760,"symbol":"parking_lot::condvar::Condvar::wait_until_internal::h8640b2ba350582ed","symbolLocation":124,"imageIndex":0},{"imageOffset":537312,"symbol":"parking_lot::condvar::Condvar::wait::h6618c4600aa6d512","symbolLocation":68,"imageIndex":0},{"imageOffset":798236,"symbol":"tokio::loom::std::parking_lot::Condvar::wait::hc858434e42aaccb1","symbolLocation":36,"imageIndex":0},{"imageOffset":773892,"symbol":"tokio::runtime::scheduler::multi_thread::park::Inner::park_condvar::h2267f8a57982d137","symbolLocation":264,"imageIndex":0},{"imageOffset":773552,"symbol":"tokio::runtime::scheduler::multi_thread::park::Inner::park::h3c3dbc8c396a0894","symbolLocation":168,"imageIndex":0},{"imageOffset":772836,"symbol":"tokio::runtime::scheduler::multi_thread::park::Parker::park::h010fa0d10ce96e9b","symbolLocation":40,"imageIndex":0},{"imageOffset":667856,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::park_timeout::h37500a9fc2ffb756","symbolLocation":776,"imageIndex":0},{"imageOffset":666572,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::park::hd6c55373d20068c0","symbolLocation":968,"imageIndex":0},{"imageOffset":661128,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::run::h541c11fea84a38ec","symbolLocation":1784,"imageIndex":0},{"imageOffset":659156,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::h60671eb717a34c70","symbolLocation":104,"imageIndex":0},{"imageOffset":725700,"symbol":"tokio::runtime::context::scoped::Scoped$LT$T$GT$::set::h263d3206a13c783c","symbolLocation":148,"imageIndex":0},{"imageOffset":629280,"symbol":"tokio::runtime::context::set_scheduler::_$u7b$$u7b$closure$u7d$$u7d$::h74a8e23df56c9882","symbolLocation":40,"imageIndex":0},{"imageOffset":754924,"symbol":"std::thread::local::LocalKey$LT$T$GT$::try*with::h2f963d543150c87c","symbolLocation":196,"imageIndex":0},{"imageOffset":753488,"symbol":"std::thread::local::LocalKey$LT$T$GT$::with::hbd17f06bbba52edc","symbolLocation":24,"imageIndex":0},{"imageOffset":629228,"symbol":"tokio::runtime::context::set_scheduler::heb413530e9784dfc","symbolLocation":68,"imageIndex":0},{"imageOffset":658936,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::h79a1aa630e49b68a","symbolLocation":248,"imageIndex":0},{"imageOffset":560916,"symbol":"tokio::runtime::context::runtime::enter*runtime::hb50775e529a9a617","symbolLocation":188,"imageIndex":0},{"imageOffset":658520,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::h79855d48ee1e3c6a","symbolLocation":600,"imageIndex":0},{"imageOffset":657908,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Launch::launch::*$u7b$$u7b$closure$u7d$$u7d$::he167c16b6e6899e4","symbolLocation":24,"imageIndex":0},{"imageOffset":804156,"symbol":"_$LT$tokio..runtime..blocking..task..BlockingTask$LT$T$GT$$u20$as$u20$core..future..future..Future$GT$::poll::h8b9db1efbc4877eb","symbolLocation":136,"imageIndex":0},{"imageOffset":405416,"symbol":"tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::_$u7b$$u7b$closure$u7d$$u7d$::h904262c7dc2f88b0","symbolLocation":192,"imageIndex":0},{"imageOffset":404756,"symbol":"tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::h973d3c0095d258c4","symbolLocation":72,"imageIndex":0},{"imageOffset":401112,"symbol":"tokio::runtime::task::harness::poll*future::*$u7b$$u7b$closure$u7d$$u7d$::h8fa5ae0829102149","symbolLocation":64,"imageIndex":0},{"imageOffset":536564,"symbol":"_$LT$core..panic..unwind_safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2974776e7861df5d","symbolLocation":44,"imageIndex":0},{"imageOffset":483764,"symbol":"std::panicking::catch_unwind::do_call::h792e9a0912086d16","symbolLocation":72,"imageIndex":0},{"imageOffset":482892,"symbol":"\_\_rust_try","symbolLocation":32,"imageIndex":0},{"imageOffset":471312,"symbol":"std::panic::catch_unwind::h4474725978f361f4","symbolLocation":96,"imageIndex":0},{"imageOffset":400496,"symbol":"tokio::runtime::task::harness::poll_future::hcf462599407f235c","symbolLocation":96,"imageIndex":0},{"imageOffset":395688,"symbol":"tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll_inner::h5e6f168e02291d20","symbolLocation":160,"imageIndex":0},{"imageOffset":395384,"symbol":"tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll::h8a00a85251f71f0c","symbolLocation":28,"imageIndex":0},{"imageOffset":499856,"symbol":"tokio::runtime::task::raw::poll::hf96de62ba33ad657","symbolLocation":36,"imageIndex":0},{"imageOffset":499120,"symbol":"tokio::runtime::task::raw::RawTask::poll::hde83a8d818768542","symbolLocation":52,"imageIndex":0},{"imageOffset":532168,"symbol":"tokio::runtime::task::UnownedTask$LT$S$GT$::run::h7ce7c13c00052430","symbolLocation":64,"imageIndex":0},{"imageOffset":569720,"symbol":"tokio::runtime::blocking::pool::Task::run::hf56d840fd68c7fdc","symbolLocation":28,"imageIndex":0},{"imageOffset":576944,"symbol":"tokio::runtime::blocking::pool::Inner::run::hd581e97506257439","symbolLocation":512,"imageIndex":0},{"imageOffset":576284,"symbol":"tokio::runtime::blocking::pool::Spawner::spawn_thread::_$u7b$$u7b$closure$u7d$$u7d$::h2a632d4852cdd40e","symbolLocation":144,"imageIndex":0},{"imageOffset":782604,"symbol":"std::sys::backtrace::**rust*begin_short_backtrace::h51232d61e41dc60c","symbolLocation":16,"imageIndex":0},{"imageOffset":542208,"symbol":"std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::hbf84c81d58c2715d","symbolLocation":116,"imageIndex":0},{"imageOffset":536620,"symbol":"\_$LT$core..panic..unwind_safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2c0e201eb4dddbb7","symbolLocation":44,"imageIndex":0},{"imageOffset":483856,"symbol":"std::panicking::catch_unwind::do_call::h94ffcffc7cdce087","symbolLocation":68,"imageIndex":0},{"imageOffset":562020,"symbol":"**rust*try","symbolLocation":32,"imageIndex":0},{"imageOffset":541012,"symbol":"std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::haea346fd331ce539","symbolLocation":728,"imageIndex":0},{"imageOffset":411316,"symbol":"core::ops::function::FnOnce::call_once$u7b$$u7b$vtable.shim$u7d$$u7d$::h12a95acb393be819","symbolLocation":24,"imageIndex":0},{"imageOffset":1492320,"symbol":"std::sys::pal::unix::thread::Thread::new::thread_start::h636d41ad3c63ac40","symbolLocation":56,"imageIndex":0},{"imageOffset":27660,"symbol":"\_pthread_start","symbolLocation":136,"imageIndex":12},{"imageOffset":7040,"symbol":"thread_start","symbolLocation":8,"imageIndex":12}]},{"id":65463681,"name":"tokio-runtime-worker","threadState":{"x":[{"value":260},{"value":0},{"value":256},{"value":0},{"value":0},{"value":160},{"value":0},{"value":0},{"value":6183718056},{"value":0},{"value":0},{"value":2},{"value":2},{"value":0},{"value":0},{"value":0},{"value":305},{"value":8326616336},{"value":0},{"value":5232504536},{"value":5232504600},{"value":6183727328},{"value":0},{"value":0},{"value":256},{"value":257},{"value":512},{"value":0},{"value":0}],"flavor":"ARM_THREAD_STATE64","lr":{"value":6464438496},"cpsr":{"value":1610616832},"fp":{"value":6183718176},"sp":{"value":6183718032},"esr":{"value":1442840704,"description":" Address size fault"},"pc":{"value":6464181196},"far":{"value":0}},"frames":[{"imageOffset":17356,"symbol":"\_\_psynch_cvwait","symbolLocation":8,"imageIndex":3},{"imageOffset":28896,"symbol":"\_pthread_cond_wait","symbolLocation":984,"imageIndex":12},{"imageOffset":1289468,"symbol":"_$LT$parking*lot_core..thread_parker..imp..ThreadParker$u20$as$u20$parking_lot_core..thread_parker..ThreadParkerT$GT$::park::h0e2d0998749974de","symbolLocation":256,"imageIndex":0},{"imageOffset":1257704,"symbol":"parking_lot_core::parking_lot::park::*$u7b$$u7b$closure$u7d$$u7d$::h0b4fc83101e8557f","symbolLocation":748,"imageIndex":0},{"imageOffset":1256464,"symbol":"parking*lot_core::parking_lot::park::hd04bbb424560662a","symbolLocation":296,"imageIndex":0},{"imageOffset":1286760,"symbol":"parking_lot::condvar::Condvar::wait_until_internal::h8640b2ba350582ed","symbolLocation":124,"imageIndex":0},{"imageOffset":537312,"symbol":"parking_lot::condvar::Condvar::wait::h6618c4600aa6d512","symbolLocation":68,"imageIndex":0},{"imageOffset":798236,"symbol":"tokio::loom::std::parking_lot::Condvar::wait::hc858434e42aaccb1","symbolLocation":36,"imageIndex":0},{"imageOffset":773892,"symbol":"tokio::runtime::scheduler::multi_thread::park::Inner::park_condvar::h2267f8a57982d137","symbolLocation":264,"imageIndex":0},{"imageOffset":773552,"symbol":"tokio::runtime::scheduler::multi_thread::park::Inner::park::h3c3dbc8c396a0894","symbolLocation":168,"imageIndex":0},{"imageOffset":772836,"symbol":"tokio::runtime::scheduler::multi_thread::park::Parker::park::h010fa0d10ce96e9b","symbolLocation":40,"imageIndex":0},{"imageOffset":667856,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::park_timeout::h37500a9fc2ffb756","symbolLocation":776,"imageIndex":0},{"imageOffset":666572,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::park::hd6c55373d20068c0","symbolLocation":968,"imageIndex":0},{"imageOffset":661128,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Context::run::h541c11fea84a38ec","symbolLocation":1784,"imageIndex":0},{"imageOffset":659156,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::h60671eb717a34c70","symbolLocation":104,"imageIndex":0},{"imageOffset":725700,"symbol":"tokio::runtime::context::scoped::Scoped$LT$T$GT$::set::h263d3206a13c783c","symbolLocation":148,"imageIndex":0},{"imageOffset":629280,"symbol":"tokio::runtime::context::set_scheduler::_$u7b$$u7b$closure$u7d$$u7d$::h74a8e23df56c9882","symbolLocation":40,"imageIndex":0},{"imageOffset":754924,"symbol":"std::thread::local::LocalKey$LT$T$GT$::try*with::h2f963d543150c87c","symbolLocation":196,"imageIndex":0},{"imageOffset":753488,"symbol":"std::thread::local::LocalKey$LT$T$GT$::with::hbd17f06bbba52edc","symbolLocation":24,"imageIndex":0},{"imageOffset":629228,"symbol":"tokio::runtime::context::set_scheduler::heb413530e9784dfc","symbolLocation":68,"imageIndex":0},{"imageOffset":658936,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::*$u7b$$u7b$closure$u7d$$u7d$::h79a1aa630e49b68a","symbolLocation":248,"imageIndex":0},{"imageOffset":560916,"symbol":"tokio::runtime::context::runtime::enter*runtime::hb50775e529a9a617","symbolLocation":188,"imageIndex":0},{"imageOffset":658520,"symbol":"tokio::runtime::scheduler::multi_thread::worker::run::h79855d48ee1e3c6a","symbolLocation":600,"imageIndex":0},{"imageOffset":657908,"symbol":"tokio::runtime::scheduler::multi_thread::worker::Launch::launch::*$u7b$$u7b$closure$u7d$$u7d$::he167c16b6e6899e4","symbolLocation":24,"imageIndex":0},{"imageOffset":804156,"symbol":"_$LT$tokio..runtime..blocking..task..BlockingTask$LT$T$GT$$u20$as$u20$core..future..future..Future$GT$::poll::h8b9db1efbc4877eb","symbolLocation":136,"imageIndex":0},{"imageOffset":405416,"symbol":"tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::_$u7b$$u7b$closure$u7d$$u7d$::h904262c7dc2f88b0","symbolLocation":192,"imageIndex":0},{"imageOffset":404756,"symbol":"tokio::runtime::task::core::Core$LT$T$C$S$GT$::poll::h973d3c0095d258c4","symbolLocation":72,"imageIndex":0},{"imageOffset":401112,"symbol":"tokio::runtime::task::harness::poll*future::*$u7b$$u7b$closure$u7d$$u7d$::h8fa5ae0829102149","symbolLocation":64,"imageIndex":0},{"imageOffset":536564,"symbol":"_$LT$core..panic..unwind_safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2974776e7861df5d","symbolLocation":44,"imageIndex":0},{"imageOffset":483764,"symbol":"std::panicking::catch_unwind::do_call::h792e9a0912086d16","symbolLocation":72,"imageIndex":0},{"imageOffset":482892,"symbol":"\_\_rust_try","symbolLocation":32,"imageIndex":0},{"imageOffset":471312,"symbol":"std::panic::catch_unwind::h4474725978f361f4","symbolLocation":96,"imageIndex":0},{"imageOffset":400496,"symbol":"tokio::runtime::task::harness::poll_future::hcf462599407f235c","symbolLocation":96,"imageIndex":0},{"imageOffset":395688,"symbol":"tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll_inner::h5e6f168e02291d20","symbolLocation":160,"imageIndex":0},{"imageOffset":395384,"symbol":"tokio::runtime::task::harness::Harness$LT$T$C$S$GT$::poll::h8a00a85251f71f0c","symbolLocation":28,"imageIndex":0},{"imageOffset":499856,"symbol":"tokio::runtime::task::raw::poll::hf96de62ba33ad657","symbolLocation":36,"imageIndex":0},{"imageOffset":499120,"symbol":"tokio::runtime::task::raw::RawTask::poll::hde83a8d818768542","symbolLocation":52,"imageIndex":0},{"imageOffset":532168,"symbol":"tokio::runtime::task::UnownedTask$LT$S$GT$::run::h7ce7c13c00052430","symbolLocation":64,"imageIndex":0},{"imageOffset":569720,"symbol":"tokio::runtime::blocking::pool::Task::run::hf56d840fd68c7fdc","symbolLocation":28,"imageIndex":0},{"imageOffset":576944,"symbol":"tokio::runtime::blocking::pool::Inner::run::hd581e97506257439","symbolLocation":512,"imageIndex":0},{"imageOffset":576284,"symbol":"tokio::runtime::blocking::pool::Spawner::spawn_thread::_$u7b$$u7b$closure$u7d$$u7d$::h2a632d4852cdd40e","symbolLocation":144,"imageIndex":0},{"imageOffset":782604,"symbol":"std::sys::backtrace::**rust*begin_short_backtrace::h51232d61e41dc60c","symbolLocation":16,"imageIndex":0},{"imageOffset":542208,"symbol":"std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::hbf84c81d58c2715d","symbolLocation":116,"imageIndex":0},{"imageOffset":536620,"symbol":"\_$LT$core..panic..unwind_safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::h2c0e201eb4dddbb7","symbolLocation":44,"imageIndex":0},{"imageOffset":483856,"symbol":"std::panicking::catch_unwind::do_call::h94ffcffc7cdce087","symbolLocation":68,"imageIndex":0},{"imageOffset":562020,"symbol":"**rust*try","symbolLocation":32,"imageIndex":0},{"imageOffset":541012,"symbol":"std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::haea346fd331ce539","symbolLocation":728,"imageIndex":0},{"imageOffset":411316,"symbol":"core::ops::function::FnOnce::call_once$u7b$$u7b$vtable.shim$u7d$$u7d$::h12a95acb393be819","symbolLocation":24,"imageIndex":0},{"imageOffset":1492320,"symbol":"std::sys::pal::unix::thread::Thread::new::thread_start::h636d41ad3c63ac40","symbolLocation":56,"imageIndex":0},{"imageOffset":27660,"symbol":"\_pthread_start","symbolLocation":136,"imageIndex":12},{"imageOffset":7040,"symbol":"thread_start","symbolLocation":8,"imageIndex":12}]},{"id":65463682,"frames":[{"imageOffset":7020,"symbol":"start_wqthread","symbolLocation":0,"imageIndex":12}],"threadState":{"x":[{"value":6184300544},{"value":9219},{"value":6183763968},{"value":0},{"value":409604},{"value":18446744073709551615},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0}],"flavor":"ARM_THREAD_STATE64","lr":{"value":0},"cpsr":{"value":4096},"fp":{"value":0},"sp":{"value":6184300544},"esr":{"value":1442840704,"description":" Address size fault"},"pc":{"value":6464416620},"far":{"value":0}}},{"id":65463683,"threadState":{"x":[{"value":6184865454},{"value":8306878672,"symbolLocation":10208,"symbol":"lsl::sPoolBytes"},{"value":0},{"value":6184865640},{"value":3},{"value":0},{"value":6184871424},{"value":6184871352},{"value":9},{"value":32767},{"value":6461172080,"symbolLocation":0,"symbol":"mach_o::LinkedDylibAttributes::regular"},{"value":1},{"value":6184871234},{"value":0},{"value":8614050938},{"value":32768},{"value":10791539664},{"value":8326729760},{"value":0},{"value":8306878672,"symbolLocation":10208,"symbol":"lsl::sPoolBytes"},{"value":11020578824},{"value":6184871512},{"value":11020578824},{"value":6184871424},{"value":6184871232},{"value":6184871352},{"value":3},{"value":10},{"value":0}],"flavor":"ARM_THREAD_STATE64","lr":{"value":6460788612},"cpsr":{"value":2147487744},"fp":{"value":6184865424},"sp":{"value":6184865424},"esr":{"value":1442840704,"description":" Address size fault"},"pc":{"value":6460728016},"far":{"value":0}},"queue":"com.apple.dock.fullscreen","frames":[{"imageOffset":123600,"symbol":"dyld4::Loader::LoaderRef::loader(dyld4::RuntimeState const&) const","symbolLocation":12,"imageIndex":10},{"imageOffset":184196,"symbol":"dyld4::PrebuiltLoader::dependent(dyld4::RuntimeState const&, unsigned int, mach_o::LinkedDylibAttributes*) const","symbolLocation":128,"imageIndex":10},{"imageOffset":151216,"symbol":"dyld4::Loader::hasExportedSymbol(Diagnostics&, dyld4::RuntimeState&, char const*, dyld4::Loader::ExportedSymbolMode, dyld4::Loader::ResolverMode, dyld4::Loader::ResolvedSymbol*, dyld3::Array<dyld4::Loader const*>*) const","symbolLocation":1280,"imageIndex":10},{"imageOffset":151272,"symbol":"dyld4::Loader::hasExportedSymbol(Diagnostics&, dyld4::RuntimeState&, char const*, dyld4::Loader::ExportedSymbolMode, dyld4::Loader::ResolverMode, dyld4::Loader::ResolvedSymbol*, dyld3::Array<dyld4::Loader const*>*) const","symbolLocation":1336,"imageIndex":10},{"imageOffset":151272,"symbol":"dyld4::Loader::hasExportedSymbol(Diagnostics&, dyld4::RuntimeState&, char const*, dyld4::Loader::ExportedSymbolMode, dyld4::Loader::ResolverMode, dyld4::Loader::ResolvedSymbol*, dyld3::Array<dyld4::Loader const*>*) const","symbolLocation":1336,"imageIndex":10},{"imageOffset":151272,"symbol":"dyld4::Loader::hasExportedSymbol(Diagnostics&, dyld4::RuntimeState&, char const*, dyld4::Loader::ExportedSymbolMode, dyld4::Loader::ResolverMode, dyld4::Loader::ResolvedSymbol*, dyld3::Array<dyld4::Loader const*>*) const","symbolLocation":1336,"imageIndex":10},{"imageOffset":151272,"symbol":"dyld4::Loader::hasExportedSymbol(Diagnostics&, dyld4::RuntimeState&, char const*, dyld4::Loader::ExportedSymbolMode, dyld4::Loader::ResolverMode, dyld4::Loader::ResolvedSymbol*, dyld3::Array<dyld4::Loader const*>*) const","symbolLocation":1336,"imageIndex":10},{"imageOffset":151272,"symbol":"dyld4::Loader::hasExportedSymbol(Diagnostics&, dyld4::RuntimeState&, char const*, dyld4::Loader::ExportedSymbolMode, dyld4::Loader::ResolverMode, dyld4::Loader::ResolvedSymbol*, dyld3::Array<dyld4::Loader const*>*) const","symbolLocation":1336,"imageIndex":10},{"imageOffset":249004,"symbol":"dyld4::APIs::dlsym(void*, char const\*)","symbolLocation":1464,"imageIndex":10},{"imageOffset":3795568,"symbol":"**loginframework_vtable_block_invoke","symbolLocation":564,"imageIndex":13},{"imageOffset":112732,"symbol":"\_dispatch_client_callout","symbolLocation":16,"imageIndex":4},{"imageOffset":18984,"symbol":"\_dispatch_once_callout","symbolLocation":32,"imageIndex":4},{"imageOffset":3802040,"symbol":"SLSCopyCurrentSessionPropertiesInternalBridge","symbolLocation":384,"imageIndex":13},{"imageOffset":1513804,"symbol":"SLSessionCopyCurrentDictionary","symbolLocation":20,"imageIndex":13},{"imageOffset":1238196,"symbol":"-[NSDockConnection _makeConnectionIfNeededWithRetryInterval:onDemand:]","symbolLocation":64,"imageIndex":9},{"imageOffset":12661136,"symbol":"**35-[NSDockConnection startConnection]\_block_invoke.13","symbolLocation":44,"imageIndex":9},{"imageOffset":6956,"symbol":"\_dispatch_call_block_and_release","symbolLocation":32,"imageIndex":4},{"imageOffset":112732,"symbol":"\_dispatch_client_callout","symbolLocation":16,"imageIndex":4},{"imageOffset":41808,"symbol":"\_dispatch_lane_serial_drain","symbolLocation":740,"imageIndex":4},{"imageOffset":44588,"symbol":"\_dispatch_lane_invoke","symbolLocation":388,"imageIndex":4},{"imageOffset":86628,"symbol":"\_dispatch_root_queue_drain_deferred_wlh","symbolLocation":292,"imageIndex":4},{"imageOffset":84712,"symbol":"\_dispatch_workloop_worker_thread","symbolLocation":540,"imageIndex":4},{"imageOffset":11876,"symbol":"\_pthread_wqthread","symbolLocation":292,"imageIndex":12},{"imageOffset":7028,"symbol":"start_wqthread","symbolLocation":8,"imageIndex":12}]},{"id":65463708,"frames":[{"imageOffset":7020,"symbol":"start_wqthread","symbolLocation":0,"imageIndex":12}],"threadState":{"x":[{"value":6185447424},{"value":0},{"value":6184910848},{"value":0},{"value":278532},{"value":18446744073709551615},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0}],"flavor":"ARM_THREAD_STATE64","lr":{"value":0},"cpsr":{"value":4096},"fp":{"value":0},"sp":{"value":6185447424},"esr":{"value":0,"description":" Address size fault"},"pc":{"value":6464416620},"far":{"value":0}}},{"id":65463717,"frames":[{"imageOffset":2992,"symbol":"semaphore_wait_trap","symbolLocation":8,"imageIndex":3},{"imageOffset":14688,"symbol":"\_dispatch_sema4_wait","symbolLocation":28,"imageIndex":4},{"imageOffset":16144,"symbol":"\_dispatch_semaphore_wait_slow","symbolLocation":132,"imageIndex":4},{"imageOffset":1482520,"symbol":"std::thread::park::h7819486c310a61ee","symbolLocation":96,"imageIndex":0},{"imageOffset":236716,"symbol":"crossbeam_channel::context::Context::wait_until::hfe8d919652164e11","symbolLocation":208,"imageIndex":0},{"imageOffset":299308,"symbol":"crossbeam_channel::flavors::array::Channel$LT$T$GT$::recv::_$u7b$$u7b$closure$u7d$$u7d$::h100e5fadd7ac726e","symbolLocation":152,"imageIndex":0},{"imageOffset":241880,"symbol":"crossbeam*channel::context::Context::with::*$u7b$$u7b$closure$u7d$$u7d$::h800801b973582feb","symbolLocation":100,"imageIndex":0},{"imageOffset":240936,"symbol":"crossbeam*channel::context::Context::with::*$u7b$$u7b$closure$u7d$$u7d$::h32d36c01f3997fab","symbolLocation":232,"imageIndex":0},{"imageOffset":301164,"symbol":"std::thread::local::LocalKey$LT$T$GT$::try*with::h6aa4f6c96a05adf1","symbolLocation":172,"imageIndex":0},{"imageOffset":240228,"symbol":"crossbeam_channel::context::Context::with::h30d2c8f0eb1cc28b","symbolLocation":52,"imageIndex":0},{"imageOffset":299116,"symbol":"crossbeam_channel::flavors::array::Channel$LT$T$GT$::recv::hbb39635dcd84a0c0","symbolLocation":280,"imageIndex":0},{"imageOffset":356568,"symbol":"crossbeam_channel::channel::Receiver$LT$T$GT$::recv::h07b397b671ed012a","symbolLocation":160,"imageIndex":0},{"imageOffset":355968,"symbol":"*$LT$crossbeam*channel..channel..IntoIter$LT$T$GT$$u20$as$u20$core..iter..traits..iterator..Iterator$GT$::next::h50472e68cf682da0","symbolLocation":36,"imageIndex":0},{"imageOffset":230628,"symbol":"streamlib_core::runtime::StreamRuntime::spawn_handler_threads::*$u7b$$u7b$closure$u7d$$u7d$::hd0c273a3f900621f","symbolLocation":2448,"imageIndex":0},{"imageOffset":339556,"symbol":"std::sys::backtrace::**rust*begin_short_backtrace::hbe487fc5f40b97b3","symbolLocation":16,"imageIndex":0},{"imageOffset":213204,"symbol":"std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::h36a738c24e545fe9","symbolLocation":120,"imageIndex":0},{"imageOffset":277724,"symbol":"\_$LT$core..panic..unwind_safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::hf3b8dc80fdc96ede","symbolLocation":44,"imageIndex":0},{"imageOffset":313552,"symbol":"std::panicking::catch_unwind::do_call::h6c915fdd9e40d569","symbolLocation":68,"imageIndex":0},{"imageOffset":213796,"symbol":"**rust*try","symbolLocation":32,"imageIndex":0},{"imageOffset":212764,"symbol":"std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::heeb9bfa0dbf47564","symbolLocation":748,"imageIndex":0},{"imageOffset":256064,"symbol":"core::ops::function::FnOnce::call_once$u7b$$u7b$vtable.shim$u7d$$u7d$::h308f6798985fc9cf","symbolLocation":24,"imageIndex":0},{"imageOffset":1492320,"symbol":"std::sys::pal::unix::thread::Thread::new::thread_start::h636d41ad3c63ac40","symbolLocation":56,"imageIndex":0},{"imageOffset":27660,"symbol":"\_pthread_start","symbolLocation":136,"imageIndex":12},{"imageOffset":7040,"symbol":"thread_start","symbolLocation":8,"imageIndex":12}],"threadState":{"x":[{"value":14},{"value":8589934595},{"value":171798697235},{"value":183631326740995},{"value":14680198217728},{"value":183631326740480},{"value":48},{"value":0},{"value":0},{"value":1},{"value":0},{"value":2},{"value":0},{"value":546448548},{"value":546448548},{"value":546448548},{"value":18446744073709551580},{"value":8326620648},{"value":0},{"value":5232808640},{"value":5232808576},{"value":18446744073709551615},{"value":4307087368,"symbolLocation":7224,"symbol":"_$LT$streamlib*apple..processors..camera..AppleCameraProcessor$u20$as$u20$streamlib_core..stream_processor..StreamProcessor$GT$::on_stop::**CALLSITE::META::h9763428936fa9b2c"},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0},{"value":0}],"flavor":"ARM_THREAD_STATE64","lr":{"value":6462642528},"cpsr":{"value":1610616832},"fp":{"value":6187586976},"sp":{"value":6187586960},"esr":{"value":1442840704,"description":" Address size fault"},"pc":{"value":6464166832},"far":{"value":0}}},{"triggered":true,"id":65463718,"threadState":{"x":[{"value":6548743315,"symbolLocation":122191,"symbol":".str.533"},{"value":0},{"value":6460837440,"symbolLocation":0,"symbol":"dyld4::APIs::dyld_program_sdk_at_least(dyld_build_version_t)"},{"value":8326741048,"symbolLocation":584,"symbol":"vtable for dyld4::APIs"},{"value":6469311951,"symbolLocation":519551,"symbol":"\_XMLPlistAppendDataUsingBase64.**CFPLDataEncodeTable"},{"value":6548743416,"symbolLocation":122292,"symbol":".str.533"},{"value":0},{"value":0},{"value":8269598720,"symbolLocation":256,"symbol":"get_asl.utx"},{"value":2},{"value":36028797018963960},{"value":0},{"value":5},{"value":5234633216},{"value":8309589776,"symbolLocation":0,"symbol":"**CFConstantStringClassReference"},{"value":8309589776,"symbolLocation":0,"symbol":"**CFConstantStringClassReference"},{"value":6463419880,"symbolLocation":0,"symbol":"\_os_crash"},{"value":8326609688},{"value":0},{"value":6189731728},{"value":5233588304},{"value":5232682112},{"value":10845153624},{"value":0},{"value":10845153216},{"value":1},{"value":10652078080},{"value":1},{"value":1}],"flavor":"ARM_THREAD_STATE64","lr":{"value":6545358016},"cpsr":{"value":1073745920},"fp":{"value":6189731712},"sp":{"value":6189731520},"esr":{"value":4060086273,"description":"(Breakpoint) brk 1"},"pc":{"value":6545358016,"matchesCrashFrame":1},"far":{"value":0}},"frames":[{"imageOffset":13995200,"symbol":"-[NSWMWindowCoordinator performTransactionUsingBlock:]","symbolLocation":752,"imageIndex":9},{"imageOffset":12089856,"symbol":"-[NSWindow(NSWMWindowManagement) window:didUpdateWithChangedProperties:]","symbolLocation":104,"imageIndex":9},{"imageOffset":34592,"symbol":"-[_WMWindow performUpdatesUsingBlock:]","symbolLocation":100,"imageIndex":14},{"imageOffset":30484,"symbol":"-[_WMWindow applyTags:mask:]","symbolLocation":712,"imageIndex":14},{"imageOffset":31992,"symbol":"-[_WMWindow setVisible:]","symbolLocation":84,"imageIndex":14},{"imageOffset":1245284,"symbol":"-[NSWindow _setVisible:]","symbolLocation":268,"imageIndex":9},{"imageOffset":1244964,"symbol":"-[NSWindow _makeKeyRegardlessOfVisibility]","symbolLocation":40,"imageIndex":9},{"imageOffset":1214440,"symbol":"-[NSWindow makeKeyAndOrderFront:]","symbolLocation":24,"imageIndex":9},{"imageOffset":133292,"symbol":"*$LT$$LP$A$C$$RP$$u20$as$u20$objc2..encode..EncodeArguments$GT$::**invoke::hb60909703740adfa","symbolLocation":88,"imageIndex":0},{"imageOffset":136828,"symbol":"objc2::runtime::message*receiver::msg_send_primitive::send::h20a306aff6203842","symbolLocation":80,"imageIndex":0},{"imageOffset":132976,"symbol":"objc2::runtime::message_receiver::MessageReceiver::send_message::he8b25aabb8931882","symbolLocation":192,"imageIndex":0},{"imageOffset":112108,"symbol":"*$LT$MethodFamily$u20$as$u20$objc2..**macro*helpers..msg_send_retained..MsgSend$LT$Receiver$C$Return$GT$$GT$::send_message::he02307f0ece5f831","symbolLocation":180,"imageIndex":0},{"imageOffset":119308,"symbol":"objc2_app_kit::generated::\_\_NSWindow::NSWindow::makeKeyAndOrderFront::hacb345b4ac9a7f1e","symbolLocation":80,"imageIndex":0},{"imageOffset":81132,"symbol":"*$LT$streamlib*apple..processors..display..AppleDisplayProcessor$u20$as$u20$streamlib_core..stream_processor..StreamProcessor$GT$::on_start::hbfc20c3368bc77fe","symbolLocation":1080,"imageIndex":0},{"imageOffset":229020,"symbol":"streamlib_core::runtime::StreamRuntime::spawn_handler_threads::*$u7b$$u7b$closure$u7d$$u7d$::hd0c273a3f900621f","symbolLocation":840,"imageIndex":0},{"imageOffset":339556,"symbol":"std::sys::backtrace::**rust*begin_short_backtrace::hbe487fc5f40b97b3","symbolLocation":16,"imageIndex":0},{"imageOffset":213204,"symbol":"std::thread::Builder::spawn_unchecked*::_$u7b$$u7b$closure$u7d$$u7d$::_$u7b$$u7b$closure$u7d$$u7d$::h36a738c24e545fe9","symbolLocation":120,"imageIndex":0},{"imageOffset":277724,"symbol":"\_$LT$core..panic..unwind_safe..AssertUnwindSafe$LT$F$GT$$u20$as$u20$core..ops..function..FnOnce$LT$$LP$$RP$$GT$$GT$::call_once::hf3b8dc80fdc96ede","symbolLocation":44,"imageIndex":0},{"imageOffset":313552,"symbol":"std::panicking::catch_unwind::do_call::h6c915fdd9e40d569","symbolLocation":68,"imageIndex":0},{"imageOffset":213796,"symbol":"**rust*try","symbolLocation":32,"imageIndex":0},{"imageOffset":212764,"symbol":"std::thread::Builder::spawn_unchecked*::\_$u7b$$u7b$closure$u7d$$u7d$::heeb9bfa0dbf47564","symbolLocation":748,"imageIndex":0},{"imageOffset":256064,"symbol":"core::ops::function::FnOnce::call_once$u7b$$u7b$vtable.shim$u7d$$u7d$::h308f6798985fc9cf","symbolLocation":24,"imageIndex":0},{"imageOffset":1492320,"symbol":"std::sys::pal::unix::thread::Thread::new::thread_start::h636d41ad3c63ac40","symbolLocation":56,"imageIndex":0},{"imageOffset":27660,"symbol":"\_pthread_start","symbolLocation":136,"imageIndex":12},{"imageOffset":7040,"symbol":"thread_start","symbolLocation":8,"imageIndex":12}]}],
"usedImages" : [
{
"source" : "P",
"arch" : "arm64",
"base" : 4304601088,
"size" : 2473984,
"uuid" : "3256a5ab-b430-3345-82d3-9f5c54959345",
"path" : "*\/camera_display",
"name" : "camera_display"
},
{
"source" : "P",
"arch" : "arm64e",
"base" : 4422631424,
"CFBundleShortVersionString" : "329.2",
"CFBundleIdentifier" : "com.apple.AGXMetalG13X",
"size" : 6914048,
"uuid" : "6b497f3b-6583-398c-9d05-5f30a1c1bae5",
"path" : "\/System\/Library\/Extensions\/AGXMetalG13X.bundle\/Contents\/MacOS\/AGXMetalG13X",
"name" : "AGXMetalG13X",
"CFBundleVersion" : "329.2"
},
{
"source" : "P",
"arch" : "arm64e",
"base" : 4420337664,
"size" : 49152,
"uuid" : "a3faee04-0f8b-3428-9497-560c97eca6fb",
"path" : "\/usr\/lib\/libobjc-trampolines.dylib",
"name" : "libobjc-trampolines.dylib"
},
{
"source" : "P",
"arch" : "arm64e",
"base" : 6464163840,
"size" : 243284,
"uuid" : "6e4a96ad-04b8-3e8a-b91d-087e62306246",
"path" : "\/usr\/lib\/system\/libsystem_kernel.dylib",
"name" : "libsystem_kernel.dylib"
},
{
"source" : "P",
"arch" : "arm64e",
"base" : 6462627840,
"size" : 288608,
"uuid" : "24ce0d89-4114-30c2-a81a-3db1f5931cff",
"path" : "\/usr\/lib\/system\/libdispatch.dylib",
"name" : "libdispatch.dylib"
},
{
"source" : "P",
"arch" : "arm64e",
"base" : 6461259776,
"size" : 305408,
"uuid" : "1b8951eb-68db-37ba-9581-240b796aa872",
"path" : "\/usr\/lib\/system\/libxpc.dylib",
"name" : "libxpc.dylib"
},
{
"source" : "P",
"arch" : "arm64e",
"base" : 6470389760,
"CFBundleShortVersionString" : "1141.1",
"CFBundleIdentifier" : "com.apple.LaunchServices",
"size" : 3088704,
"uuid" : "1e58a116-8211-3368-8f5a-5df00d6f8971",
"path" : "\/System\/Library\/Frameworks\/CoreServices.framework\/Versions\/A\/Frameworks\/LaunchServices.framework\/Versions\/A\/LaunchServices",
"name" : "LaunchServices",
"CFBundleVersion" : "1141.1"
},
{
"source" : "P",
"arch" : "arm64e",
"base" : 6593249280,
"CFBundleShortVersionString" : "944",
"CFBundleIdentifier" : "com.apple.AE",
"size" : 472968,
"uuid" : "c410f3f4-79c0-3066-92bd-3b8ab89ade37",
"path" : "\/System\/Library\/Frameworks\/CoreServices.framework\/Versions\/A\/Frameworks\/AE.framework\/Versions\/A\/AE",
"name" : "AE",
"CFBundleVersion" : "944"
},
{
"source" : "P",
"arch" : "arm64e",
"base" : 6660292608,
"CFBundleShortVersionString" : "2.1.1",
"CFBundleIdentifier" : "com.apple.HIToolbox",
"size" : 3174368,
"uuid" : "1a037942-11e0-3fc8-aad2-20b11e7ae1a4",
"path" : "\/System\/Library\/Frameworks\/Carbon.framework\/Versions\/A\/Frameworks\/HIToolbox.framework\/Versions\/A\/HIToolbox",
"name" : "HIToolbox"
},
{
"source" : "P",
"arch" : "arm64e",
"base" : 6531362816,
"CFBundleShortVersionString" : "6.9",
"CFBundleIdentifier" : "com.apple.AppKit",
"size" : 21564992,
"uuid" : "860c164c-d04c-30ff-8c6f-e672b74caf11",
"path" : "\/System\/Library\/Frameworks\/AppKit.framework\/Versions\/C\/AppKit",
"name" : "AppKit",
"CFBundleVersion" : "2575.70.52"
},
{
"source" : "P",
"arch" : "arm64e",
"base" : 6460604416,
"size" : 636280,
"uuid" : "3247e185-ced2-36ff-9e29-47a77c23e004",
"path" : "\/usr\/lib\/dyld",
"name" : "dyld"
},
{
"size" : 0,
"source" : "A",
"base" : 0,
"uuid" : "00000000-0000-0000-0000-000000000000"
},
{
"source" : "P",
"arch" : "arm64e",
"base" : 6464409600,
"size" : 51784,
"uuid" : "d6494ba9-171e-39fc-b1aa-28ecf87975d1",
"path" : "\/usr\/lib\/system\/libsystem_pthread.dylib",
"name" : "libsystem_pthread.dylib"
},
{
"source" : "P",
"arch" : "arm64e",
"base" : 6565343232,
"CFBundleShortVersionString" : "1.600.0",
"CFBundleIdentifier" : "com.apple.SkyLight",
"size" : 5398624,
"uuid" : "4e052846-80c2-38af-85bf-1482e070a32b",
"path" : "\/System\/Library\/PrivateFrameworks\/SkyLight.framework\/Versions\/A\/SkyLight",
"name" : "SkyLight"
},
{
"source" : "P",
"arch" : "arm64e",
"base" : 10234355712,
"CFBundleIdentifier" : "com.apple.WindowManagement",
"size" : 126752,
"uuid" : "70ba6e1a-afb1-39d8-b8a8-8d6246ac6064",
"path" : "\/System\/Library\/PrivateFrameworks\/WindowManagement.framework\/Versions\/A\/WindowManagement",
"name" : "WindowManagement",
"CFBundleVersion" : "278.4.7"
},
{
"source" : "P",
"arch" : "arm64e",
"base" : 6464888832,
"CFBundleShortVersionString" : "6.9",
"CFBundleIdentifier" : "com.apple.CoreFoundation",
"size" : 5500928,
"uuid" : "8d45baee-6cc0-3b89-93fd-ea1c8e15c6d7",
"path" : "\/System\/Library\/Frameworks\/CoreFoundation.framework\/Versions\/A\/CoreFoundation",
"name" : "CoreFoundation",
"CFBundleVersion" : "3603"
},
{
"source" : "P",
"arch" : "arm64e",
"base" : 6462930944,
"size" : 528964,
"uuid" : "dfea8794-80ce-37c3-8f6a-108aa1d0b1b0",
"path" : "\/usr\/lib\/system\/libsystem_c.dylib",
"name" : "libsystem_c.dylib"
}
],
"sharedCache" : {
"base" : 6459768832,
"size" : 5040898048,
"uuid" : "4c1223e5-cace-3982-a003-6110a7a8a25c"
},
"legacyInfo" : {
"threadTriggered" : {

}
},
"logWritingSignature" : "e2d7f2356e5bf8dbeec5bec1aea5dfd1d04e7f3b",
"trialInfo" : {
"rollouts" : [
{
"rolloutId" : "670e9bd77a111748a97092a1",
"factorPackIds" : {
"SIRI_TTS_DEVICE_TRAINING" : "67d07fb744f1a3655d87002b"
},
"deploymentId" : 240000016
},
{
"rolloutId" : "64c025b28b7f0e739e4fbe58",
"factorPackIds" : {
"SIRI_UNDERSTANDING_CLASSIC_DEPRECATION" : "657ba0a39ec5da283662e9d2"
},
"deploymentId" : 240000041
}
],
"experiments" : [

]
}
}
