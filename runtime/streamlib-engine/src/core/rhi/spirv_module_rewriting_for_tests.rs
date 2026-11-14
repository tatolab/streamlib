// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Rewriting a compiled SPIR-V module so a test can reach a binding-name
//! refusal no shader in the tree can express.
//!
//! The build compiles only well-formed shaders with their names kept, so a stripped name, a
//! slot two stages spell differently, and one name on two slots have no fixture
//! that produces them. Each is reached by editing the debug and decoration
//! instructions of a real compiled module.

use super::kernel_binding_names::{
    OP_SOURCE, OP_SOURCE_CONTINUED, OP_STRING, SPIRV_HEADER_WORD_COUNT,
};

const OP_NAME: u32 = 5;
const OP_DECORATE: u32 = 71;
const DECORATION_BINDING: u32 = 33;
const DECORATION_DESCRIPTOR_SET: u32 = 34;

/// Drop every `OpName` from a module, reproducing an optimised compile without
/// debug info.
pub(crate) fn strip_every_debug_name_from_spirv_module(spirv: &[u8]) -> Vec<u8> {
    let (stripped, stripped_count) =
        rewrite_spirv_instructions(spirv, |opcode, _| (opcode == OP_NAME).then(Vec::new));
    assert!(stripped_count > 0, "the module carries no OpName to strip");
    stripped
}

/// Respell one binding, leaving its slot and every other instruction alone.
pub(crate) fn rename_binding_in_spirv_module(
    spirv: &[u8],
    current_name: &str,
    replacement_name: &str,
) -> Vec<u8> {
    let (renamed, renamed_count) = rewrite_spirv_instructions(spirv, |opcode, instruction| {
        if opcode != OP_NAME
            || instruction.len() < 3
            || decode_spirv_literal_string(&instruction[2..]) != current_name
        {
            return None;
        }
        let mut rewritten = vec![0, instruction[1]];
        rewritten.extend(encode_spirv_literal_string(replacement_name));
        rewritten[0] = ((rewritten.len() as u32) << 16) | OP_NAME;
        Some(rewritten)
    });
    assert!(
        renamed_count > 0,
        "the module names nothing `{current_name}`"
    );
    renamed
}

/// Move one binding to another slot, leaving its name and every other
/// instruction alone.
pub(crate) fn move_binding_to_another_slot_in_spirv_module(
    spirv: &[u8],
    current_slot: u32,
    replacement_slot: u32,
) -> Vec<u8> {
    let (moved, moved_count) = rewrite_spirv_instructions(spirv, |opcode, instruction| {
        if opcode != OP_DECORATE
            || instruction.len() < 4
            || instruction[2] != DECORATION_BINDING
            || instruction[3] != current_slot
        {
            return None;
        }
        let mut rewritten = instruction.to_vec();
        rewritten[3] = replacement_slot;
        Some(rewritten)
    });
    assert!(
        moved_count > 0,
        "the module decorates nothing with binding {current_slot}"
    );
    moved
}

/// Move one binding to another descriptor set, leaving its slot, its name and
/// every other instruction alone.
pub(crate) fn move_binding_to_another_descriptor_set_in_spirv_module(
    spirv: &[u8],
    current_set: u32,
    replacement_set: u32,
) -> Vec<u8> {
    let (moved, moved_count) = rewrite_spirv_instructions(spirv, |opcode, instruction| {
        if opcode != OP_DECORATE
            || instruction.len() < 4
            || instruction[2] != DECORATION_DESCRIPTOR_SET
            || instruction[3] != current_set
        {
            return None;
        }
        let mut rewritten = instruction.to_vec();
        rewritten[3] = replacement_set;
        Some(rewritten)
    });
    assert!(
        moved_count > 0,
        "the module decorates nothing with descriptor set {current_set}"
    );
    moved
}

/// Name a file in the module's `OpSource`, with its source text and its
/// `OpString` file name packed as SPIRV-Tools 2023.6 packs a non-ASCII
/// character: each of its bytes sign-extended, so the rest of that word reads
/// `0xFF`.
pub(crate) fn pack_the_debug_source_strings_sign_extended_for_a_non_ascii_character(
    spirv: &[u8],
) -> Vec<u8> {
    const SOURCE_TEXT_AFTER_AN_EM_DASH: &[u8] = b"// \xe2\xff\xff\xff the";
    const FILE_NAME_AFTER_AN_E_ACUTE: &[u8] = b"caf\xc3\xa9\xff\xff\xffcomp";
    replace_the_op_source_with_one_naming_a_file(
        spirv,
        FILE_NAME_AFTER_AN_E_ACUTE,
        SOURCE_TEXT_AFTER_AN_EM_DASH,
        &[],
    )
}

