// SPDX-License-Identifier: Apache-2.0

//! `liberty-to-cellir`: generate a cell-model-IR descriptor from a Liberty
//! library (ADR 0019 D6 — the converter crate).
//!
//! Build/CI-time tool, mirroring `opensta-to-ir`. Not depended on by
//! jacquard core. Three modules:
//!
//! - [`function`] (Module A): Liberty `function` expression -> single-output
//!   [`cell_model_ir::CombLogic`] AIG. The crux, TDD-gated against an
//!   independent reference evaluator.
//! - [`convert`] (Module B): walk a parsed Liberty tree -> [`cell_model_ir::CellModelIr`].
//! - [`crosscheck`] (Module C): D6 eval-based cross-check against the PDK's
//!   `functional.v` models.

pub mod convert;
pub mod crosscheck;
pub mod function;
pub mod sequential;
pub mod timing;
