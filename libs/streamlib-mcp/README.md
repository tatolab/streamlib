# streamlib-mcp

MCP (Model Context Protocol) server integration for streamlib, enabling AI agents to discover and interact with streaming processors.

## Architecture

The MCP server exposes streamlib's processor registry via two main interfaces:

### Resources (Read-only)
- **List Resources**: Discover all available processors in the registry
- **Read Resource**: Get detailed processor descriptor (schema, ports, capabilities)

Resources use the URI pattern: `processor://ProcessorName`

### Tools (Runtime Actions)
- **add_processor**: Add a processor to the runtime (placeholder)
- **remove_processor**: Remove a processor from the runtime (placeholder)
- **connect_processors**: Connect two processors via ports (placeholder)
- **list_processors**: List currently running processors (placeholder)

## Usage

### Running the MCP Server

Build and run the example MCP server:

```bash
cargo build --example mcp_server --release
./target/release/examples/mcp_server
```

The server communicates via stdio using JSON-RPC protocol.

### Adding to Claude Code

To add this as an MCP server in Claude Code, create or edit your MCP configuration file:

**Location**: `~/.config/claude-code/mcp_config.json` (Linux/Mac) or `%APPDATA%\claude-code\mcp_config.json` (Windows)

```json
{
  "mcpServers": {
    "streamlib": {
      "command": "/absolute/path/to/streamlib/target/release/examples/mcp_server",
      "args": [],
      "env": {}
    }
  }
}
```

Replace `/absolute/path/to/streamlib` with the actual path to your streamlib repository.

### Verification

Once configured, you can verify the MCP server is working by asking Claude Code:

- "What MCP servers are available?"
- "List the streamlib processors"
- "Show me the CameraProcessor descriptor"

## Current Status

### ✅ Implemented
- **Resource Discovery**: List and read processor descriptors
- **Tool Definitions**: JSON schemas for all runtime tools
- **stdio Transport**: JSON-RPC over stdin/stdout
- **Integration**: Works with rmcp 0.8 (official Rust MCP SDK)

### 🚧 In Progress
- **Tool Execution**: Currently returns placeholders
- **Runtime Integration**: Need StreamRuntime for actual processor operations
- **HTTP Transport**: stdio is prioritized (HTTP planned for future)

## Development

### Running Tests

```bash
cargo test -p streamlib-mcp
```

### Example Output

```json
{
  "uri": "processor://CameraProcessor",
  "name": "CameraProcessor",
  "description": "Captures video from camera devices",
  "mimeType": "application/json"
}
```

## Protocol Compliance

This implementation follows the [Model Context Protocol](https://modelcontextprotocol.io/) specification version 2024-11-05.

Key features:
- JSON-RPC 2.0 message format
- Standard resource and tool patterns
- Capability negotiation
- Error handling with proper status codes