/// Name a file in the module's `OpSource` and split one multi-byte character
/// between its source text and an `OpSourceContinued` after it, so neither
/// literal is UTF-8 alone.
pub(crate) fn split_a_multi_byte_character_across_op_source_and_op_source_continued(
    spirv: &[u8],
) -> Vec<u8> {
    replace_the_op_source_with_one_naming_a_file(spirv, b"blend.comp", b"// \xe2", b"\x80\x94 the")
}

/// Replace the module's `OpSource` with an `OpString` holding `file_name` under
/// a fresh id and an `OpSource` naming that file with `source_text`, followed
/// by an `OpSourceContinued` when `continued_source_text` is not empty.
fn replace_the_op_source_with_one_naming_a_file(
    spirv: &[u8],
    file_name: &[u8],
    source_text: &[u8],
    continued_source_text: &[u8],
) -> Vec<u8> {
    const SPIRV_HEADER_ID_BOUND_WORD_INDEX: usize = 3;
    let mut words = spirv_module_words(spirv);
    let file_name_string_id = words[SPIRV_HEADER_ID_BOUND_WORD_INDEX];
    words[SPIRV_HEADER_ID_BOUND_WORD_INDEX] += 1;
    let (rewritten, rewritten_count) =
        rewrite_spirv_instructions(&spirv_module_bytes(&words), |opcode, instruction| {
            if opcode != OP_SOURCE {
                return None;
            }
            let mut replacement = with_literal_bytes(&[OP_STRING, file_name_string_id], file_name);
            replacement.extend(with_literal_bytes(
                &[
                    OP_SOURCE,
                    instruction[1],
                    instruction[2],
                    file_name_string_id,
                ],
                source_text,
            ));
            if !continued_source_text.is_empty() {
                replacement.extend(with_literal_bytes(
                    &[OP_SOURCE_CONTINUED],
                    continued_source_text,
                ));
            }
            Some(replacement)
        });
    assert!(rewritten_count > 0, "the module carries no OpSource");
    rewritten
}

/// `leading_words` with its header's opcode kept, `literal` packed after it as
/// a null-terminated, zero-padded string, and the word count set to fit.
fn with_literal_bytes(leading_words: &[u32], literal: &[u8]) -> Vec<u32> {
    let mut literal_bytes = literal.to_vec();
    literal_bytes.push(0);
    while !literal_bytes.len().is_multiple_of(4) {
        literal_bytes.push(0);
    }
    let mut instruction = leading_words.to_vec();
    instruction.extend(spirv_module_words(&literal_bytes));
    instruction[0] = ((instruction.len() as u32) << 16) | (leading_words[0] & 0xffff);
    instruction
}

/// Walk a module instruction by instruction, replacing each one the rewriter
/// returns words for and dropping each one it returns no words for; reports how
/// many instructions it touched.
fn rewrite_spirv_instructions(
    spirv: &[u8],
    mut rewrite_instruction: impl FnMut(u32, &[u32]) -> Option<Vec<u32>>,
) -> (Vec<u8>, usize) {
    let words = spirv_module_words(spirv);
    let mut rewritten: Vec<u32> = words[..SPIRV_HEADER_WORD_COUNT].to_vec();
    let mut rewritten_instruction_count = 0;
    let mut at = SPIRV_HEADER_WORD_COUNT;
    while at < words.len() {
        let word_count = (words[at] >> 16) as usize;
        let opcode = words[at] & 0xffff;
        assert!(word_count > 0, "malformed SPIR-V instruction");
        let instruction = &words[at..at + word_count];
        match rewrite_instruction(opcode, instruction) {
            Some(replacement) => {
                rewritten.extend_from_slice(&replacement);
                rewritten_instruction_count += 1;
            }
            None => rewritten.extend_from_slice(instruction),
        }
        at += word_count;
    }
    (spirv_module_bytes(&rewritten), rewritten_instruction_count)
}

fn spirv_module_words(spirv: &[u8]) -> Vec<u32> {
    assert_eq!(
        spirv.len() % 4,
        0,
        "a SPIR-V module is a whole number of 32-bit words"
    );
    spirv
        .as_chunks::<4>()
        .0
        .iter()
        .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
        .collect()
}

fn spirv_module_bytes(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}

/// A SPIR-V literal string is null-terminated UTF-8 packed little-endian into
/// whole words and zero-padded to a word boundary (SPIR-V 1.6 §2.2.1).
fn decode_spirv_literal_string(words: &[u32]) -> String {
    let bytes = spirv_module_bytes(words);
    let terminator = bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..terminator]).into_owned()
}

fn encode_spirv_literal_string(literal: &str) -> Vec<u32> {
    let mut bytes = literal.as_bytes().to_vec();
    bytes.push(0);
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
    spirv_module_words(&bytes)
}
