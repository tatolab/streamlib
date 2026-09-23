use std::sync::Arc;

use super::*;
use crate::python_bag_conversion::gpu_limited_access_of_the_typed_read_in_progress;
use crate::python_class_from_source_for_tests::class_from_source_in_namespace;
use crate::python_helper_process_pixel_exchange::HelperProcessGpuExchangeClient;
use crate::python_surface_share_service_for_tests::SurfaceShareUnderTest;
use pyo3::types::{IntoPyDict, PyDict};

const OUTPUT_PORT: &str = "frames_to_downstream";
const INPUT_PORT: &str = "frames_from_upstream";

/// A frame class somebody else could write today, using only what the
/// wheel exports: it asks the read in progress for the GPU capability and
/// keeps the claim in a field. Nothing marks it, and nothing registers it.
const FRAME_CLASS_THE_WHEEL_DOES_NOT_SHIP: &str = "\
class FrameSomebodyElseWrote:
def __init__(self, surface_id, **rest_of_the_bag):
    self.surface_id = surface_id
    gpu_limited_access = gpu_limited_access_of_the_typed_read_in_progress()
    self.claim = (
        None
        if gpu_limited_access is None
        else gpu_limited_access.claim_surface_against_producer_reuse(surface_id)
    )
";

/// One link, wired to itself, plus a reader carrying a capability that
/// reaches `share`.
///
/// The destination subscribes first: iceoryx2 drops a send with no
/// subscriber attached. Both planes live on the caller's thread because
/// iceoryx2's ports are `!Send`.
struct ReadUnderTest {
    source: Py<PythonProcessorLinkDataAccess>,
    reader: PythonLinkInputDataReader,
}

/// A data plane the way a helper process builds one, over a node in this test
/// process's own iceoryx2 domain.
fn helper_process_data_plane(python: Python<'_>) -> Py<PythonProcessorLinkDataAccess> {
    Py::new(
        python,
        PythonProcessorLinkDataAccess::over_helper_process_iceoryx2_node(
            streamlib::sdk::iceoryx2::Iceoryx2Node::for_this_test_process(),
        ),
    )
    .unwrap()
}

fn wire_one_link_into_a_reader(
    python: Python<'_>,
    label: &str,
    share: &SurfaceShareUnderTest,
) -> ReadUnderTest {
    let unique = format!("castclaim{}_{label}", std::process::id());
    let channel_service_name = format!("{unique}/frames");
    let notify_service_name = format!("{unique}_dest/notify");
    let link_id = format!("L-{unique}");

    let destination = helper_process_data_plane(python);
    destination
        .bind(python)
        .call_method1(
            "wire_input_link",
            (
                INPUT_PORT,
                &channel_service_name,
                // The link's name is its channel here: this source is on
                // this runtime. The two differ only across the mesh.
                &channel_service_name,
                streamlib::sdk::iceoryx2::THIS_MACHINE_STAMP_CLOCK_TOKEN,
                &notify_service_name,
                "read_next_in_order",
                8,
                8,
                2,
                1,
                &link_id,
            ),
        )
        .unwrap();
    let source = helper_process_data_plane(python);
    source
        .bind(python)
        .call_method1(
            "wire_output_link",
            (
                OUTPUT_PORT,
                &channel_service_name,
                &notify_service_name,
                1024,
                1 << 20,
                8,
                2,
                1,
                &link_id,
            ),
        )
        .unwrap();

    // The capability a helper's context carries: the escalate callables are
    // never reached, because a claim speaks only to the surface socket.
    let exchange_client = Arc::new(HelperProcessGpuExchangeClient::new(
        python.None(),
        python.None(),
        share.socket_path.clone(),
        "helper:read-under-test".to_string(),
    ));
    ReadUnderTest {
        source,
        reader: PythonLinkInputDataReader {
            link_data_access: destination,
            gpu_limited_access_context: Py::new(
                python,
                PythonGpuContextLimitedAccess::new_for_helper_process(Some(exchange_client)),
            )
            .unwrap(),
            ask_the_parent: None,
        },
    }
}

fn publish_a_frame_bag(python: Python<'_>, link: &ReadUnderTest, surface_id: &str) {
    let bag = PyDict::new(python);
    bag.set_item("surface_id", surface_id).unwrap();
    bag.set_item("width", 32i64).unwrap();
    bag.set_item("height", 32i64).unwrap();
    bag.set_item("timestamp_ns", 1i64).unwrap();
    link.source
        .bind(python)
        .call_method1("write_to_output_port", (OUTPUT_PORT, &bag))
        .unwrap();
}

