// SPDX-License-Identifier: AGPL-3.0-only
//! The proof every cave passes before a builder returns it.
//!
//! It decodes the cave's code independently of the assembler that wrote it, then walks every path from the
//! entry with an abstract stack. Each slot records what was pushed: the flags, a saved register, a literal
//! return address, the cave's scratch frame, or game data pushed by a copied game instruction. The walk holds:
//!
//! - **Control snapshot.** The control word's address appears once, in one `mov r64, imm64`, and every path
//!   reads through that register exactly once, with a 32-bit `mov` load at offset 0.
//!   No other constant lies within [`NEAR`] bytes of the control word, the cave's own memory operands have
//!   displacements under [`NEAR`], and no instruction uses an absolute or segment-overridden operand to
//!   read it, so no constant base plus displacement reaches it.
//! - **Registers and flags.** The cave's own instructions write a general register or the flags only while
//!   the value they replace is saved on the stack, and each `pop` restores the register that pushed it.
//! - **Memory.** The cave's own instructions write memory only inside the frame they reserved, and touch the
//!   stack only through push, pop, `add`/`sub rsp` and bounded `[rsp+disp]` operands. The one exception is
//!   the hook's declared effect: at most one instruction per path, such as the pressure floor's store.
//! - **Game instructions.** A copied or substituted game instruction runs with no cave data on the stack,
//!   so with every register, the flags and the stack as the game had them, and changes RSP only by a push,
//!   pop, `add`/`sub rsp` or call. The only cave data allowed is a literal return address under the
//!   indirect jump, through an operand not based on RSP, that stands in for the game's own call.
//! - **Exits.** Every path ends in an exit the hook shape declares, with nothing of the cave's on the stack
//!   except, for a call, the literal return address. Every declared exit is reachable.
//! - **Literals.** The pool sits after the code; the code never falls into it, and every RIP-relative operand
//!   is a `push` or `jmp` of one whole pool entry. Every entry is used.
//! - **Reentry.** A call appears exactly when the cave's type says the game returns into it.
//!
//! Branches only go forward, so the paths form a DAG and one pass in address order visits each instruction
//! after all of its predecessors. Where paths join, their abstract states must be equal.
#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use iced_x86::{
    Code, Decoder, DecoderOptions, FlowControl, Instruction, InstructionInfoFactory, Mnemonic,
    OpAccess, OpKind, Register,
};

use crate::addr::Va;

use super::{Callee, Exit, Literal};

