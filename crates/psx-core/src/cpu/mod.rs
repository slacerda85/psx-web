//! CPU MIPS R3000A e seus coprocessadores.
//!
//! Referência: PSX-SPX — "CPU Specifications".

pub mod cop0;
pub mod instruction;
mod r3000a;

pub use cop0::{Cop0, Exception};
pub use instruction::Instruction;
pub use r3000a::{Cpu, RESET_VECTOR};
