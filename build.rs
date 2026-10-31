// SPDX-License-Identifier: MIT
fn main() {
    // The optional, explicitly configured QQ research signer resolves these two
    // registration symbols in its host. This exports no browser/Node runtime.
    println!(
        "cargo:rustc-link-arg-bin=agent-runtime=-Wl,--export-dynamic-symbol=qq_magic_napi_register"
    );
    println!(
        "cargo:rustc-link-arg-bin=agent-runtime=-Wl,--export-dynamic-symbol=_ZTVN4vips7VOptionE"
    );
}