/// Why a cave failed its proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ProofError {
    /// Bytes in the code do not decode as whole instructions.
    #[error("the code does not decode as whole instructions")]
    Undecodable,
    /// The literal pool is not a run of 8-byte entries after the code holding the recorded values.
    #[error("the literal pool is malformed")]
    LiteralPool,
    /// A pool entry no instruction uses.
    #[error("a literal is never used")]
    UnusedLiteral,
    /// A RIP-relative operand that is not a `push` or `jmp` of one whole pool entry.
    #[error("a RIP-relative operand does not name a literal")]
    StrayRipRelative,
    /// A branch goes backwards; caves have no loops.
    #[error("a branch goes backwards")]
    BackwardBranch,
    /// A branch lands outside the code or inside an instruction.
    #[error("a branch lands outside the code or inside an instruction")]
    BranchTarget,
    /// An instruction no path reaches.
    #[error("unreachable code")]
    Unreachable,
    /// Two paths reach an instruction with different stacks or control state.
    #[error("paths join with different states")]
    PathsDisagree,
    /// The last instruction falls through into the literal pool.
    #[error("the code falls through into the literal pool")]
    FallsIntoLiterals,
    /// A kind of control transfer caves never use.
    #[error("an unsupported control transfer")]
    UnsupportedFlow,
    /// A stack adjustment caves never use.
    #[error("an unsupported stack operation")]
    UnsupportedStack,
    /// A `pop` or `popf` that does not restore what the matching push saved.
    #[error("a pop does not restore the value its push saved")]
    WrongRestore,
    /// The cave addresses the stack outside its own frame, or an instruction moves RSP other than by push,
    /// pop, `add`/`sub rsp` or a call.
    #[error("the stack is addressed outside the cave's frame or RSP is moved")]
    OutsideFrame,
    /// A game instruction uses the stack while the cave's data is on it.
    #[error("a game instruction runs with the cave's data on the stack")]
    GameStackHidden,
    /// The cave writes a register it has not saved.
    #[error("the cave writes {0:?} without saving it")]
    UnsavedRegister(Register),
    /// The cave changes the flags without saving them.
    #[error("the cave changes the flags without saving them")]
    UnsavedFlags,
    /// The cave writes game memory.
    #[error("the cave writes game memory")]
    MemoryWrite,
    /// The control word's address is loaded more than once, or another constant lies near it.
    #[error(
        "the control word's address appears more than once, or another constant lies within 64 KiB of it"
    )]
    ControlAddress,
    /// A memory operand with an FS or GS override, which adds a base the proof cannot see.
    #[error("a segment-overridden memory operand")]
    SegmentOverride,
    /// A memory operand with neither base nor index register.
    #[error("an absolute memory operand")]
    AbsoluteAddress,
    /// A cave memory operand with a displacement of 64 KiB or more.
    #[error("a cave memory operand has a large displacement")]
    FarDisplacement,
    /// More than one declared effect on a path.
    #[error("a path has more than one declared effect")]
    TooManyEffects,
    /// The control word is accessed other than by one aligned 32-bit load.
    #[error("the control word is accessed other than by a 32-bit load")]
    ControlAccess,
    /// A path reads the control word other than exactly once.
    #[error("a path reads the control word {0} times")]
    ControlLoads(u8),
    /// A path leaves with the cave's data still on the stack.
    #[error("a path leaves with the stack unbalanced")]
    UnbalancedExit,
    /// A path leaves somewhere the hook shape does not declare.
    #[error("undeclared exit: {0}")]
    UndeclaredExit(Exit),
    /// A declared exit no path takes.
    #[error("no path takes the declared exit: {0}")]
    MissingExit(Exit),
    /// A call in a cave typed as never returned into, or one that is not a game instruction.
    #[error("a call in a cave the game must never return into, or one that is not the game's")]
    UnexpectedCall,
    /// A cave typed as returned into holds no call.
    #[error("a cave typed as returned into holds no call")]
    MissingCall,
}

/// What a builder claims about a cave.
pub(super) struct Claim<'a> {
    /// Where the cave is.
    pub(super) address: Va,
    /// Its bytes: code, then the literal pool.
    pub(super) bytes: &'a [u8],
    /// Where the pool starts.
    pub(super) code_len: u32,
    /// The pool's entries.
    pub(super) literals: &'a [Literal],
    /// Offsets of game instructions, sorted.
    pub(super) game: &'a [u32],
    /// Offsets of the hook's declared effects, sorted.
    pub(super) effects: &'a [u32],
    /// The control word.
    pub(super) controls: Va,
    /// Whether the cave is typed as returned into.
    pub(super) returns_into_cave: bool,
    /// The shape's exits, sorted and unique.
    pub(super) exits: &'a [Exit],
}

/// A proof failure and the offset of the instruction at fault.
type Failure = (u32, ProofError);

/// What a pushed stack slot holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    /// The flags, saved by the cave.
    Flags,
    /// A register, saved by the cave.
    Saved(Register),
    /// A return address from the pool.
    Literal(u64),
    /// Eight bytes of the cave's scratch frame.
    Frame,
    /// Data a game instruction pushed.
    Game,
}

/// The register that held the control word's address when last written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Held {
    register: Register,
    /// `false` after a join with a path where the register was overwritten: then it may or may not hold the
    /// address, so any read of it is refused.
    certain: bool,
}

/// The abstract machine state on one path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct State {
    stack: Vec<Slot>,
    controls: Option<Held>,
    /// Control-word loads so far.
    loads: u8,
    /// Declared effects so far.
    effects: u8,
}

impl State {
    /// Joins `other` into `self`. Stacks and load counts must agree; a register held on only one side, or held
    /// differently, becomes uncertain.
    fn merge(&mut self, other: &Self) -> Result<(), ProofError> {
        if self.stack != other.stack || self.loads != other.loads {
            return Err(ProofError::PathsDisagree);
        }
        // An effect happens on some paths and not others; "at most one" holds for the larger count.
        self.effects = self.effects.max(other.effects);
        if self.controls != other.controls {
            let held = self.controls.or(other.controls);
            if let (Some(a), Some(b)) = (self.controls, other.controls)
                && a.register != b.register
            {
                return Err(ProofError::PathsDisagree);
            }
            self.controls = held.map(|held| Held {
                certain: false,
                ..held
            });
        }
        Ok(())
    }

