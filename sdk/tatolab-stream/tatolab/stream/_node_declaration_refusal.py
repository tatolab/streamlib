# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A node declaration refused at decoration, with the finding code the declaration check answers it by."""

from __future__ import annotations

from typing import Literal, Optional

__all__ = ["NodeDeclarationFindingCode", "NodeDeclarationRefusal"]

type NodeDeclarationFindingCode = Literal[
    "pin_has_no_type",
    "pin_typed_any",
    "pin_type_unknown",
    "pin_type_not_read_yet",
    "exact_size_only_as_an_element",
    "menu_options_not_valid",
    "list_element_not_taken",
    "pin_annotation_not_taken",
    "pin_annotation_repeated",
    "former_name_finds_no_one_pin",
    "value_input_has_no_default",
    "unset_input_with_a_default",
    "pin_default_not_of_its_type",
    "pin_default_outside_its_range",
    "parameter_not_a_pin",
    "return_not_read_yet",
    "outputs_not_named",
    "wire_only_input_with_a_default",
    "output_type_found_from_no_input",
    "type_parameter_types_several_inputs",
    "field_typed_as_a_stream",
    "pin_flow_not_taken",
    "attribute_not_a_pin",
    "pin_has_a_value",
    "init_takes_parameters",
    "class_takes_a_config",
    "conversion_reads_no_pin_types",
    "declaration_retired",
    "conversion_not_one_input_to_one_output",
    "conversion_end_states_no_one_pin_type",
    "pin_group_slot_not_a_frozen_dataclass",
    "pin_group_slot_holds_no_pin",
    "pin_group_inside_a_slot",
    "pin_group_on_a_function_node",
    "pin_group_on_a_node_function",
    "node_function_parameter_not_keyword_only",
    "node_function_parameter_not_typed",
    "node_function_parameter_not_an_input",
    "input_default_not_an_input",
    "setting_on_a_node_function",
    "node_function_parameter_private",
    "node_function_not_a_plain_def",
    "node_function_takes_the_context_twice",
]
"""What a refused declaration got wrong, as the declaration check's finding names it."""


class NodeDeclarationRefusal(TypeError):
    """A node declaration refused at decoration: the words, with their fix, and what was refused."""

    def __init__(
        self,
        words: str,
        *,
        finding_code: NodeDeclarationFindingCode,
        node: str,
        pin: Optional[str] = None,
    ) -> None:
        super().__init__(words)
        self.finding_code: NodeDeclarationFindingCode = finding_code
        """What the declaration got wrong."""
        self.node: str = node
        """The node type whose declaration was refused, by its short name."""
        self.pin: Optional[str] = pin
        """The pin the refusal is about, as written; `None` for one about the whole node."""
