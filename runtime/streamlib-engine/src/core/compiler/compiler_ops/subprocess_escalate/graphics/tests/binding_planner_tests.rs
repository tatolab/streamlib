// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The binding planner: pure wire validation shared with the trace path,
//! driven here through the graphics kinds. No device, so these run everywhere
//! CI does.

use super::*;

fn declared_graphics_bindings(
    entries: &'static [(u32, &'static str, GraphicsBindingKind)],
) -> Vec<DeclaredKernelBindingUnderPlanning<'static>> {
    entries
        .iter()
        .map(
            |(binding_slot, name, kind)| DeclaredKernelBindingUnderPlanning {
                binding_slot: *binding_slot,
                name: Some(name),
                kind_wire_name: graphics_binding_kind_to_wire(*kind).wire_name(),
                surface_bound_kind: surface_bound_graphics_binding_kind(*kind),
            },
        )
        .collect()
}

/// The shape the planner tests measure against: one sampled input and
/// one storage output, deliberately different kinds so binding by slot
/// order rather than by name would swap them.
const A_DRAWING_KERNELS_BINDINGS: &[(u32, &str, GraphicsBindingKind)] = &[
    (0, "source_image", GraphicsBindingKind::SampledTexture),
    (1, "painted_output", GraphicsBindingKind::StorageImage),
];

/// A kernel whose one binding is a uniform buffer — a kind a draw
/// cannot name a surface for.
const A_TINTING_KERNELS_BINDINGS: &[(u32, &str, GraphicsBindingKind)] =
    &[(0, "tint_parameters", GraphicsBindingKind::UniformBuffer)];

fn supplied_graphics_bindings<'a>(
    entries: &'a [(&'a str, EscalateGraphicsBindingKind, &'a str)],
) -> Vec<SuppliedKernelBindingUnderPlanning<'a>> {
    entries
        .iter()
        .map(
            |(name, kind, target_id)| SuppliedKernelBindingUnderPlanning {
                name,
                target_id,
                kind_wire_name: kind.wire_name(),
            },
        )
        .collect()
}

fn draw_plan_refusal(
    declared: &'static [(u32, &'static str, GraphicsBindingKind)],
    supplied: &[(&str, EscalateGraphicsBindingKind, &str)],
) -> String {
    let declared = declared_graphics_bindings(declared);
    let supplied = supplied_graphics_bindings(supplied);
    plan_supplied_surface_bound_kernel_bindings("draw", &supplied, &declared)
        .err()
        .expect("expected the plan to be refused")
        .to_string()
}

#[test]
fn a_complete_draw_resolves_every_name_to_its_slot() {
    let declared = declared_graphics_bindings(A_DRAWING_KERNELS_BINDINGS);
    let supplied = supplied_graphics_bindings(&[
        (
            "painted_output",
            EscalateGraphicsBindingKind::StorageImage,
            "surface-out",
        ),
        (
            "source_image",
            EscalateGraphicsBindingKind::SampledTexture,
            "surface-in",
        ),
    ]);
    let planned = plan_supplied_surface_bound_kernel_bindings("draw", &supplied, &declared)
        .expect("a complete, correctly-typed draw");

    // Resolution is by name, so the order the caller supplied them in
    // is not the order the shaders declared them in — and that is fine.
    assert_eq!(planned.len(), 2);
    assert_eq!(planned[0].name, "painted_output");
    assert_eq!(planned[0].binding_slot, 1);
    assert_eq!(planned[0].kind, SurfaceBoundKernelBindingKind::StorageImage);
    assert_eq!(planned[0].target_id, "surface-out");
    assert_eq!(planned[1].name, "source_image");
    assert_eq!(planned[1].binding_slot, 0);
    assert_eq!(
        planned[1].kind,
        SurfaceBoundKernelBindingKind::SampledTexture
    );
}

/// Not expressible in a Python mapping — a dict cannot carry one key
/// twice — so the wire array is the only layer that can guard it.
#[test]
fn a_name_supplied_twice_is_refused() {
    let message = draw_plan_refusal(
        A_DRAWING_KERNELS_BINDINGS,
        &[
            (
                "source_image",
                EscalateGraphicsBindingKind::SampledTexture,
                "surface-in",
            ),
            (
                "source_image",
                EscalateGraphicsBindingKind::SampledTexture,
                "surface-other",
            ),
            (
                "painted_output",
                EscalateGraphicsBindingKind::StorageImage,
                "surface-out",
            ),
        ],
    );
    assert!(
        message.contains("binding `source_image` was supplied twice"),
        "must name the duplicate, got: {message}"
    );
    assert!(
        message.contains("`source_image`, `painted_output`"),
        "must name the kernel's declared bindings, got: {message}"
    );
}