    fn saved(&self, register: Register) -> bool {
        self.stack.contains(&Slot::Saved(register))
    }

    fn flags_saved(&self) -> bool {
        self.stack.contains(&Slot::Flags)
    }

    /// Whether only game data is on the stack, so a game instruction sees the stack it had at the site.
    fn game_stack(&self) -> bool {
        self.stack.iter().all(|slot| *slot == Slot::Game)
    }

    /// Bytes of scratch frame on top of the stack.
    fn frame(&self) -> u64 {
        let slots = self
            .stack
            .iter()
            .rev()
            .take_while(|slot| **slot == Slot::Frame)
            .count();
        slots as u64 * 8
    }

    fn pop(&mut self, expected: Slot) -> Result<(), ProofError> {
        match self.stack.pop() {
            Some(slot) if slot == expected => Ok(()),
            _ => Err(ProofError::WrongRestore),
        }
    }
}

/// Where control goes after an instruction.
enum Flow {
    /// The next instruction.
    Next,
    /// A branch target and, for a conditional branch, the next instruction too.
    Branch { target: u64, falls_through: bool },
    /// Out of the cave.
    Exit(Exit),
}

/// Checks `claim`, returning the offset of the first instruction at fault.
pub(super) fn prove(claim: &Claim<'_>) -> Result<(), Failure> {
    check_pool(claim)?;
    let code = decode(claim)?;
    let mut walk = Walk {
        claim,
        info: InstructionInfoFactory::new(),
        controls_seen: false,
        call_seen: false,
        literals_used: BTreeSet::new(),
        exits: BTreeSet::new(),
    };
    let mut incoming: Vec<Option<State>> = vec![None; code.len()];
    if let Some(entry) = incoming.first_mut() {
        *entry = Some(State::default());
    }
    for (index, instruction) in code.iter().enumerate() {
        let offset = walk.offset(instruction);
        let fail = |reason| (offset, reason);
        let mut state = incoming[index]
            .take()
            .ok_or(fail(ProofError::Unreachable))?;
        match walk.step(instruction, &mut state).map_err(fail)? {
            Flow::Next => join(&mut incoming, index, index + 1, state).map_err(fail)?,
            Flow::Branch {
                target,
                falls_through,
            } => {
                let target = code
                    .binary_search_by_key(&target, Instruction::ip)
                    .map_err(|_| fail(ProofError::BranchTarget))?;
                if falls_through {
                    join(&mut incoming, index, index + 1, state.clone()).map_err(fail)?;
                }
                join(&mut incoming, index, target, state).map_err(fail)?;
            }
            Flow::Exit(exit) => {
                if state.loads != 1 {
                    return Err(fail(ProofError::ControlLoads(state.loads)));
                }
                if !claim.exits.contains(&exit) {
                    return Err(fail(ProofError::UndeclaredExit(exit)));
                }
                walk.exits.insert(exit);
            }
        }
    }
    if let Some(unused) = claim
        .literals
        .iter()
        .find(|literal| !walk.literals_used.contains(&literal.offset))
    {
        return Err((unused.offset, ProofError::UnusedLiteral));
    }
    if let Some(missing) = claim.exits.iter().find(|exit| !walk.exits.contains(exit)) {
        return Err((0, ProofError::MissingExit(*missing)));
    }
    if claim.returns_into_cave && !walk.call_seen {
        return Err((0, ProofError::MissingCall));
    }
    Ok(())
}

/// Passes `state` from instruction `from` to instruction `to`, which must lie ahead and inside the code.
fn join(
    incoming: &mut [Option<State>],
    from: usize,
    to: usize,
    state: State,
) -> Result<(), ProofError> {
    if to <= from {
        return Err(ProofError::BackwardBranch);
    }
    match incoming.get_mut(to) {
        None => Err(ProofError::FallsIntoLiterals),
        Some(Some(existing)) => existing.merge(&state),
        Some(slot) => {
            *slot = Some(state);
            Ok(())
        }
    }
}

