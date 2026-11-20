# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The fix named when a pin's declared default is not a value its type takes."""

from __future__ import annotations

from ._pin_value_stored_form import PIN_TYPE_DESCRIPTION_BY_NAME, PinValueNotTaken

__all__ = ["fix_of_a_default_not_taken"]


def fix_of_a_default_not_taken(not_taken: PinValueNotTaken, pin_python_form: str) -> str:
    """The fix a refusal of a default `not_taken` names, for a pin written `pin_python_form`."""
    match not_taken.kind:
        case "not_of_its_type":
            python_form = (
                pin_python_form
                if not_taken.pin_type["type"] == "bundle"
                else PIN_TYPE_DESCRIPTION_BY_NAME[not_taken.pin_type["type"]].python_forms[0]
            )
            return (
                f"give it a default of its type, or type it `{python_form} | None = None` "
                f"so it may be unset."
            )
        case "outside_its_exact_size":
            return "give it a default that fits."
        case "not_finite":
            return "give it a finite default."
        case "not_an_option":
            return "default to one of them."
        case "outside_its_range":
            return "give it a default inside its range."