#[test]
fn a_name_the_shaders_do_not_declare_is_refused() {
    let message = draw_plan_refusal(
        A_DRAWING_KERNELS_BINDINGS,
        &[
            (
                "source_image",
                EscalateGraphicsBindingKind::SampledTexture,
                "surface-in",
            ),
            (
                "painted_output",
                EscalateGraphicsBindingKind::StorageImage,
                "surface-out",
            ),
            (
                "sharpen_amount",
                EscalateGraphicsBindingKind::UniformBuffer,
                "surface-x",
            ),
        ],
    );
    assert!(
        message.contains("binding `sharpen_amount` is not one this kernel declares"),
        "must name the unknown binding, got: {message}"
    );
    assert!(
        message.contains("`source_image`, `painted_output`"),
        "must name the kernel's declared bindings, got: {message}"
    );
}

/// No implicit default and no carried-over value: the kernel holds no
/// binding state between draws to fall back on.
#[test]
fn a_declared_binding_left_out_is_refused() {
    let message = draw_plan_refusal(
        A_DRAWING_KERNELS_BINDINGS,
        &[(
            "source_image",
            EscalateGraphicsBindingKind::SampledTexture,
            "surface-in",
        )],
    );
    assert!(
        message.contains("binding `painted_output` was not supplied"),
        "must name the missing binding, got: {message}"
    );
    assert!(
        message.contains("do not persist between draws"),
        "must say why there is no fallback, got: {message}"
    );
}

#[test]
fn a_binding_supplied_as_the_wrong_kind_is_refused() {
    let message = draw_plan_refusal(
        A_DRAWING_KERNELS_BINDINGS,
        &[
            (
                "source_image",
                EscalateGraphicsBindingKind::SampledTexture,
                "surface-in",
            ),
            (
                "painted_output",
                EscalateGraphicsBindingKind::StorageBuffer,
                "surface-out",
            ),
        ],
    );
    assert!(
        message.contains("binding `painted_output` was supplied as storage_buffer"),
        "must name the binding and the kind supplied, got: {message}"
    );
    assert!(
        message.contains("declares it storage_image"),
        "must name the kind the kernel declares, got: {message}"
    );
}

/// A uniform buffer is legal in a shader and legal on the wire, but no
/// escalate op mints one a descriptor can point at — so the draw that
/// would need one is refused rather than silently unbound.
#[test]
fn a_binding_of_a_kind_no_surface_can_back_is_refused() {
    let message = draw_plan_refusal(
        A_TINTING_KERNELS_BINDINGS,
        &[(
            "tint_parameters",
            EscalateGraphicsBindingKind::UniformBuffer,
            "surface-x",
        )],
    );
    assert!(
        message.contains(
            "binding `tint_parameters` is uniform_buffer, which a draw cannot bind by surface id"
        ),
        "must name the binding and its kind, got: {message}"
    );
}

/// A draw binds a tensor storage buffer by surface id, as a dispatch does.
#[test]
fn a_storage_buffer_binding_plans_as_a_storage_buffer() {
    const A_TENSOR_READING_KERNELS_BINDINGS: &[(u32, &str, GraphicsBindingKind)] = &[
        (0, "source_image", GraphicsBindingKind::SampledTexture),
        (1, "detection_scores", GraphicsBindingKind::StorageBuffer),
    ];
    let declared = declared_graphics_bindings(A_TENSOR_READING_KERNELS_BINDINGS);
    let supplied = supplied_graphics_bindings(&[
        (
            "source_image",
            EscalateGraphicsBindingKind::SampledTexture,
            "surface-in",
        ),
        (
            "detection_scores",
            EscalateGraphicsBindingKind::StorageBuffer,
            "tensor-in",
        ),
    ]);
    let planned = plan_supplied_surface_bound_kernel_bindings("draw", &supplied, &declared)
        .expect("a storage buffer is a bindable kind");
    assert_eq!(planned[1].name, "detection_scores");
    assert_eq!(planned[1].binding_slot, 1);
    assert_eq!(
        planned[1].kind,
        SurfaceBoundKernelBindingKind::StorageBuffer
    );
    assert_eq!(planned[1].target_id, "tensor-in");
}

/// A kernel with no bindings at all draws — the empty case is not an
/// error, and the "missing" rule has nothing to fire on.
#[test]
fn a_kernel_declaring_nothing_needs_nothing_supplied() {
    let planned = plan_supplied_surface_bound_kernel_bindings("draw", &[], &[])
        .expect("an unbound kernel draws");
    assert!(planned.is_empty());
}
