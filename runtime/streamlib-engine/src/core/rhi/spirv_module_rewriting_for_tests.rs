// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Rewriting a compiled SPIR-V module so a test can reach a binding-name
//! refusal no shader in the tree can express.
//!
//! The build compiles only well-formed shaders with `-g`, so a stripped name, a
//! slot two stages spell differently, and one name on two slots have no fixture
//! that produces them. Each is reached by editing the debug and decoration
//! instructions of a real compiled module.

/// A SPIR-V module opens with five header words, then a stream of
/// instructions whose first word packs word count and opcode (SPIR-V 1.6
/// §2.3).
const SPIRV_HEADER_WORD_COUNT: usize = 5;
const OP_NAME: u16 = 5;
const OP_DECORATE: u16 = 71;
const DECORATION_BINDING: u32 = 33;

/// Drop every `OpName` from a module, reproducing what `glslc -O` emits
/// without `-g`.
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
        rewritten[0] = ((rewritten.len() as u32) << 16) | u32::from(OP_NAME);
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

/// Walk a module instruction by instruction, replacing each one the rewriter
/// returns words for and dropping each one it returns no words for; reports how
/// many instructions it touched.
fn rewrite_spirv_instructions(
    spirv: &[u8],
    mut rewrite_instruction: impl FnMut(u16, &[u32]) -> Option<Vec<u32>>,
) -> (Vec<u8>, usize) {
    let words = spirv_module_words(spirv);
    let mut rewritten: Vec<u32> = words[..SPIRV_HEADER_WORD_COUNT].to_vec();
    let mut rewritten_instruction_count = 0;
    let mut at = SPIRV_HEADER_WORD_COUNT;
    while at < words.len() {
        let word_count = (words[at] >> 16) as usize;
        let opcode = (words[at] & 0xffff) as u16;
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
        .chunks_exact(4)
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
    while bytes.len() % 4 != 0 {
        bytes.push(0);
    }
    spirv_module_words(&bytes)
}
