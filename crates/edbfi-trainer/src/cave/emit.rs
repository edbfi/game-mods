// SPDX-License-Identifier: AGPL-3.0-or-later
//! The assembler wrapper the builders share: original decoding, a literal pool, game-instruction tagging and
//! the hand-off to the proof.
#![forbid(unsafe_code)]

use std::marker::PhantomData;
use std::num::NonZeroU32;

use iced_x86::code_asm::{CodeAssembler, CodeLabel, qword_ptr};
use iced_x86::{
    BlockEncoderOptions, Code, Decoder, DecoderOptions, FlowControl, IcedError, Instruction,
    MemoryOperand, OpKind, Register,
};

use crate::addr::{AddressError, CodeRange, Va};
use crate::profile::SiteId;

use super::proof::{self, Claim, ProofError};
use super::{Cave, Exit, HookError, Layout, Literal, PAGE, Reentry};

/// Why a builder stopped, before the site is known.
#[derive(Debug)]
pub(super) enum Fault {
    /// The original bytes are not the shape the hook expects.
    Original(&'static str),
    /// A profile value cannot be encoded.
    Unencodable(&'static str),
    /// An address does not fit.
    Address(AddressError),
    /// The assembler refused.
    Assembler(IcedError),
}

impl From<IcedError> for Fault {
    fn from(error: IcedError) -> Self {
        Self::Assembler(error)
    }
}

impl From<AddressError> for Fault {
    fn from(error: AddressError) -> Self {
        Self::Address(error)
    }
}

impl Fault {
    /// The error for `site`.
    pub(super) fn at(self, site: SiteId) -> HookError {
        match self {
            Self::Original(reason) => HookError::Original { site, reason },
            Self::Unencodable(reason) => HookError::Unencodable { site, reason },
            Self::Address(source) => HookError::Address { site, source },
            Self::Assembler(error) => HookError::Assembler {
                site,
                message: error.to_string(),
            },
        }
    }
}

/// The instructions of `original` decoded at `at`, the site's own address.
pub(super) fn decode(original: &[u8], at: Va) -> Result<Vec<Instruction>, Fault> {
    let mut decoder = Decoder::with_ip(64, original, at.get(), DecoderOptions::NONE);
    let mut instructions = Vec::new();
    while decoder.can_decode() {
        let instruction = decoder.decode();
        if instruction.is_invalid() {
            return Err(Fault::Original(
                "the bytes do not decode as whole instructions",
            ));
        }
        instructions.push(instruction);
    }
    Ok(instructions)
}

/// Whether `instruction` behaves the same at any address: it neither transfers control nor addresses
/// memory relative to its own position.
pub(super) fn position_independent(instruction: &Instruction) -> bool {
    instruction.flow_control() == FlowControl::Next && !instruction.is_ip_rel_memory_operand()
}

/// Whether `call` is `call qword [reg...]`: an indirect call through a memory operand with a base or index
/// register other than RSP and no segment override, which reads the same callee wherever it runs and
/// whatever the cave pushes first.
pub(super) fn indirect_call(call: &Instruction) -> bool {
    call.code() == Code::Call_rm64
        && call.op0_kind() == OpKind::Memory
        && !call.is_ip_rel_memory_operand()
        && call.segment_prefix() == Register::None
        && call.memory_base().full_register() != Register::RSP
        && (call.memory_base() != Register::None || call.memory_index() != Register::None)
}

/// Refuses an original holding a branch, call or RIP-relative operand.
pub(super) fn require_position_independent(instructions: &[Instruction]) -> Result<(), Fault> {
    if instructions.iter().all(position_independent) {
        Ok(())
    } else {
        Err(Fault::Original(
            "it holds a branch, call or RIP-relative operand the hook does not handle",
        ))
    }
}

/// The original's single instruction.
pub(super) fn single(instructions: &[Instruction]) -> Result<&Instruction, Fault> {
    match instructions {
        [only] => Ok(only),
        _ => Err(Fault::Original("it is not exactly one instruction")),
    }
}

/// `value` as a 32-bit displacement or immediate.
pub(super) fn imm32(value: u32, what: &'static str) -> Result<i32, Fault> {
    i32::try_from(value).map_err(|_| Fault::Unencodable(what))
}

/// A cave being assembled.
///
/// Instructions added through [`Emitter::asm`] are the cave's own; instructions added through
/// [`Emitter::game`] or [`Emitter::replay`] are the game's, copied from the site or standing in for it; an
/// instruction added through [`Emitter::effect`] is the hook's declared effect, such as the pressure floor's
/// store. The proof holds each to its own rules.
pub(super) struct Emitter {
    asm: CodeAssembler,
    pool: Vec<(u64, CodeLabel)>,
    game: Vec<usize>,
    effects: Vec<usize>,
}

impl Emitter {
    /// An empty 64-bit cave.
    pub(super) fn new() -> Result<Self, Fault> {
        Ok(Self {
            asm: CodeAssembler::new(64)?,
            pool: Vec::new(),
            game: Vec::new(),
            effects: Vec::new(),
        })
    }

    /// The assembler, for the cave's own instructions.
    pub(super) fn asm(&mut self) -> &mut CodeAssembler {
        &mut self.asm
    }

    /// A new label.
    pub(super) fn label(&mut self) -> CodeLabel {
        self.asm.create_label()
    }

    /// Binds `label` to the next instruction.
    pub(super) fn bind(&mut self, label: &mut CodeLabel) -> Result<(), Fault> {
        Ok(self.asm.set_label(label)?)
    }

    /// The pool entry holding `value`.
    pub(super) fn literal(&mut self, value: u64) -> CodeLabel {
        if let Some((_, label)) = self.pool.iter().find(|(held, _)| *held == value) {
            return *label;
        }
        let label = self.asm.create_label();
        self.pool.push((value, label));
        label
    }

    /// Game instructions emitted by `emit`.
    pub(super) fn game(
        &mut self,
        emit: impl FnOnce(&mut CodeAssembler) -> Result<(), IcedError>,
    ) -> Result<(), Fault> {
        let first = self.asm.instructions().len();
        emit(&mut self.asm)?;
        self.game.extend(first..self.asm.instructions().len());
        Ok(())
    }

    /// The hook's declared effect, emitted by `emit`: a cave instruction allowed one memory write.
    pub(super) fn effect(
        &mut self,
        emit: impl FnOnce(&mut CodeAssembler) -> Result<(), IcedError>,
    ) -> Result<(), Fault> {
        let first = self.asm.instructions().len();
        emit(&mut self.asm)?;
        self.effects.extend(first..self.asm.instructions().len());
        Ok(())
    }

    /// Re-emits decoded game instructions. The assembler keys labels on instruction addresses, so each copy
    /// drops the address it was decoded at.
    pub(super) fn replay(&mut self, instructions: &[Instruction]) -> Result<(), Fault> {
        self.game(|asm| {
            instructions.iter().try_for_each(|instruction| {
                let mut copy = *instruction;
                copy.set_ip(0);
                asm.add_instruction(copy)
            })
        })
    }

    /// `jmp qword [rip+literal]` to `target`.
    pub(super) fn jump(&mut self, target: Va) -> Result<(), Fault> {
        let literal = self.literal(target.get());
        Ok(self.asm.jmp(qword_ptr(literal))?)
    }

    /// Pushes `ret` and jumps to `callee`: the callee runs as if called from the site.
    pub(super) fn tail_call(&mut self, callee: Va, ret: Va) -> Result<(), Fault> {
        let ret = self.literal(ret.get());
        self.asm.push(qword_ptr(ret))?;
        self.jump(callee)
    }

    /// Pushes `ret` and jumps through the memory operand of `call`, an indirect call from the game: the callee
    /// runs as if called from the site.
    pub(super) fn tail_call_via(&mut self, call: &Instruction, ret: Va) -> Result<(), Fault> {
        if !indirect_call(call) {
            return Err(Fault::Original(
                "it is not an indirect call through a register-based memory operand",
            ));
        }
        let operand = MemoryOperand::with_base_index_scale_displ_size(
            call.memory_base(),
            call.memory_index(),
            call.memory_index_scale(),
            call.memory_displacement64().cast_signed(),
            call.memory_displ_size(),
        );
        let jump = Instruction::with1(Code::Jmp_rm64, operand)?;
        let ret = self.literal(ret.get());
        self.asm.push(qword_ptr(ret))?;
        self.game(|asm| asm.add_instruction(jump))
    }

    /// Assembles the cave at `cave`, followed by its literal pool. `exits` are the shape's declared exits.
    pub(super) fn finish(mut self, cave: Va, exits: Vec<Exit>) -> Result<Draft, Fault> {
        let code_instructions = self.asm.instructions().len();
        for (value, label) in &mut self.pool {
            self.asm.set_label(label)?;
            self.asm.dq(&[*value])?;
        }
        let result = self.asm.assemble_options(
            cave.get(),
            BlockEncoderOptions::RETURN_NEW_INSTRUCTION_OFFSETS,
        )?;
        let offsets = &result.inner.new_instruction_offsets;
        let code_len = match offsets.get(code_instructions) {
            Some(&offset) => offset,
            None => u32::try_from(result.inner.code_buffer.len())
                .map_err(|_| Fault::Unencodable("cave size"))?,
        };
        let literals = self
            .pool
            .iter()
            .map(|(value, label)| {
                let at = result.label_ip(label)?;
                let offset = u32::try_from(at.wrapping_sub(cave.get()))
                    .map_err(|_| Fault::Unencodable("literal offset"))?;
                Ok(Literal {
                    offset,
                    value: *value,
                })
            })
            .collect::<Result<_, Fault>>()?;
        let tagged = |indexes: &[usize]| {
            indexes
                .iter()
                .map(|&index| {
                    offsets
                        .get(index)
                        .copied()
                        .ok_or(Fault::Unencodable("instruction offset"))
                })
                .collect::<Result<Vec<u32>, Fault>>()
        };
        let game = tagged(&self.game)?;
        let effects = tagged(&self.effects)?;
        let bytes = result.inner.code_buffer;
        Ok(Draft {
            cave,
            bytes,
            code_len,
            literals,
            game,
            effects,
            exits,
        })
    }
}

/// An assembled cave that has not been proven.
#[derive(Debug)]
pub(super) struct Draft {
    pub(super) cave: Va,
    pub(super) bytes: Vec<u8>,
    pub(super) code_len: u32,
    pub(super) literals: Vec<Literal>,
    pub(super) game: Vec<u32>,
    pub(super) effects: Vec<u32>,
    pub(super) exits: Vec<Exit>,
}

impl Draft {
    /// The cave, once it fits a page and passes its proof as a cave of type `R`.
    pub(super) fn prove<R: Reentry>(
        self,
        site: SiteId,
        layout: Layout,
    ) -> Result<Cave<R>, HookError> {
        let len = self.bytes.len();
        let fits = u32::try_from(len)
            .ok()
            .filter(|&len| len <= PAGE)
            .and_then(NonZeroU32::new);
        let Some(len) = fits else {
            return Err(HookError::TooLarge { site, len });
        };
        let address = |source| HookError::Address { site, source };
        let range = CodeRange::new(self.cave, len).map_err(address)?;
        let code = NonZeroU32::new(self.code_len).ok_or(HookError::Proof {
            site,
            offset: 0,
            reason: ProofError::Undecodable,
        })?;
        let code = CodeRange::new(self.cave, code).map_err(address)?;
        let mut exits = self.exits;
        exits.sort_unstable();
        exits.dedup();
        let mut game = self.game;
        game.sort_unstable();
        let mut effects = self.effects;
        effects.sort_unstable();
        proof::prove(&Claim {
            address: self.cave,
            bytes: &self.bytes,
            code_len: self.code_len,
            literals: &self.literals,
            game: &game,
            effects: &effects,
            controls: layout.controls(),
            returns_into_cave: R::RETURNS_INTO_CAVE,
            exits: &exits,
        })
        .map_err(|(offset, reason)| HookError::Proof {
            site,
            offset,
            reason,
        })?;
        Ok(Cave {
            bytes: self.bytes,
            range,
            code,
            literals: self.literals,
            exits,
            reentry: PhantomData,
        })
    }
}
