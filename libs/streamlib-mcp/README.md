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

The MCP server supports two transport modes:

#### stdio Transport (Default)

For local AI agents like Claude Desktop:

```bash
# Build
cargo build --example mcp_server --release

# Run with stdio
./target/release/examples/mcp_server
```

The server communicates via stdin/stdout using JSON-RPC protocol.

#### HTTP Transport

For remote AI agents or testing with MCP tools:

```bash
# Run with HTTP on default port 3050
cargo run --example mcp_server -- --http

# Run with custom port
cargo run --example mcp_server -- --http --port 3060

# Run with custom host and port
cargo run --example mcp_server -- --http --host 0.0.0.0 --port 8080
```

**Features**:
- Streamable HTTP transport (not SSE)
- Automatic port selection if requested port is in use
- Access endpoints at `http://host:port/` or `http://host:port/mcp`
- Stateful sessions with resume support

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
- **stdio Transport**: JSON-RPC over stdin/stdout for local AI agents
- **HTTP Transport**: Streamable HTTP for remote AI agents and MCP tools
- **Auto-Registration**: Built-in processors (CameraProcessor, DisplayProcessor) automatically registered via inventory
- **Integration**: Works with rmcp 0.8 (official Rust MCP SDK)

### 🚧 In Progress
- **Tool Execution**: Currently returns placeholders
- **Runtime Integration**: Need StreamRuntime for actual processor operations

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