/// The pool is a run of 8-byte entries from the end of the code to the end of the cave, each holding its
/// recorded value, and the control word's address is never one of them.
fn check_pool(claim: &Claim<'_>) -> Result<(), Failure> {
    let mut expected = claim.code_len;
    for literal in claim.literals {
        let at = literal.offset as usize;
        let held = claim
            .bytes
            .get(at..at + 8)
            .and_then(|bytes| bytes.try_into().ok())
            .map(u64::from_le_bytes);
        if literal.offset != expected || held != Some(literal.value) {
            return Err((literal.offset, ProofError::LiteralPool));
        }
        if near_controls(literal.value, claim.controls) {
            return Err((literal.offset, ProofError::ControlAddress));
        }
        expected += 8;
    }
    if expected as usize == claim.bytes.len() {
        Ok(())
    } else {
        Err((expected, ProofError::LiteralPool))
    }
}

/// The code's instructions, decoded on their own: the pool is never decoded.
fn decode(claim: &Claim<'_>) -> Result<Vec<Instruction>, Failure> {
    let code = claim
        .bytes
        .get(..claim.code_len as usize)
        .ok_or((0, ProofError::LiteralPool))?;
    let mut decoder = Decoder::with_ip(64, code, claim.address.get(), DecoderOptions::NONE);
    let mut instructions = Vec::new();
    while decoder.can_decode() {
        let offset = decoder.position() as u32;
        let instruction = decoder.decode();
        if instruction.is_invalid() {
            return Err((offset, ProofError::Undecodable));
        }
        instructions.push(instruction);
    }
    if instructions.is_empty() {
        return Err((0, ProofError::Undecodable));
    }
    Ok(instructions)
}

/// The walk's context, shared by every path.
struct Walk<'c> {
    claim: &'c Claim<'c>,
    info: InstructionInfoFactory,
    controls_seen: bool,
    call_seen: bool,
    literals_used: BTreeSet<u32>,
    exits: BTreeSet<Exit>,
}

