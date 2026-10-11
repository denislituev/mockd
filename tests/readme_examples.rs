//! README documentation tests.
//!
//! Every YAML example in the README must stay parseable and compilable.
//! Fragment examples (`when:`, `response:`, `body:`, a single route item)
//! are wrapped into a full configuration before checking; the elided
//! editor-hint example (`# yaml-language-server: ...` with `# ...`) is
//! skipped.

use mockd::config::Config;
use mockd::server::{Server, ServerError};

#[test]
fn readme_yaml_examples_stay_valid() {
    let readme = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"))
        .expect("README.md is readable");

    let blocks = extract_yaml_blocks(&readme);
    assert!(
        blocks.len() >= 5,
        "expected several yaml examples, got {}",
        blocks.len()
    );

    for (index, block) in blocks.iter().enumerate() {
        if block.trim_start().starts_with("# yaml-language-server") {
            continue; // intentionally elided example
        }
        let config_yaml = wrap_into_config(block);
        let cfg = Config::parse(&config_yaml).unwrap_or_else(|e| {
            panic!("README yaml block #{index} does not parse: {e}\n---\n{config_yaml}")
        });
        Server::from_config(cfg).unwrap_or_else(|e: ServerError| {
            panic!("README yaml block #{index} does not compile: {e}")
        });
    }
}

/// Extract the contents of every ```yaml fenced block.
fn extract_yaml_blocks(markdown: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut rest = markdown;
    while let Some(start) = rest.find("```yaml\n") {
        rest = &rest[start + "```yaml\n".len()..];
        let end = rest.find("```").expect("unterminated yaml block");
        blocks.push(&rest[..end]);
        rest = &rest[end + 3..];
    }
    blocks
}

/// Wrap a README fragment into a full configuration.
fn wrap_into_config(block: &str) -> String {
    let indent = |n: usize| {
        block
            .lines()
            .map(|l| {
                if l.is_empty() {
                    String::new()
                } else {
                    format!("{}{l}", " ".repeat(n))
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    if block.starts_with("listen:") {
        block.to_string()
    } else if block.starts_with("when:") || block.starts_with("response:") {
        format!("routes:\n  - method: GET\n    path: /x\n{}\n", indent(4))
    } else if block.starts_with("- ") {
        format!("routes:\n{block}\n")
    } else if block.starts_with("body:") {
        format!(
            "routes:\n  - method: GET\n    path: /x\n    response:\n      status: 200\n{}\n",
            indent(6)
        )
    } else {
        panic!("unknown README yaml fragment shape: {block}");
    }
}
