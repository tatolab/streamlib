// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use super::*;
use crate::python_surface_share_service_for_tests::SurfaceShareUnderTest;
use std::io::Write as _;
use streamlib::sdk::context::{
    GpuPoolClaimTableRowLease, GpuPoolClaimTablesOfOnePool, GpuPoolSlotFrameDescriptorBody,
};

fn a_vulkan_device_is_available() -> bool {
    match streamlib_consumer_rhi::vulkan_device_uuid_of_the_first_device_the_loader_enumerates()
        .and_then(ConsumerVulkanDevice::new_on_the_device_the_runtime_named)
    {
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
        .expect("the checkout and import")
    else {
        panic!("{surface_id:?} is registered as a pixel buffer");
    };
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
    let exchange_client = share.exchange_client_labelled("iosurface-import-under-test");

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
        exchange_client
            .pool_slot_imports_by_pool_slot
            .held_pool_slot_count(),
        1,
        "the slot's import stays cached for its next frame"
    );
    assert!(
        !iosurface.is_in_use(),
        "a cached import must not pin the slot while no frame is held"
    );
}

/// The slot's next frame reuses its import: no second lookup, no second
/// Vulkan import, and no port sent with its checkout.
#[test]
fn a_later_frame_over_a_cached_slot_reuses_the_slots_import() {
    if !a_vulkan_device_is_available() {
        return;
    }
    let share = SurfaceShareUnderTest::start("slot-reuse");
    share.publish_pool_slot_frame("pool-slot-reused", 1);
    let iosurface = share.iosurface_registered_as("pool-slot-reused");
    let exchange_client = share.exchange_client_labelled("iosurface-import-under-test");

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
        "no port may linger once the frames over the slot are dropped"
    );
}

/// A slot the engine unregisters leaves the cache on the connection's next
/// reply, whichever request that reply answers.
#[test]
fn a_retired_slot_leaves_the_cache_on_the_next_reply() {
    if !a_vulkan_device_is_available() {
        return;
    }
    let share = SurfaceShareUnderTest::start("slot-retired");
    share.publish_pool_slot_frame("pool-slot-retired", 1);
    share.publish_pool_slot_frame("pool-slot-kept", 1);
    let exchange_client = share.exchange_client_labelled("iosurface-import-under-test");
    drop(check_out_pixel_surface(
        &exchange_client,
        "pool-slot-retired#1",
    ));
    assert_eq!(
        exchange_client
            .pool_slot_imports_by_pool_slot
            .held_pool_slot_count(),
        1
    );

    share.unregister_pool_slot("pool-slot-retired");
    drop(check_out_pixel_surface(
        &exchange_client,
        "pool-slot-kept#1",
    ));

    assert!(
        exchange_client
            .pool_slot_imports_by_pool_slot
            .import_held_for("pool-slot-retired")
            .is_none(),
        "the retired slot must leave the cache on the connection's next reply"
    );
    assert!(
        exchange_client
            .pool_slot_imports_by_pool_slot
            .import_held_for("pool-slot-kept")
            .is_some()
    );
}

/// Replacing the connection empties the cache, whether or not the death watch
/// ever fires: the notices of the slots it held would have come on the
/// connection that is gone.
#[test]
fn replacing_the_connection_empties_the_cache() {
    if !a_vulkan_device_is_available() {
        return;
    }
    let share = SurfaceShareUnderTest::start("connection-replaced");
    share.publish_pool_slot_frame("pool-slot-before", 1);
    share.publish_pool_slot_frame("pool-slot-after", 1);
    let exchange_client = share.exchange_client_labelled("iosurface-import-under-test");
    drop(check_out_pixel_surface(
        &exchange_client,
        "pool-slot-before#1",
    ));

    let replaced_connection = exchange_client
        .surface_share_mach_connection
        .lock()
        .take()
        .expect("the first checkout opened a connection");
    drop(check_out_pixel_surface(
        &exchange_client,
        "pool-slot-after#1",
    ));

    assert!(
        exchange_client
            .pool_slot_imports_by_pool_slot
            .import_held_for("pool-slot-before")
            .is_none(),
        "a slot imported over the replaced connection must leave the cache"
    );
    assert_eq!(
        exchange_client
            .pool_slot_imports_by_pool_slot
            .held_pool_slot_count(),
        1
    );
    drop(replaced_connection);
}

fn the_frame_of(
    row_lease: &GpuPoolClaimTableRowLease,
    generation: u64,
) -> GpuPoolSlotFrameDescriptorBody {
    GpuPoolSlotFrameDescriptorBody {
        pool: row_lease.table().pool_uuid(),
        slot: row_lease.row_index(),
        generation,
        timeline_value: 0,
        image_layout: 0,
    }
}

