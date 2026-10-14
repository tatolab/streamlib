// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Which physical device the engine builds its one logical device on.

use vulkanalia::vk;

use crate::core::{Error, Result};

use super::vulkan_instance::NO_USABLE_VULKAN_DRIVER_GUIDANCE;

/// A physical device the loader listed, as device selection weighs it.
#[derive(Debug, Clone)]
pub(crate) struct VulkanPhysicalDeviceCandidate {
    pub(crate) physical_device: vk::PhysicalDevice,
    pub(crate) device_name: String,
    pub(crate) device_type: vk::PhysicalDeviceType,
    /// `vkGetPhysicalDeviceProperties`, read once at enumeration.
    pub(crate) device_properties: vk::PhysicalDeviceProperties,
}

impl std::fmt::Display for VulkanPhysicalDeviceCandidate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} (type: {})",
            self.device_name,
            vulkan_physical_device_type_name(self.device_type)
        )
    }
}

/// Why the engine could not build its logical device on a candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VulkanDevicePassOverReason {
    /// No queue family serves graphics.
    NoGraphicsQueueFamily,
    /// `vkCreateDevice` returned an error.
    LogicalDeviceCreationRefused { vulkan_error: vk::ErrorCode },
    /// A sentinel allocation failed in an export flavour the device
    /// advertises.
    ExportPoolSentinelAllocationFailed { refusal: String },
    /// Any other step of bringing the device up failed.
    DeviceBringUpFailed { refusal: String },
}

impl std::fmt::Display for VulkanDevicePassOverReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoGraphicsQueueFamily => write!(formatter, "it has no graphics queue family"),
            Self::LogicalDeviceCreationRefused { vulkan_error }
                if *vulkan_error == vk::ErrorCode::FEATURE_NOT_PRESENT
                    || *vulkan_error == vk::ErrorCode::EXTENSION_NOT_PRESENT =>
            {
                write!(
                    formatter,
                    "vkCreateDevice refused a feature or extension the engine requests: \
                     {vulkan_error}"
                )
            }
            Self::LogicalDeviceCreationRefused { vulkan_error } => {
                write!(formatter, "vkCreateDevice failed: {vulkan_error}")
            }
            Self::ExportPoolSentinelAllocationFailed { refusal } => write!(
                formatter,
                "a sentinel allocation failed in an export flavour the device advertises: {refusal}"
            ),
            Self::DeviceBringUpFailed { refusal } => {
                write!(formatter, "bringing the device up failed: {refusal}")
            }
        }
    }
}

impl From<Error> for VulkanDevicePassOverReason {
    fn from(refusal: Error) -> Self {
        Self::DeviceBringUpFailed {
            refusal: refusal.to_string(),
        }
    }
}

/// A candidate the engine passed over, named with its reason.
#[derive(Debug, Clone)]
pub(crate) struct VulkanDevicePassedOver {
    pub(crate) candidate: VulkanPhysicalDeviceCandidate,
    pub(crate) reason: VulkanDevicePassOverReason,
}

impl std::fmt::Display for VulkanDevicePassedOver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.candidate, self.reason)
    }
}

/// What the engine built, the candidate it built on, and every candidate
/// passed over before it.
pub(crate) struct VulkanDeviceSelection<BuiltDevice> {
    pub(crate) built_device: BuiltDevice,
    pub(crate) chosen_candidate: VulkanPhysicalDeviceCandidate,
    pub(crate) passed_over_candidates: Vec<VulkanDevicePassedOver>,
}

/// The name the log gives a physical-device type.
pub(crate) fn vulkan_physical_device_type_name(
    device_type: vk::PhysicalDeviceType,
) -> &'static str {
    match device_type {
        vk::PhysicalDeviceType::DISCRETE_GPU => "Discrete GPU",
        vk::PhysicalDeviceType::INTEGRATED_GPU => "Integrated GPU",
        vk::PhysicalDeviceType::VIRTUAL_GPU => "Virtual GPU",
        vk::PhysicalDeviceType::CPU => "CPU",
        _ => "Other",
    }
}

/// The loader's order with every CPU-type device moved last, otherwise
/// untouched. The loader sorts by type only from 1.3.204; this pins the
/// software device last whatever loader runs.
fn candidates_in_the_loaders_order_with_cpu_type_last(
    mut candidates: Vec<VulkanPhysicalDeviceCandidate>,
) -> Vec<VulkanPhysicalDeviceCandidate> {
    candidates.sort_by_key(|candidate| candidate.device_type == vk::PhysicalDeviceType::CPU);
    candidates
}

