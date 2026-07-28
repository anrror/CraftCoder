# Example: Web Search Tool

A CraftCoder custom tool that searches the web using the **DuckDuckGo Instant Answer API** (no API key required).

## Overview

This example demonstrates how to implement a `Tool` trait for network-based operations. The tool queries the DuckDuckGo API and returns formatted search results including an abstract and related topics.

## Input Parameters

| Parameter    | Type    | Required | Default | Description                              |
|-------------|---------|----------|---------|------------------------------------------|
| `query`     | string  | Yes      | —       | The search query to execute              |
| `max_results` | u64  | No       | 5       | Maximum results to return (capped at 10) |

## Output

Formatted text containing:
- Search header
- Abstract (best answer from DuckDuckGo)
- Source URL of the abstract
- Numbered list of related topics with links

## Quick Test

```bash
cargo test -p example-web-search
```

## Integration

Register with the CraftCoder tool registry:

```rust
use std::sync::Arc;
use code_agent_core::tools::registry::{DefaultToolRegistry, ToolRegistry};
use example_web_search::WebSearchTool;

let mut registry = DefaultToolRegistry::new();
registry.register(Arc::new(WebSearchTool::new())).unwrap();
```

## Dependencies

- `code-agent-core` — Tool trait and registry
- `reqwest` — HTTP client for API calls
- `urlencoding` — URL-safe query encoding

## Architecture

```
ToolRegistry
  └── web_search ──→ WebSearchTool.execute()
                          │
                          ├── extract params (require_string, optional_u64)
                          ├── HTTP GET → DuckDuckGo API
                          ├── parse JSON response
                          └── format output → tool_success()
```
