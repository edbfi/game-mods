// SPDX-License-Identifier: AGPL-3.0-only
//! The proof every cave passes before a builder returns it.
//!
//! It decodes the cave's code independently of the assembler that wrote it, then walks every path from the
//! entry with an abstract stack. Each slot records what was pushed: the flags, a saved register, a literal
//! return address, the cave's scratch frame, or game data pushed by a copied game instruction. The walk holds:
//!
//! - **Control snapshot.** The control word's address appears once, in one `mov r64, imm64`, and every path
//!   reads through that register exactly once, with a 32-bit `mov` load at offset 0.
//! - **Registers and flags.** The cave's own instructions write a general register or the flags only while
//!   the value they replace is saved on the stack, and each `pop` restores the register that pushed it.
//! - **Memory.** The cave's own instructions write memory only inside the frame they reserved.
//! - **Game instructions.** A copied or substituted game instruction that uses the stack runs with no cave
//!   data on it, so it sees the stack the game had at the site.
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
    Code, Decoder, DecoderOptions, FlowControl, Instruction, InstructionInfoFactory, OpAccess,
    OpKind, Register,
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
    /// The cave addresses the stack outside its own frame.
    #[error("the cave addresses the stack outside its frame")]
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
    /// The control word's address is loaded more than once or held in the pool.
    #[error("the control word's address appears more than once")]
    ControlAddress,
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
    #[error("a call in a cave the game must never return into")]
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
}

impl State {
    /// Joins `other` into `self`. Stacks and load counts must agree; a register held on only one side, or held
    /// differently, becomes uncertain.
    fn merge(&mut self, other: &Self) -> Result<(), ProofError> {
        if self.stack != other.stack || self.loads != other.loads {
            return Err(ProofError::PathsDisagree);
        }
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
        if literal.value == claim.controls.get() {
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

    fn is_game(&self, instruction: &Instruction) -> bool {
        self.claim
            .game
            .binary_search(&self.offset(instruction))
            .is_ok()
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
        let game = self.is_game(instruction);
        let literal = self.literal(instruction)?;
        self.control_access(instruction, state)?;
        let stack_op = self.stack(instruction, game, literal, state)?;
        if !stack_op {
            self.frame_access(instruction, game, state)?;
            if !game {
                self.clobbers(instruction, state)?;
            }
        }
        self.track_controls(instruction, state)?;
        self.flow(instruction, game, literal, state)
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
            && instruction.memory_displacement64() == 0;
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

    /// Bounds explicit stack operands, and refuses any other write to RSP.
    fn frame_access(
        &mut self,
        instruction: &Instruction,
        game: bool,
        state: &State,
    ) -> Result<(), ProofError> {
        let transfers = matches!(
            instruction.flow_control(),
            FlowControl::Call | FlowControl::IndirectCall | FlowControl::Return
        );
        let info = self.info.info(instruction);
        let writes_rsp = info
            .used_registers()
            .iter()
            .any(|used| used.register().full_register() == Register::RSP && writes(used.access()));
        if writes_rsp && !transfers {
            return Err(ProofError::OutsideFrame);
        }
        let uses_stack = transfers
            || info
                .used_registers()
                .iter()
                .any(|used| used.register().full_register() == Register::RSP);
        if game {
            return if uses_stack && !state.game_stack() {
                Err(ProofError::GameStackHidden)
            } else {
                Ok(())
            };
        }
        if transfers {
            return Ok(());
        }
        for memory in info.used_memory() {
            if memory.base() == Register::RSP {
                if memory.index() != Register::None {
                    return Err(ProofError::OutsideFrame);
                }
                let start = memory.displacement();
                let end = start.checked_add(memory.memory_size().size() as u64);
                let inside = end.is_some_and(|end| end <= state.frame());
                if !inside {
                    return Err(ProofError::OutsideFrame);
                }
            } else if writes(memory.access()) {
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
            FlowControl::IndirectBranch if instruction.code() == Code::Jmp_rm64 => {
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
