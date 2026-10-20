# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A 5×7 bitmap font, and the label bitmaps the compositor generates into its GLSL.

Uppercase only. A character the font has no glyph for is refused by name rather
than drawn as a blank.
"""

from __future__ import annotations

GLYPH_WIDTH = 5
GLYPH_HEIGHT = 7
# One blank column between glyphs, so a label reads as words.
GLYPH_ADVANCE = GLYPH_WIDTH + 1

# `#` is ink, anything else is background.
_GLYPH_ART: dict[str, tuple[str, ...]] = {
    " ": (".....", ".....", ".....", ".....", ".....", ".....", "....."),
    "-": (".....", ".....", ".....", "#####", ".....", ".....", "....."),
    ".": (".....", ".....", ".....", ".....", ".....", ".....", "..#.."),
    "A": (".###.", "#...#", "#...#", "#####", "#...#", "#...#", "#...#"),
    "B": ("####.", "#...#", "#...#", "####.", "#...#", "#...#", "####."),
    "C": (".####", "#....", "#....", "#....", "#....", "#....", ".####"),
    "D": ("####.", "#...#", "#...#", "#...#", "#...#", "#...#", "####."),
    "E": ("#####", "#....", "#....", "####.", "#....", "#....", "#####"),
    "F": ("#####", "#....", "#....", "####.", "#....", "#....", "#...."),
    "G": (".###.", "#...#", "#....", "#..##", "#...#", "#...#", ".###."),
    "H": ("#...#", "#...#", "#...#", "#####", "#...#", "#...#", "#...#"),
    "I": ("#####", "..#..", "..#..", "..#..", "..#..", "..#..", "#####"),
    "J": ("....#", "....#", "....#", "....#", "#...#", "#...#", ".###."),
    "K": ("#...#", "#..#.", "#.#..", "##...", "#.#..", "#..#.", "#...#"),
    "L": ("#....", "#....", "#....", "#....", "#....", "#....", "#####"),
    "M": ("#...#", "##.##", "#.#.#", "#...#", "#...#", "#...#", "#...#"),
    "N": ("#...#", "##..#", "#.#.#", "#..##", "#...#", "#...#", "#...#"),
    "O": (".###.", "#...#", "#...#", "#...#", "#...#", "#...#", ".###."),
    "P": ("####.", "#...#", "#...#", "####.", "#....", "#....", "#...."),
    "Q": (".###.", "#...#", "#...#", "#...#", "#.#.#", "#..#.", ".##.#"),
    "R": ("####.", "#...#", "#...#", "####.", "#.#..", "#..#.", "#...#"),
    "S": (".####", "#....", "#....", ".###.", "....#", "....#", "####."),
    "T": ("#####", "..#..", "..#..", "..#..", "..#..", "..#..", "..#.."),
    "U": ("#...#", "#...#", "#...#", "#...#", "#...#", "#...#", ".###."),
    "V": ("#...#", "#...#", "#...#", "#...#", "#...#", ".#.#.", "..#.."),
    "W": ("#...#", "#...#", "#...#", "#...#", "#.#.#", "##.##", "#...#"),
    "X": ("#...#", "#...#", ".#.#.", "..#..", ".#.#.", "#...#", "#...#"),
    "Y": ("#...#", "#...#", ".#.#.", "..#..", "..#..", "..#..", "..#.."),
    "Z": ("#####", "....#", "...#.", "..#..", ".#...", "#....", "#####"),
    "0": (".###.", "#...#", "#..##", "#.#.#", "##..#", "#...#", ".###."),
    "1": ("..#..", ".##..", "..#..", "..#..", "..#..", "..#..", "#####"),
    "2": (".###.", "#...#", "....#", "...#.", "..#..", ".#...", "#####"),
    "3": ("#####", "...#.", "..#..", "...#.", "....#", "#...#", ".###."),
    "4": ("...#.", "..##.", ".#.#.", "#..#.", "#####", "...#.", "...#."),
    "5": ("#####", "#....", "####.", "....#", "....#", "#...#", ".###."),
    "6": ("..##.", ".#...", "#....", "####.", "#...#", "#...#", ".###."),
    "7": ("#####", "....#", "...#.", "..#..", ".#...", ".#...", ".#..."),
    "8": (".###.", "#...#", "#...#", ".###.", "#...#", "#...#", ".###."),
    "9": (".###.", "#...#", "#...#", ".####", "....#", "...#.", ".##.."),
}


def characters_drawn_for(text: str) -> str:
    """The characters a label actually draws, which can outnumber the caller's string.

    Upper-casing can lengthen a string — `\N{LATIN SMALL LETTER SHARP S}` becomes
    `SS` — so the width and the bitmap both measure this.
    """
    return text.upper()


def label_width_in_pixels(text: str) -> int:
    """How wide `text` renders, with no trailing inter-glyph gap."""
    return max(0, len(characters_drawn_for(text)) * GLYPH_ADVANCE - 1)


def label_pixel_rows(text: str) -> list[int]:
    """One integer per font row, bit `x` set where pixel `x` is ink.

    Bit 0 is the leftmost pixel, which is the order the shader unpacks in.
    """
    glyphs = [_glyph_art_for(character) for character in characters_drawn_for(text)]
    rows = []
    for row_index in range(GLYPH_HEIGHT):
        row_bits = 0
        for glyph_index, glyph in enumerate(glyphs):
            for column, cell in enumerate(glyph[row_index]):
                if cell == "#":
                    row_bits |= 1 << (glyph_index * GLYPH_ADVANCE + column)
        rows.append(row_bits)
    return rows


def _glyph_art_for(character: str) -> tuple[str, ...]:
    glyph = _GLYPH_ART.get(character)
    if glyph is None:
        raise ValueError(
            f"the label font has no glyph for {character!r}; it covers A-Z, 0-9, "
            f"space, hyphen and full stop, and a label is upper-cased before it "
            f"is laid out"
        )
    return glyph
