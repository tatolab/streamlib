use super::*;
use crate::python_surface_share_service_for_tests::SurfaceShareUnderTest;
use std::io::Write as _;

fn exchange_client_on(share: &SurfaceShareUnderTest) -> Arc<HelperProcessGpuExchangeClient> {
    Python::initialize();
    Python::attach(|python| {
        Arc::new(HelperProcessGpuExchangeClient::new(
            python.None(),
            python.None(),
            share.channel_name_for_the_helper(),
            "helper:iosurface-import-under-test".to_string(),
        ))
    })
}

fn a_vulkan_device_is_available() -> bool {
    match ConsumerVulkanDevice::new() {
        Ok(_) => true,
        Err(unavailable) => {
            let _ = writeln!(
                std::io::stdout(),
                "Skipping test — no consumer Vulkan device: {unavailable}"
            );
            false
        }
    }
}

fn check_out_pixel_surface(
    exchange_client: &Arc<HelperProcessGpuExchangeClient>,
    surface_id: &str,
) -> HelperCheckedOutPixelSurface {
    let HelperCheckedOutSurface::PixelBuffer(pixel_surface) = exchange_client
        .check_out_and_import(surface_id)
        .expect("the checkout and import");
    pixel_surface
}

/// A held frame claims its IOSurface's use count and its lease; letting
/// it go returns both, while the slot's import stays cached — and a
/// cached import alone does not pin the slot.
#[test]
fn a_held_frame_pins_its_slot_and_a_cached_slot_does_not() {
    if !a_vulkan_device_is_available() {
        return;
    }
    let share = SurfaceShareUnderTest::start("use-count");
    share.publish_pool_slot_frame("pool-slot-held", 1);
    let iosurface = share.iosurface_registered_as("pool-slot-held");
    let exchange_client = exchange_client_on(&share);

    let held_frame = check_out_pixel_surface(&exchange_client, "pool-slot-held#1");
    assert!(
        iosurface.is_in_use(),
        "a held frame must read in use to the pool"
    );
    assert_eq!(share.outstanding_claims_on("pool-slot-held"), 1);
    assert_eq!(
        held_frame.host_mapped_base_address(),
        iosurface.base_address().as_ptr().cast::<u8>(),
        "the helper's view must be the IOSurface's own memory"
    );

    drop(held_frame);
    assert_eq!(share.outstanding_claims_on("pool-slot-held"), 0);
    assert_eq!(
        exchange_client.iosurface_imports_by_pool_slot.lock().len(),
        1,
        "the slot's import stays cached for its next frame"
    );
    assert!(
        !iosurface.is_in_use(),
        "a cached import must not pin the slot while no frame is held"
    );
}

/// The slot's next frame reuses its import: no second lookup, no second
/// Vulkan import — the port it arrived with is released unlooked-up.
#[test]
fn a_later_frame_over_a_cached_slot_reuses_the_slots_import() {
    if !a_vulkan_device_is_available() {
        return;
    }
    let share = SurfaceShareUnderTest::start("slot-reuse");
    share.publish_pool_slot_frame("pool-slot-reused", 1);
    let iosurface = share.iosurface_registered_as("pool-slot-reused");
    let exchange_client = exchange_client_on(&share);

    let first_frame = check_out_pixel_surface(&exchange_client, "pool-slot-reused#1");
    let first_import = Arc::clone(&first_frame.iosurface_pool_slot_import);
    drop(first_frame);

    share.publish_pool_slot_frame("pool-slot-reused", 2);
    let second_frame = check_out_pixel_surface(&exchange_client, "pool-slot-reused#2");
    assert!(
        Arc::ptr_eq(&first_import, &second_frame.iosurface_pool_slot_import),
        "the second frame over the slot must reuse the slot's import"
    );
    drop(second_frame);
    drop(first_import);
    assert!(
        !iosurface.is_in_use(),
        "the unlooked-up port must have been released with the checkout"
    );
}

/// The engine going away empties the per-slot cache: an IOSurface this
/// helper still holds stays readable after the engine dies, and nothing
/// else would let it go. A view already handed out keeps its own share.
#[test]
fn the_service_going_away_releases_every_cached_slot() {
    if !a_vulkan_device_is_available() {
        return;
    }
    let share = SurfaceShareUnderTest::start("service-gone");
    share.publish_pool_slot_frame("pool-slot-orphaned", 1);
    let exchange_client = exchange_client_on(&share);
    let still_held_view = check_out_pixel_surface(&exchange_client, "pool-slot-orphaned#1");
    assert_eq!(
        exchange_client.iosurface_imports_by_pool_slot.lock().len(),
        1
    );

    drop(share);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !exchange_client
        .iosurface_imports_by_pool_slot
        .lock()
        .is_empty()
        && std::time::Instant::now() < deadline
    {
        std::thread::yield_now();
    }
    assert!(
        exchange_client
            .iosurface_imports_by_pool_slot
            .lock()
            .is_empty(),
        "the per-slot cache must empty once the service is gone"
    );
    assert!(
        !still_held_view.host_mapped_base_address().is_null(),
        "a view handed out before keeps its own share of the slot"
    );
}

/// Where the helper-process half of the kill test finds its service.
const SERVICE_NAME_FOR_THE_HELPER_UNDER_TEST: &str =
    "STREAMLIB_WHEEL_TEST_SURFACE_SHARE_MACH_SERVICE";
const HELPER_HOLDS_THE_FRAME_MARKER: &str = "HELPER_HOLDS_THE_FRAME";