impl Walk<'_> {
    fn offset(&self, instruction: &Instruction) -> u32 {
        // Decoding started at the cave's address and covered at most a page.
        instruction.ip().wrapping_sub(self.claim.address.get()) as u32
    }

    fn role(&self, instruction: &Instruction) -> Role {
        let offset = self.offset(instruction);
        if self.claim.game.binary_search(&offset).is_ok() {
            Role::Game
        } else if self.claim.effects.binary_search(&offset).is_ok() {
            Role::Effect
        } else {
            Role::Cave
        }
    }

    /// The pool entry a RIP-relative operand names.
    fn literal(&mut self, instruction: &Instruction) -> Result<Option<u64>, ProofError> {
        if !instruction.is_ip_rel_memory_operand() {
            return Ok(None);
        }
        let target = instruction.ip_rel_memory_address();
        let whole_entry = matches!(instruction.code(), Code::Push_rm64 | Code::Jmp_rm64);
        let literal = self
            .claim
            .literals
            .iter()
            .find(|literal| self.claim.address.get() + u64::from(literal.offset) == target)
            .filter(|_| whole_entry)
            .ok_or(ProofError::StrayRipRelative)?;
        self.literals_used.insert(literal.offset);
        Ok(Some(literal.value))
    }

    /// Applies `instruction` to `state` and says where control goes.
    fn step(&mut self, instruction: &Instruction, state: &mut State) -> Result<Flow, ProofError> {
        let role = self.role(instruction);
        let game = role == Role::Game;
        let literal = self.literal(instruction)?;
        self.constants(instruction)?;
        self.control_access(instruction, state)?;
        let stack_op = self.stack(instruction, game, literal, state)?;
        if !stack_op {
            if game {
                Self::game_stack(instruction, state)?;
                self.game_keeps_rsp(instruction)?;
            } else {
                self.frame_access(instruction, role, state)?;
                self.clobbers(instruction, state)?;
            }
        }
        if role == Role::Effect {
            state.effects += 1;
            if state.effects > 1 {
                return Err(ProofError::TooManyEffects);
            }
        }
        self.track_controls(instruction, state)?;
        self.flow(instruction, game, literal, state)
    }

    /// Refuses absolute and segment-overridden memory operands, and constants other than the counted `mov`
    /// that lie near the control word.
    fn constants(&self, instruction: &Instruction) -> Result<(), ProofError> {
        let controls = self.claim.controls;
        let has_memory = (0..instruction.op_count())
            .any(|operand| instruction.op_kind(operand) == OpKind::Memory);
        if has_memory && instruction.segment_prefix() != Register::None {
            return Err(ProofError::SegmentOverride);
        }
        for operand in 0..instruction.op_count() {
            let value = match instruction.op_kind(operand) {
                OpKind::Memory if instruction.is_ip_rel_memory_operand() => continue,
                OpKind::Memory => {
                    if instruction.memory_base() == Register::None
                        && instruction.memory_index() == Register::None
                    {
                        return Err(ProofError::AbsoluteAddress);
                    }
                    instruction.memory_displacement64()
                }
                OpKind::Immediate8
                | OpKind::Immediate8_2nd
                | OpKind::Immediate16
                | OpKind::Immediate32
                | OpKind::Immediate64
                | OpKind::Immediate8to16
                | OpKind::Immediate8to32
                | OpKind::Immediate8to64
                | OpKind::Immediate32to64 => {
                    if instruction.code() == Code::Mov_r64_imm64
                        && instruction.immediate64() == controls.get()
                    {
                        continue;
                    }
                    instruction.immediate(operand)
                }
                _ => continue,
            };
            if near_controls(value, controls) {
                return Err(ProofError::ControlAddress);
            }
        }
        Ok(())
    }

    /// Refuses a game instruction other than a push, pop, `add`/`sub rsp` or call that writes RSP: the walk
    /// could no longer model the stack after it.
    fn game_keeps_rsp(&mut self, instruction: &Instruction) -> Result<(), ProofError> {
        let transfers = matches!(
            instruction.flow_control(),
            FlowControl::Call | FlowControl::IndirectCall | FlowControl::Return
        );
        let writes_rsp = self
            .info
            .info(instruction)
            .used_registers()
            .iter()
            .any(|used| used.register().full_register() == Register::RSP && writes(used.access()));
        if writes_rsp && !transfers {
            Err(ProofError::OutsideFrame)
        } else {
            Ok(())
        }
    }

    /// A game instruction must see the game's own stack. The indirect jump standing in for the game's call
    /// may have the literal return address on top.
    fn game_stack(instruction: &Instruction, state: &State) -> Result<(), ProofError> {
        let stack = match (instruction.code(), state.stack.split_last()) {
            // Through an RSP-based operand the pushed literal would shift what the jump reads.
            (Code::Jmp_rm64, Some((Slot::Literal(_), below)))
                if instruction.memory_base().full_register() != Register::RSP =>
            {
                below
            }
            _ => state.stack.as_slice(),
        };
        if stack.iter().all(|slot| *slot == Slot::Game) {
            Ok(())
        } else {
            Err(ProofError::GameStackHidden)
        }
    }

    /// Counts control-word loads; refuses any other use of the register holding its address.
    fn control_access(
        &mut self,
        instruction: &Instruction,
        state: &mut State,
    ) -> Result<(), ProofError> {
        let Some(Held {
            register: controls,
            certain,
        }) = state.controls
        else {
            return Ok(());
        };
        let load = certain
            && instruction.code() == Code::Mov_r32_rm32
            && instruction.op1_kind() == OpKind::Memory
            && instruction.memory_base() == controls
            && instruction.memory_index() == Register::None
            && instruction.memory_displacement64() == 0
            && instruction.segment_prefix() == Register::None;
        if load {
            state.loads = state.loads.saturating_add(1);
            return Ok(());
        }
        let info = self.info.info(instruction);
        let reads = info
            .used_registers()
            .iter()
            .any(|used| used.register().full_register() == controls && reads(used.access()));
        if reads {
            return Err(ProofError::ControlAccess);
        }
        Ok(())
    }

    /// Applies a push, pop or stack adjustment. Returns whether `instruction` was one.
    fn stack(
        &self,
        instruction: &Instruction,
        game: bool,
        literal: Option<u64>,
        state: &mut State,
    ) -> Result<bool, ProofError> {
        let own = |slot| if game { Slot::Game } else { slot };
        let names_rsp = matches!(instruction.code(), Code::Push_r64 | Code::Pop_r64)
            && instruction.op0_register() == Register::RSP;
        if names_rsp {
            return Err(ProofError::OutsideFrame);
        }
        match instruction.code() {
            Code::Pushfq => state.stack.push(own(Slot::Flags)),
            Code::Push_r64 => state
                .stack
                .push(own(Slot::Saved(instruction.op0_register()))),
            Code::Push_rm64 => match literal {
                Some(value) => state.stack.push(Slot::Literal(value)),
                None if game => state.stack.push(Slot::Game),
                None => return Err(ProofError::UnsupportedStack),
            },
            Code::Popfq => state.pop(own(Slot::Flags))?,
            Code::Pop_r64 => state.pop(own(Slot::Saved(instruction.op0_register())))?,
            Code::Sub_rm64_imm8
            | Code::Sub_rm64_imm32
            | Code::Add_rm64_imm8
            | Code::Add_rm64_imm32
                if instruction.op0_kind() == OpKind::Register
                    && instruction.op0_register() == Register::RSP =>
            {
                let bytes = instruction.immediate(1);
                if bytes == 0 || !bytes.is_multiple_of(8) || bytes > 0x100 {
                    return Err(ProofError::UnsupportedStack);
                }
                // The adjustment changes the arithmetic flags.
                if !game && !state.flags_saved() {
                    return Err(ProofError::UnsavedFlags);
                }
                let slot = own(Slot::Frame);
                for _ in 0..bytes / 8 {
                    if matches!(
                        instruction.code(),
                        Code::Sub_rm64_imm8 | Code::Sub_rm64_imm32
                    ) {
                        state.stack.push(slot);
                    } else {
                        state.pop(slot)?;
                    }
                }
            }
            _ => return Ok(false),
        }
        if game && !state.game_stack() {
            return Err(ProofError::GameStackHidden);
        }
        Ok(true)
    }

    /// Bounds the stack operands of a cave instruction or declared effect, and refuses any other use of RSP
    /// and, outside a declared effect, any memory write.
    fn frame_access(
        &mut self,
        instruction: &Instruction,
        role: Role,
        state: &State,
    ) -> Result<(), ProofError> {
        let transfers = matches!(
            instruction.flow_control(),
            FlowControl::Call | FlowControl::IndirectCall | FlowControl::Return
        );
        if transfers {
            return Ok(());
        }
        let names_rsp = (0..instruction.op_count()).any(|operand| {
            instruction.op_kind(operand) == OpKind::Register
                && instruction.op_register(operand).full_register() == Register::RSP
        });
        let has_memory = (0..instruction.op_count())
            .any(|operand| instruction.op_kind(operand) == OpKind::Memory);
        let aliases_rsp = has_memory
            && instruction.memory_base().full_register() == Register::RSP
            && (instruction.memory_base() != Register::RSP
                || instruction.mnemonic() == Mnemonic::Lea);
        if names_rsp || aliases_rsp {
            return Err(ProofError::OutsideFrame);
        }
        let far = has_memory
            && !instruction.is_ip_rel_memory_operand()
            && instruction
                .memory_displacement64()
                .cast_signed()
                .unsigned_abs()
                >= NEAR;
        if far {
            return Err(ProofError::FarDisplacement);
        }
        let info = self.info.info(instruction);
        let writes_rsp = info
            .used_registers()
            .iter()
            .any(|used| used.register().full_register() == Register::RSP && writes(used.access()));
        if writes_rsp {
            return Err(ProofError::OutsideFrame);
        }
        for memory in info.used_memory() {
            if memory.base() == Register::RSP {
                let size = memory.memory_size().size() as u64;
                let end = memory.displacement().checked_add(size);
                let inside = memory.index() == Register::None
                    && size > 0
                    && end.is_some_and(|end| end <= state.frame());
                if !inside {
                    return Err(ProofError::OutsideFrame);
                }
            } else if writes(memory.access()) && role != Role::Effect {
                return Err(ProofError::MemoryWrite);
            }
        }
        Ok(())
    }

    /// Refuses a cave instruction that writes an unsaved register or the unsaved flags.
    fn clobbers(&mut self, instruction: &Instruction, state: &State) -> Result<(), ProofError> {
        if instruction.rflags_modified() != 0 && !state.flags_saved() {
            return Err(ProofError::UnsavedFlags);
        }
        let info = self.info.info(instruction);
        for used in info.used_registers() {
            let register = used.register().full_register();
            if register.is_gpr64()
                && register != Register::RSP
                && writes(used.access())
                && !state.saved(register)
            {
                return Err(ProofError::UnsavedRegister(register));
            }
        }
        Ok(())
    }

    /// Notes where the control word's address is loaded and when its register is overwritten.
    fn track_controls(
        &mut self,
        instruction: &Instruction,
        state: &mut State,
    ) -> Result<(), ProofError> {
        let controls = self.claim.controls.get();
        if instruction.code() == Code::Mov_r64_imm64 && instruction.immediate64() == controls {
            if self.controls_seen {
                return Err(ProofError::ControlAddress);
            }
            self.controls_seen = true;
            state.controls = Some(Held {
                register: instruction.op0_register(),
                certain: true,
            });
            return Ok(());
        }
        let Some(Held { register: held, .. }) = state.controls else {
            return Ok(());
        };
        let calls = matches!(
            instruction.flow_control(),
            FlowControl::Call | FlowControl::IndirectCall
        );
        let info = self.info.info(instruction);
        let overwritten = calls
            || info
                .used_registers()
                .iter()
                .any(|used| used.register().full_register() == held && writes(used.access()));
        if overwritten {
            state.controls = None;
        }
        Ok(())
    }

    /// Where control goes after `instruction`.
    fn flow(
        &mut self,
        instruction: &Instruction,
        game: bool,
        literal: Option<u64>,
        state: &State,
    ) -> Result<Flow, ProofError> {
        let near = || match instruction.op0_kind() {
            OpKind::NearBranch64 => Ok(instruction.near_branch_target()),
            _ => Err(ProofError::UnsupportedFlow),
        };
        Ok(match instruction.flow_control() {
            FlowControl::Next => Flow::Next,
            FlowControl::ConditionalBranch => Flow::Branch {
                target: near()?,
                falls_through: true,
            },
            FlowControl::UnconditionalBranch => Flow::Branch {
                target: near()?,
                falls_through: false,
            },
            FlowControl::IndirectBranch
                if instruction.code() == Code::Jmp_rm64
                    && instruction.op0_kind() == OpKind::Memory =>
            {
                Flow::Exit(Self::jump_exit(literal, game, state)?)
            }
            FlowControl::Return if instruction.code() == Code::Retnq => {
                if !state.stack.is_empty() {
                    return Err(ProofError::UnbalancedExit);
                }
                Flow::Exit(Exit::Return)
            }
            FlowControl::Call | FlowControl::IndirectCall => {
                if !game || !self.claim.returns_into_cave {
                    return Err(ProofError::UnexpectedCall);
                }
                self.call_seen = true;
                Flow::Next
            }
            _ => return Err(ProofError::UnsupportedFlow),
        })
    }

    /// The exit a `jmp qword [..]` takes: a jump with the game's stack, or a call with exactly a literal
    /// return address pushed on it.
    fn jump_exit(literal: Option<u64>, game: bool, state: &State) -> Result<Exit, ProofError> {
        let (top, below) = match state.stack.split_last() {
            Some((Slot::Literal(ret), below)) => (Some(Va::new(*ret)), below),
            _ => (None, state.stack.as_slice()),
        };
        if !below.iter().all(|slot| *slot == Slot::Game) {
            return Err(ProofError::UnbalancedExit);
        }
        match (literal, top) {
            (Some(target), None) => Ok(Exit::Jump(Va::new(target))),
            (Some(callee), Some(ret)) => Ok(Exit::TailCall {
                callee: Callee::Fixed(Va::new(callee)),
                ret,
            }),
            // An indirect jump through game memory stands in for the game's own call.
            (None, Some(ret)) if game => Ok(Exit::TailCall {
                callee: Callee::Indirect,
                ret,
            }),
            _ => Err(ProofError::UnsupportedFlow),
        }
    }
}

/// How close to the control word a constant may come, and the largest displacement a cave's own memory
/// operand may use.
const NEAR: u64 = 0x1_0000;

/// The kind of instruction, as the builder tagged it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    /// The cave's own scaffolding.
    Cave,
    /// The game's instruction, copied or substituted.
    Game,
    /// The hook's declared effect: a cave instruction allowed one memory write.
    Effect,
}

/// Whether `value` lies within [`NEAR`] bytes of the control word.
fn near_controls(value: u64, controls: Va) -> bool {
    value.abs_diff(controls.get()) < NEAR
}

/// Whether an access reads.
fn reads(access: OpAccess) -> bool {
    matches!(
        access,
        OpAccess::Read | OpAccess::CondRead | OpAccess::ReadWrite | OpAccess::ReadCondWrite
    )
}

/// Whether an access writes.
fn writes(access: OpAccess) -> bool {
    matches!(
        access,
        OpAccess::Write | OpAccess::CondWrite | OpAccess::ReadWrite | OpAccess::ReadCondWrite
    )
}