fn frame_class<'py>(python: Python<'py>) -> Bound<'py, PyAny> {
    let namespace = PyDict::new(python);
    namespace
        .set_item(
            "gpu_limited_access_of_the_typed_read_in_progress",
            wrap_pyfunction!(gpu_limited_access_of_the_typed_read_in_progress, python).unwrap(),
        )
        .unwrap();
    class_from_source_in_namespace(
        python,
        FRAME_CLASS_THE_WHEEL_DOES_NOT_SHIP,
        "FrameSomebodyElseWrote",
        &namespace,
    )
}

/// The whole contract in one test: the cast claims the frame, the frame's
/// existence is what holds the claim, and letting the frame go is what
/// returns the slot to its producer. Nothing is called to release it.
#[test]
fn a_frame_read_into_a_type_pins_its_surface_until_the_object_goes_away() {
    let share = SurfaceShareUnderTest::start("typed-read");
    let surface_id = share.publish_one_surface();

    Python::initialize();
    Python::attach(|python| {
        let link = wire_one_link_into_a_reader(python, "held", &share);
        publish_a_frame_bag(python, &link, &surface_id);

        let frame = link
            .reader
            .read(python, INPUT_PORT, Some(&frame_class(python)))
            .expect("the read")
            .expect("the wired input received nothing");
        assert!(
            !frame.getattr("claim").unwrap().is_none(),
            "the read must offer the constructing type a way to claim"
        );
        assert_eq!(
            share.outstanding_claims_on(&surface_id),
            1,
            "a frame the consumer is holding must not be rehanded to its producer"
        );

        drop(frame);
        assert_eq!(
            share.outstanding_claims_on(&surface_id),
            0,
            "the claim releases with the object, without anything being called"
        );
    });
}

/// The offer is the read's, not the thread's: the same class constructed
/// outside a read claims nothing, which is what keeps a hand-rolled bag —
/// possibly naming no live surface at all — an ordinary construction.
#[test]
fn the_same_class_constructed_outside_a_read_claims_nothing() {
    let share = SurfaceShareUnderTest::start("outside");
    let surface_id = share.publish_one_surface();

    Python::initialize();
    Python::attach(|python| {
        let link = wire_one_link_into_a_reader(python, "outside", &share);
        let frame_class = frame_class(python);

        // Once through a read, so the offer has been opened on this thread
        // at least once — a stale offer would show up here.
        publish_a_frame_bag(python, &link, &surface_id);
        let frame = link
            .reader
            .read(python, INPUT_PORT, Some(&frame_class))
            .unwrap()
            .unwrap();
        drop(frame);

        let bag = PyDict::new(python);
        bag.set_item("surface_id", &surface_id).unwrap();
        let built_by_hand = frame_class.call((), Some(&bag)).unwrap();
        assert!(
            built_by_hand.getattr("claim").unwrap().is_none(),
            "construction outside a read is offered nothing"
        );
        assert_eq!(
            share.outstanding_claims_on(&surface_id),
            0,
            "nothing outside a read may claim a producer's slot"
        );
    });
}

/// The bare data plane a helper wires by hand holds no context, so a type
/// it constructs is offered nothing — the claim belongs to the read a
/// processor actually writes.
#[test]
fn the_context_free_data_plane_offers_no_capability() {
    let share = SurfaceShareUnderTest::start("contextfree");
    let surface_id = share.publish_one_surface();

    Python::initialize();
    Python::attach(|python| {
        let link = wire_one_link_into_a_reader(python, "contextfree", &share);
        publish_a_frame_bag(python, &link, &surface_id);

        let frame = link
            .reader
            .link_data_access
            .bind(python)
            .call_method(
                "read_from_input_port",
                (INPUT_PORT,),
                Some(
                    &[("into", frame_class(python))]
                        .into_py_dict(python)
                        .unwrap(),
                ),
            )
            .expect("the read");
        assert!(
            frame.getattr("claim").unwrap().is_none(),
            "a read with no context has no capability to offer"
        );
        assert_eq!(share.outstanding_claims_on(&surface_id), 0);
    });
}