/// Not a test on its own: the helper process
/// [`a_killed_helper_releases_the_frame_it_held`] runs this test binary as.
/// It checks a frame out, locks it for writing, says so, and waits to be
/// killed.
#[test]
#[ignore = "the helper half of a_killed_helper_releases_the_frame_it_held"]
fn helper_process_that_holds_a_frame_until_it_is_killed() {
    let Some(service_name) = std::env::var_os(SERVICE_NAME_FOR_THE_HELPER_UNDER_TEST) else {
        return;
    };
    Python::initialize();
    let exchange_client = Python::attach(|python| {
        Arc::new(HelperProcessGpuExchangeClient::new(
            python.None(),
            python.None(),
            service_name,
            "helper:killed-under-test".to_string(),
        ))
    });
    let frame = check_out_pixel_surface(&exchange_client, "pool-slot-killed#1");
    frame
        .lock_the_iosurface_for_cpu_access(false)
        .expect("the write lock");
    let mut standard_output = std::io::stdout();
    writeln!(standard_output, "{HELPER_HOLDS_THE_FRAME_MARKER}").expect("write the marker");
    standard_output.flush().expect("flush the marker");
    loop {
        std::thread::park();
    }
}

/// A helper killed while holding a frame — lease, use count and IOSurface
/// lock all taken — gives the slot back: the service drops the lease with
/// the connection, and the kernel the use count with the process.
#[test]
fn a_killed_helper_releases_the_frame_it_held() {
    use std::io::BufRead as _;
    if !a_vulkan_device_is_available() {
        return;
    }
    let share = SurfaceShareUnderTest::start("killed");
    share.publish_pool_slot_frame("pool-slot-killed", 1);
    let iosurface = share.iosurface_registered_as("pool-slot-killed");

    let mut helper =
        std::process::Command::new(std::env::current_exe().expect("this test binary's path"))
            .args([
                "--exact",
                "python_helper_process_pixel_exchange::iosurface_pool_slot_import_tests::\
         helper_process_that_holds_a_frame_until_it_is_killed",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(
                SERVICE_NAME_FOR_THE_HELPER_UNDER_TEST,
                share.channel_name_for_the_helper(),
            )
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("the helper process starts");
    let _admission = share.admit_helper_process(helper.id());

    let (marker_seen, marker_heard) = std::sync::mpsc::channel();
    let helper_output = helper.stdout.take().expect("the helper's stdout");
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(helper_output).lines() {
            if line.is_ok_and(|line| line.contains(HELPER_HOLDS_THE_FRAME_MARKER)) {
                let _ = marker_seen.send(());
            }
        }
    });
    if marker_heard
        .recv_timeout(std::time::Duration::from_secs(30))
        .is_err()
    {
        let _ = helper.kill();
        panic!("the helper never reported holding the frame");
    }
    assert!(
        iosurface.is_in_use(),
        "the helper's held frame reads in use"
    );
    assert_eq!(share.outstanding_claims_on("pool-slot-killed"), 1);

    helper.kill().expect("SIGKILL the helper");
    helper.wait().expect("reap the helper");

    // The kernel tears a dead task's IOSurface client down on its own
    // schedule, 100–400 µs after the reap under load.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while (iosurface.is_in_use() || share.outstanding_claims_on("pool-slot-killed") != 0)
        && std::time::Instant::now() < deadline
    {
        std::thread::yield_now();
    }
    assert!(
        !iosurface.is_in_use(),
        "the dead helper's use count still pins the slot"
    );
    assert_eq!(
        share.outstanding_claims_on("pool-slot-killed"),
        0,
        "the dead helper's lease still pins the slot"
    );
}

/// The IOSurface lock brackets CPU access, and a frame dropped while
/// locked lets the lock go.
#[test]
fn cpu_access_locks_the_iosurface_and_a_dropped_frame_unlocks_it() {
    if !a_vulkan_device_is_available() {
        return;
    }
    let share = SurfaceShareUnderTest::start("cpu-lock");
    share.publish_pool_slot_frame("pool-slot-locked", 1);
    let iosurface = share.iosurface_registered_as("pool-slot-locked");
    let exchange_client = exchange_client_on(&share);

    let frame = check_out_pixel_surface(&exchange_client, "pool-slot-locked#1");
    frame
        .lock_the_iosurface_for_cpu_access(false)
        .expect("the write lock");
    // SAFETY: the mapping spans the 32x32 BGRA surface.
    unsafe { frame.host_mapped_base_address().add(8).write(0x5A) };
    frame
        .unlock_the_iosurface_after_cpu_access()
        .expect("the unlock");
    frame
        .lock_the_iosurface_for_cpu_access(true)
        .expect("the read lock");
    drop(frame);

    // SAFETY: an unlock with no matching lock is refused, not undefined.
    let unlock_with_nothing_held = unsafe {
        iosurface.unlock(
            objc2_io_surface::IOSurfaceLockOptions::ReadOnly,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(
        unlock_with_nothing_held, 0,
        "the dropped frame must have released the read lock it held"
    );

    // SAFETY: the surface is live and the seed pointer is optional.
    let kern_return = unsafe {
        iosurface.lock(
            objc2_io_surface::IOSurfaceLockOptions::empty(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(kern_return, 0);
    // SAFETY: byte 8 lies inside the locked surface.
    let written = unsafe { iosurface.base_address().as_ptr().cast::<u8>().add(8).read() };
    // SAFETY: unlocks the lock taken above.
    unsafe {
        iosurface.unlock(
            objc2_io_surface::IOSurfaceLockOptions::empty(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(written, 0x5A, "the helper's write lands in the IOSurface");
}
