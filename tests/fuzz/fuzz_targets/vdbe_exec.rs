// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
#![no_main]

use libfuzzer_sys::fuzz_target;

use sqlite_rs::vdbe::{execute, Collation, Instruction, Opcode, Program, P4};

// Discharges spec 009's no-panic-totality obligation (#89): `execute()`
// must never panic on an arbitrary instruction stream, including
// out-of-range register indices, malformed P4 operands, jumps to
// nonexistent addresses, and adversarial loops (bounded by `MAX_STEPS`,
// surfaced as a structured `Err` rather than a hang).
fuzz_target!(|data: &[u8]| {
    let program = decode_program(data);
    let _ = execute(&program);
});

const OPCODES: &[Opcode] = &[
    Opcode::Init,
    Opcode::Goto,
    Opcode::Once,
    Opcode::BeginSubrtn,
    Opcode::Return,
    Opcode::Halt,
    Opcode::Transaction,
    Opcode::IfNot,
    Opcode::IfNotZero,
    Opcode::IfPos,
    Opcode::DecrJumpZero,
    Opcode::IsNull,
    Opcode::NotNull,
    Opcode::MustBeInt,
    Opcode::OffsetLimit,
    Opcode::Eq,
    Opcode::Ge,
    Opcode::Gt,
    Opcode::Le,
    Opcode::Lt,
    Opcode::RealAffinity,
    Opcode::Add,
    Opcode::Subtract,
    Opcode::Multiply,
    Opcode::Divide,
    Opcode::Remainder,
    Opcode::Integer,
    Opcode::String8,
    Opcode::MakeRecord,
    Opcode::ResultRow,
    // Deliberately included though unimplemented by this VM (#90/#91
    // territory): dispatch must return `ExecError::Unimplemented`, not
    // panic, for cursor/function/sorter opcodes too.
    Opcode::OpenRead,
    Opcode::Function,
    Opcode::SorterOpen,
];

fn decode_program(data: &[u8]) -> Program {
    let mut instructions = Vec::new();
    let mut rest = data;
    // Cap instruction count independent of MAX_STEPS, so a huge input
    // can't spend all its time just building the program.
    while instructions.len() < 256 {
        let Some(instr) = decode_instruction(&mut rest) else {
            break;
        };
        instructions.push(instr);
    }
    Program::new(instructions)
}

fn decode_instruction(data: &mut &[u8]) -> Option<Instruction> {
    let opcode_byte = take_u8(data)?;
    #[allow(clippy::indexing_slicing)]
    let opcode = OPCODES[(opcode_byte as usize) % OPCODES.len()];
    let p1 = take_i32(data)?;
    let p2 = take_i32(data)?;
    let p3 = take_i32(data)?;
    let p4_tag = take_u8(data)?;
    let p4 = match p4_tag % 4 {
        0 => P4::None,
        1 => P4::Int(i64::from(take_i32(data)?)),
        2 => {
            let len = usize::from(take_u8(data)?).min(data.len());
            let (bytes, remaining) = data.split_at(len);
            *data = remaining;
            P4::Str(String::from_utf8_lossy(bytes).into_owned())
        }
        _ => P4::CollSeq {
            collation: match take_u8(data)? % 3 {
                0 => Collation::Binary,
                1 => Collation::NoCase,
                _ => Collation::RTrim,
            },
            affinity: take_u8(data)?,
        },
    };
    Some(Instruction::with_p4(opcode, p1, p2, p3, p4))
}

fn take_u8(data: &mut &[u8]) -> Option<u8> {
    let (&byte, rest) = data.split_first()?;
    *data = rest;
    Some(byte)
}

fn take_i32(data: &mut &[u8]) -> Option<i32> {
    if data.len() < 4 {
        return None;
    }
    let (head, rest) = data.split_at(4);
    let mut buf = [0u8; 4];
    buf.copy_from_slice(head);
    *data = rest;
    Some(i32::from_le_bytes(buf))
}
