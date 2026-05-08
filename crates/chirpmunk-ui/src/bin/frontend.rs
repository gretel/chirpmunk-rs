// SPDX-License-Identifier: GPL-3.0-only

#[cfg(not(target_arch = "wasm32"))]
pub fn main() {
    eprintln!(
        "chirpmunk-ui is a WASM-only frontend; build via `trunk build --release` (target: wasm32-unknown-unknown)."
    );
}

#[cfg(target_arch = "wasm32")]
pub fn main() {
    chirpmunk_ui::frontend::frontend();
}