/// Build on each candidate, in the loader's order with CPU-type devices last,
/// and keep the first that accepts. Refuses naming each pass-over when none
/// accepts.
pub(crate) fn build_on_the_first_candidate_that_accepts<BuiltDevice>(
    candidates_in_the_loaders_order: Vec<VulkanPhysicalDeviceCandidate>,
    mut build_on_candidate: impl FnMut(
        &VulkanPhysicalDeviceCandidate,
    )
        -> std::result::Result<BuiltDevice, VulkanDevicePassOverReason>,
) -> Result<VulkanDeviceSelection<BuiltDevice>> {
    let mut passed_over_candidates = Vec::new();
    for candidate in
        candidates_in_the_loaders_order_with_cpu_type_last(candidates_in_the_loaders_order)
    {
        match build_on_candidate(&candidate) {
            Ok(built_device) => {
                return Ok(VulkanDeviceSelection {
                    built_device,
                    chosen_candidate: candidate,
                    passed_over_candidates,
                });
            }
            Err(reason) => {
                passed_over_candidates.push(VulkanDevicePassedOver { candidate, reason })
            }
        }
    }
    let every_pass_over = passed_over_candidates
        .iter()
        .map(VulkanDevicePassedOver::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    Err(Error::GpuError(format!(
        "The engine could build on no Vulkan device the loader listed — passed over: \
         {every_pass_over}. {NO_USABLE_VULKAN_DRIVER_GUIDANCE}"
    )))
}

impl<BuiltDevice> VulkanDeviceSelection<BuiltDevice> {
    /// Log the chosen device beside every candidate passed over, and hand
    /// back what was built.
    pub(crate) fn log_the_chosen_device_beside_every_pass_over(self) -> BuiltDevice {
        for passed_over in &self.passed_over_candidates {
            tracing::warn!(
                "Passed over Vulkan device {passed_over}; the engine built on {} instead",
                self.chosen_candidate.device_name
            );
        }
        tracing::info!(
            passed_over_device_count = self.passed_over_candidates.len(),
            "Selected Vulkan device: {}",
            self.chosen_candidate
        );
        self.built_device
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vulkanalia::vk::Handle as _;

    fn candidate(
        device_name: &str,
        device_type: vk::PhysicalDeviceType,
    ) -> VulkanPhysicalDeviceCandidate {
        VulkanPhysicalDeviceCandidate {
            physical_device: vk::PhysicalDevice::null(),
            device_name: device_name.to_owned(),
            device_type,
            device_properties: vk::PhysicalDeviceProperties::default(),
        }
    }

    fn names_of(candidates: &[VulkanPhysicalDeviceCandidate]) -> Vec<&str> {
        candidates
            .iter()
            .map(|candidate| candidate.device_name.as_str())
            .collect()
    }

    /// Mental-revert: prefer discrete over integrated, or drop the sort,
    /// and the asserted order breaks.
    #[test]
    fn a_cpu_type_device_goes_last_and_every_other_keeps_the_loaders_order() {
        let ordered = candidates_in_the_loaders_order_with_cpu_type_last(vec![
            candidate("llvmpipe", vk::PhysicalDeviceType::CPU),
            candidate("iGPU", vk::PhysicalDeviceType::INTEGRATED_GPU),
            candidate("second-cpu", vk::PhysicalDeviceType::CPU),
            candidate("dGPU", vk::PhysicalDeviceType::DISCRETE_GPU),
            candidate("vGPU", vk::PhysicalDeviceType::VIRTUAL_GPU),
        ]);

        assert_eq!(
            names_of(&ordered),
            ["iGPU", "dGPU", "vGPU", "llvmpipe", "second-cpu"],
            "the engine moves CPU-type devices last and never prefers one hardware type itself"
        );
    }

    /// The loader lists the software device first. A fake builder refuses the
    /// hardware device for each named reason in turn; the software device
    /// behind it is taken and the refusal is reported with the device that
    /// gave it.
    #[test]
    fn the_first_device_the_engine_can_build_on_wins_and_a_cpu_type_device_is_last() {
        for refusal in [
            VulkanDevicePassOverReason::LogicalDeviceCreationRefused {
                vulkan_error: vk::ErrorCode::FEATURE_NOT_PRESENT,
            },
            VulkanDevicePassOverReason::NoGraphicsQueueFamily,
            VulkanDevicePassOverReason::ExportPoolSentinelAllocationFailed {
                refusal: "OUT_OF_DEVICE_MEMORY on opaque_fd_device_local".into(),
            },
        ] {
            let candidates_in_the_loaders_order = vec![
                candidate("llvmpipe", vk::PhysicalDeviceType::CPU),
                candidate("RTX 3090", vk::PhysicalDeviceType::DISCRETE_GPU),
            ];
            let mut attempted = Vec::new();
            let selection = build_on_the_first_candidate_that_accepts(
                candidates_in_the_loaders_order,
                |candidate| {
                    attempted.push(candidate.device_name.clone());
                    if candidate.device_type == vk::PhysicalDeviceType::CPU {
                        Ok(candidate.device_name.clone())
                    } else {
                        Err(refusal.clone())
                    }
                },
            )
            .expect("the software device accepts");

            assert_eq!(attempted, ["RTX 3090", "llvmpipe"]);
            assert_eq!(selection.built_device, "llvmpipe");
            assert_eq!(selection.chosen_candidate.device_name, "llvmpipe");
            let [passed_over] = selection.passed_over_candidates.as_slice() else {
                panic!("exactly the hardware device is passed over");
            };
            assert_eq!(passed_over.candidate.device_name, "RTX 3090");
            assert_eq!(
                passed_over.candidate.device_type,
                vk::PhysicalDeviceType::DISCRETE_GPU
            );
            assert_eq!(passed_over.reason, refusal);
        }
    }

    /// The software device, listed first, sits behind the hardware one and is
    /// never built on when the hardware one accepts.
    #[test]
    fn a_hardware_device_that_accepts_is_taken_before_the_software_device_is_tried() {
        let candidates = vec![
            candidate("llvmpipe", vk::PhysicalDeviceType::CPU),
            candidate("RTX 3090", vk::PhysicalDeviceType::DISCRETE_GPU),
        ];
        let mut attempted = Vec::new();
        let selection = build_on_the_first_candidate_that_accepts(candidates, |candidate| {
            attempted.push(candidate.device_name.clone());
            Ok::<_, VulkanDevicePassOverReason>(())
        })
        .expect("the hardware device accepts");

        assert_eq!(attempted, ["RTX 3090"]);
        assert_eq!(selection.chosen_candidate.device_name, "RTX 3090");
        assert!(selection.passed_over_candidates.is_empty());
    }

    #[test]
    fn a_device_passed_over_is_named_with_its_reason() {
        let passed_over_for = |vulkan_error| VulkanDevicePassedOver {
            candidate: candidate("RTX 3090", vk::PhysicalDeviceType::DISCRETE_GPU),
            reason: VulkanDevicePassOverReason::LogicalDeviceCreationRefused { vulkan_error },
        };

        let missing_feature = passed_over_for(vk::ErrorCode::FEATURE_NOT_PRESENT).to_string();
        assert!(
            missing_feature.starts_with("RTX 3090 (type: Discrete GPU): "),
            "{missing_feature}"
        );
        assert!(
            missing_feature.contains("refused a feature or extension"),
            "{missing_feature}"
        );

        let broken_driver = passed_over_for(vk::ErrorCode::INITIALIZATION_FAILED).to_string();
        assert!(
            broken_driver.contains("vkCreateDevice failed: "),
            "{broken_driver}"
        );
        assert!(
            !broken_driver.contains("feature"),
            "a failure that is not a missing feature must not name one: {broken_driver}"
        );
    }

    /// With every candidate refused the start is refused, naming each
    /// candidate and its reason beside the loader's guidance.
    #[test]
    fn every_candidate_refused_refuses_naming_each_pass_over_and_the_loaders_guidance() {
        let candidates = vec![
            candidate("RTX 3090", vk::PhysicalDeviceType::DISCRETE_GPU),
            candidate("llvmpipe", vk::PhysicalDeviceType::CPU),
        ];
        let refusal = build_on_the_first_candidate_that_accepts(candidates, |candidate| {
            Err::<(), _>(if candidate.device_type == vk::PhysicalDeviceType::CPU {
                VulkanDevicePassOverReason::NoGraphicsQueueFamily
            } else {
                VulkanDevicePassOverReason::DeviceBringUpFailed {
                    refusal: "allocator refused".into(),
                }
            })
        })
        .err()
        .expect("no candidate accepts");

        let Error::GpuError(message) = refusal else {
            panic!("the refusal must be the engine's typed GPU error, got {refusal:?}");
        };
        assert!(
            message.contains("RTX 3090 (type: Discrete GPU)"),
            "{message}"
        );
        assert!(message.contains("allocator refused"), "{message}");
        assert!(message.contains("llvmpipe (type: CPU)"), "{message}");
        assert!(message.contains("no graphics queue family"), "{message}");
        assert!(
            message.contains(NO_USABLE_VULKAN_DRIVER_GUIDANCE),
            "{message}"
        );
    }
}
