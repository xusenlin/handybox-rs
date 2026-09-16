use handybox_core::catalog::{TOOLS, ToolId};
use std::collections::HashSet;

#[test]
fn routes_are_unique_and_only_implemented_tools_are_available() {
    let ready = [
        ToolId::Documents,
        ToolId::Json,
        ToolId::Crypto,
        ToolId::Diff,
        ToolId::Codes,
        ToolId::Clipboard,
        ToolId::Cleanup,
        ToolId::Images,
        ToolId::Archives,
    ];
    let mut keys = HashSet::new();
    for tool in TOOLS {
        assert!(keys.insert(tool.key), "duplicate route {}", tool.key);
        assert_eq!(tool.available, ready.contains(&tool.id), "{}", tool.key);
        assert!(!tool.libraries.is_empty());
    }
    assert_eq!(TOOLS.len(), 9);
}
