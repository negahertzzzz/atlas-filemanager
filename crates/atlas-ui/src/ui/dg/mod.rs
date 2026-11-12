//! Digital Grove UI Kit for Atlas.
//!
//! Reusable immediate-mode primitives adhering to:
//! - Principle 2: Construction every frame.
//! - Principle 3: "The widget is a lie" (Composable functional helpers).
//! - Principle 4: Deterministic two-pass / rectangle slicing layout.
//! - Principle 8: Structural error elimination (saturating bounds).

pub mod cut;
pub mod input;
mod input_bridge;
pub(crate) mod menu;
pub(crate) mod menu_panel;
pub(crate) mod motion;
pub(crate) mod scroll;
pub(crate) mod table;

pub use cut::RectCut;
pub use input::DgTextInputState;
