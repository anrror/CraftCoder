# Example: Web Fetch Tool

A CraftCoder custom tool that fetches web page content via HTTP GET.

## Overview

This example demonstrates how to implement a `Tool` trait for content retrieval. The tool performs an HTTP GET request and returns the raw response body as text, truncated to 20,000 characters to prevent context window overflow.

## Input Parameters

| Parameter | Type   | Required | Default | Description                                    |
|-----------|--------|----------|---------|------------------------------------------------|
| `url`     | string | Yes      | —       | The URL to fetch (must start with http/https)  |

## Output

Formatted text containing:
- The requested URL
- HTTP status code
- Response body (truncated to 20,000 characters)

## Quick Test

```bash
cargo test -p example-web-fetch
```

## Integration

Register with the CraftCoder tool registry:

```rust
use std::sync::Arc;
use code_agent_core::tools::registry::{DefaultToolRegistry, ToolRegistry};
use example_web_fetch::WebFetchTool;

let mut registry = DefaultToolRegistry::new();
registry.register(Arc::new(WebFetchTool::new())).unwrap();
```

## Error Handling

The tool handles three categories of errors gracefully:

| Error Type       | Behavior                                              |
|------------------|-------------------------------------------------------|
| Invalid URL      | Returns `ToolError::InvalidInput` (missing `http(s)://`) |
| Network failure  | Returns `ToolError::ExecutionError` with classified message (timeout / connect / generic) |
| HTTP error       | Returns `tool_error()` with status code (200+ OK, others as error result) |

## Safety Notes

- Content is truncated to 20,000 characters to prevent model context overflow
- HTTP client has a 30-second timeout
- Only `http://` and `https://` schemes are accepted

## Dependencies

- `code-agent-core` — Tool trait and registry
- `reqwest` — HTTP client for page fetching

## Architecture

```
ToolRegistry
  └── web_fetch ──→ WebFetchTool.execute()
                        │
                        ├── extract url (require_string)
                        ├── validate URL scheme
                        ├── HTTP GET → remote server
                        ├── classify network errors
                        ├── read response body
                        ├── truncate (max 20K chars)
                        └── format output → tool_success() / tool_error()
```
