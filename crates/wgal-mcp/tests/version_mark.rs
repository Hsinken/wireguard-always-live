//! The built `wgal-mcp` executable, not just its source, carries its
//! version mark: whole, exactly once, with this build's version, as the
//! window reads it (`wgal_protocol::mcp_version_in`). Whatever the
//! optimizer or linker does to the constant, this reads what they made.

use wgal_protocol::{MCP_VERSION_MARK, mcp_version_in};

#[test]
fn the_built_executable_carries_its_version_mark_once() {
    let bytes = std::fs::read(env!("CARGO_BIN_EXE_wgal-mcp")).unwrap();
    let version = env!("CARGO_PKG_VERSION");
    let whole = format!("{MCP_VERSION_MARK}{version}\0");
    let count = |needle: &[u8]| bytes.windows(needle.len()).filter(|w| *w == needle).count();
    assert_eq!(count(whole.as_bytes()), 1, "the whole mark, once");
    assert_eq!(
        count(MCP_VERSION_MARK.as_bytes()),
        1,
        "no other copy of the mark's start"
    );
    assert_eq!(
        mcp_version_in(&bytes).map(|v| v.as_str().to_owned()),
        Some(version.to_owned())
    );
}