/// After the pool's claim table and the slot's IOSurface cross once, pooled
/// frames are claimed in the shared table and read over the slot's one
/// import: no checkout lease, no further import, and both halves of the
/// claim — the table's and the IOSurface use count — held while a frame is.
#[test]
fn pooled_frames_cross_with_no_port_per_frame_once_the_slot_set_has() {
    if !a_vulkan_device_is_available() {
        return;
    }
    const POOL_SLOT_KEY: &str = "pool-slot-claimed";
    let share = SurfaceShareUnderTest::start("claim-table-read");
    share.publish_pool_slot_frame(POOL_SLOT_KEY, 1);
    let iosurface = share.iosurface_registered_as(POOL_SLOT_KEY);
    let mut claim_tables_of_the_pool = GpuPoolClaimTablesOfOnePool::named("claim-table-read");
    let row_lease = claim_tables_of_the_pool
        .lease_a_row_for_a_slot(
            share.check_out_leases().gpu_pool_claim_tables(),
            POOL_SLOT_KEY,
        )
        .expect("the pool leases its slot a row");
    let exchange_client = share.exchange_client_labelled("claim-table-read-under-test");
    row_lease.row().publish_generation(1);

    let first_read_claim = exchange_client
        .note_and_claim_the_pooled_gpu_frame_a_read_carries(the_frame_of(&row_lease, 1))
        .expect("the first frame fetches the pool's claim table")
        .expect("the first read claims its frame");
    let HelperCheckedOutSurface::PixelBuffer(first_frame) = exchange_client
        .check_out_and_import(&format!("{POOL_SLOT_KEY}#1"))
        .expect("the first frame imports its slot once")
    else {
        panic!("a pixel_buffer registration reads as a pixel buffer");
    };
    let slot_import = Arc::clone(&first_frame.iosurface_pool_slot_import);
    assert!(row_lease.row().is_claimed());
    assert!(iosurface.is_in_use(), "a held frame raises the use count");
    assert_eq!(
        share.outstanding_claims_on(POOL_SLOT_KEY),
        0,
        "a claimed frame takes no checkout lease"
    );
    drop((first_frame, first_read_claim));
    assert!(!row_lease.row().is_claimed());

    for generation in 2..=4 {
        row_lease.row().publish_generation(generation);
        share.publish_frame_generation_over_a_registered_pool_slot(POOL_SLOT_KEY, generation);
        let read_claim = exchange_client
            .note_and_claim_the_pooled_gpu_frame_a_read_carries(the_frame_of(
                &row_lease, generation,
            ))
            .expect("a known slot's frame needs no hand-over")
            .expect("a read claims the frame it carries");
        let HelperCheckedOutSurface::PixelBuffer(frame) = exchange_client
            .check_out_and_import(&format!("{POOL_SLOT_KEY}#{generation}"))
            .expect("a claimed frame reads over its slot's import")
        else {
            panic!("a pixel_buffer registration reads as a pixel buffer");
        };
        assert!(
            Arc::ptr_eq(&slot_import, &frame.iosurface_pool_slot_import),
            "every later frame reuses the slot's one import, asking the service nothing"
        );
        assert_eq!(share.outstanding_claims_on(POOL_SLOT_KEY), 0);
        drop((frame, read_claim));
        assert!(!row_lease.row().is_claimed());
    }

    row_lease.row().publish_generation(5);
    assert!(
        exchange_client
            .claim_in_its_pool_claim_table(&format!("{POOL_SLOT_KEY}#4"))
            .is_err(),
        "a frame the slot moved past is refused as recycled"
    );
    drop(slot_import);
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
    let exchange_client = share.exchange_client_labelled("iosurface-import-under-test");
    let still_held_view = check_out_pixel_surface(&exchange_client, "pool-slot-orphaned#1");
    assert_eq!(
        exchange_client
            .pool_slot_imports_by_pool_slot
            .held_pool_slot_count(),
        1
    );

    drop(share);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while exchange_client
        .pool_slot_imports_by_pool_slot
        .held_pool_slot_count()
        != 0
        && std::time::Instant::now() < deadline
    {
        std::thread::yield_now();
    }
    assert_eq!(
        exchange_client
            .pool_slot_imports_by_pool_slot
            .held_pool_slot_count(),
        0,
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
            streamlib_consumer_rhi::vulkan_device_uuid_of_the_first_device_the_loader_enumerates(),
            "helper:killed-under-test".to_string(),
        ))
    });
    let frame = check_out_pixel_surface(&exchange_client, "pool-slot-killed#1");
    frame
        .lock_the_iosurface_for_cpu_access_once(false)
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
                "python_helper_process_pixel_exchange::macos::iosurface_pool_slot_import_tests::\
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
    let exchange_client = share.exchange_client_labelled("iosurface-import-under-test");

    let frame = check_out_pixel_surface(&exchange_client, "pool-slot-locked#1");
    frame
        .lock_the_iosurface_for_cpu_access_once(false)
        .expect("the write lock");
    // SAFETY: the mapping spans the 32x32 BGRA surface.
    unsafe { frame.host_mapped_base_address().add(8).write(0x5A) };
    frame
        .unlock_the_iosurface_after_cpu_access()
        .expect("the unlock");
    frame
        .lock_the_iosurface_for_cpu_access_once(true)
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
